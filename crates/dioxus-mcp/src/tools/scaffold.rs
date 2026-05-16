use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use heck::{ToPascalCase, ToSnakeCase};
use minijinja::{Environment, context};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::state::State;

const COMPONENT_TPL: &str = r#"use dioxus::prelude::*;

{%- if has_props %}

#[derive(Props, PartialEq, Clone)]
pub struct {{ pascal }}Props {
{%- for p in props %}
    {%- if p.optional %}
    #[props(default)]
    pub {{ p.name }}: Option<{{ p.ty }}>,
    {%- else %}
    pub {{ p.name }}: {{ p.ty }},
    {%- endif %}
{%- endfor %}
}
{%- endif %}

#[component]
pub fn {{ pascal }}({% if has_props %}props: {{ pascal }}Props{% endif %}) -> Element {
    rsx! {
{{ body }}
    }
}
"#;

/// Body skeletons selectable via `template:`. Indentation is calibrated to slot
/// in two-spaces under the `rsx! {` block in `COMPONENT_TPL`.
const COMPONENT_BODY_EMPTY: &str = r#"        div { class: "{{ snake }}",
            "{{ pascal }} component"
        }"#;

const COMPONENT_BODY_FORM: &str = r#"        form { class: "{{ snake }}",
            onsubmit: move |evt: Event<FormData>| {
                evt.prevent_default();
                // TODO: read form values and submit
            },
            div { class: "field",
                label { "Field" }
                input { r#type: "text", name: "field" }
            }
            button { r#type: "submit", "Submit" }
        }"#;

const COMPONENT_BODY_LIST: &str = r#"        div { class: "{{ snake }}",
            h2 { "{{ pascal }}" }
            // TODO: replace with real items, e.g. `for item in items.iter()`
            ul { class: "{{ snake }}-items",
                li { "Empty list" }
            }
        }"#;

const COMPONENT_BODY_CRUD_TABLE: &str = r#"        div { class: "{{ snake }}",
            div { class: "toolbar",
                button { "New" }
            }
            table { class: "{{ snake }}-table",
                thead {
                    tr {
                        th { "Id" }
                        th { "Name" }
                        th { class: "actions", "Actions" }
                    }
                }
                tbody {
                    // TODO: `for row in rows.iter() { tr { key: "{row.id}", ... } }`
                    tr {
                        td { "—" }
                        td { "No rows" }
                        td {}
                    }
                }
            }
        }"#;

const COMPONENT_BODY_RESOURCE_VIEW: &str = r#"        article { class: "{{ snake }}",
            header {
                h2 { "{{ pascal }}" }
            }
            dl { class: "{{ snake }}-fields",
                dt { "Field" }
                dd { "—" }
            }
            footer { class: "actions",
                button { "Edit" }
                button { class: "danger", "Delete" }
            }
        }"#;

fn component_body_for(template: &str) -> Result<&'static str, String> {
    match template {
        "empty" => Ok(COMPONENT_BODY_EMPTY),
        "form" => Ok(COMPONENT_BODY_FORM),
        "list" => Ok(COMPONENT_BODY_LIST),
        "crud_table" => Ok(COMPONENT_BODY_CRUD_TABLE),
        "resource_view" => Ok(COMPONENT_BODY_RESOURCE_VIEW),
        other => Err(format!(
            "create_component: unknown template {other:?}; valid: empty, form, list, crud_table, resource_view"
        )),
    }
}

const SERVER_FN_TPL: &str = r#"use dioxus::prelude::*;

#[{{ method }}("{{ path }}")]
pub async fn {{ snake }}(
{%- for a in args %}
    {{ a.name }}: {{ a.ty }},
{%- endfor %}
) -> Result<{{ ret }}, ServerFnError> {
    Ok(Default::default())
}
"#;

#[derive(Debug, Serialize, Default)]
pub struct ScaffoldResult {
    pub files_created: Vec<PathBuf>,
    pub files_modified: Vec<PathBuf>,
    pub next_steps: Vec<String>,
    /// Files that already existed at a target path (populated when running
    /// `execute_code` with `if_missing: true` and a re-run skipped a primitive).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collisions: Vec<PathBuf>,
    /// Files that would be created — populated only by `execute_code` in
    /// `dry_run: true` mode.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub would_create: Vec<PathBuf>,
    /// Files that would be modified — populated only by `execute_code` in
    /// `dry_run: true` mode.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub would_modify: Vec<PathBuf>,
    /// True when the result is a dry-run plan rather than an applied change.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dry_run: bool,
    /// High-level outcome of the call. `"no_changes"` when nothing was written
    /// (everything collided under if_missing); `"partial"` when at least one
    /// primitive was skipped but others applied; `"applied"` when the whole
    /// doc landed cleanly. Populated by `execute_code` at the end of the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// File containing the `#[derive(Routable)]` enum where new Screen /
    /// LoginScreen variants will be inserted. Populated by `execute_code` when
    /// the doc declares routes, both for dry_run plans and applied runs.
    /// Useful when the enum lives somewhere other than `src/router.rs` (e.g.
    /// inlined in `src/main.rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routable_file: Option<PathBuf>,
    /// Generated file contents keyed by would-be path. Populated by
    /// `execute_code` in `dry_run: true` mode so the agent can preview what
    /// a template emits without committing. Currently scoped to Screen bodies
    /// (the main case where agents bypass the primitive because they can't
    /// predict the output); other primitives stay path-only.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub previews: std::collections::BTreeMap<PathBuf, String>,
}

// ---------- create_component ----------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PropSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct CreateComponentParams {
    /// Component name in any case; will be normalized to PascalCase / snake_case.
    pub name: String,
    #[serde(default)]
    pub props: Vec<PropSpec>,
    /// Optional override directory (relative to crate root). Defaults to `src/components`.
    pub path: Option<String>,
    /// Stub-body skeleton. One of: `empty` (default — single placeholder div),
    /// `form` (form with submit handler), `list` (ul with empty-state),
    /// `crud_table` (table with header + toolbar), `resource_view` (article
    /// with field list + edit/delete actions). Templates are structural only —
    /// they do not wire to any data source; pair with `props:` or hand-edit
    /// after generation.
    #[serde(default)]
    pub template: Option<String>,
    /// Absolute path to the Dioxus project root. Required when the MCP server was not
    /// started in the target project directory.
    pub project_root: Option<String>,
}

pub async fn create_component(
    state: &Arc<State>,
    p: CreateComponentParams,
) -> Result<ScaffoldResult, String> {
    let crate_root = crate_root(state, p.project_root.as_deref()).await?;
    let components_dir = crate_root.join(p.path.as_deref().unwrap_or("src/components"));
    std::fs::create_dir_all(&components_dir).map_err(|e| e.to_string())?;

    let pascal = p.name.to_pascal_case();
    let snake = p.name.to_snake_case();
    let target = components_dir.join(format!("{snake}.rs"));
    if target.exists() {
        return Err(format!("{} already exists", target.display()));
    }

    let template_kind = p.template.as_deref().unwrap_or("empty");
    let body_tpl = component_body_for(template_kind)?;

    let mut env = Environment::new();
    env.add_template("component_body", body_tpl).unwrap();
    let body = env
        .get_template("component_body")
        .unwrap()
        .render(context! {
            pascal => pascal.clone(),
            snake => snake.clone(),
        })
        .map_err(|e| e.to_string())?;

    env.add_template("component", COMPONENT_TPL).unwrap();
    let tpl = env.get_template("component").unwrap();
    let rendered = tpl
        .render(context! {
            pascal => pascal.clone(),
            snake => snake.clone(),
            has_props => !p.props.is_empty(),
            props => p.props.iter().map(|p| context!{ name => p.name.clone(), ty => p.ty.clone(), optional => p.optional }).collect::<Vec<_>>(),
            body => body,
        })
        .map_err(|e| e.to_string())?;
    std::fs::write(&target, rendered).map_err(|e| e.to_string())?;

    // ensure mod.rs exports it
    let mod_rs = components_dir.join("mod.rs");
    let upsert = upsert_mod_entry(&mod_rs, &snake, None, false)?;
    let (files_created, files_modified) = match upsert {
        ModUpsert::Created => (vec![target, mod_rs], vec![]),
        ModUpsert::Modified => (vec![target], vec![mod_rs]),
        ModUpsert::Unchanged => (vec![target], vec![]),
    };

    let next_steps = vec![
        format!("`use crate::components::{pascal};` where you want to render it"),
        "wire `mod components;` into your crate root if it isn't already".into(),
    ];

    Ok(ScaffoldResult {
        files_created,
        files_modified,
        next_steps,
        ..Default::default()
    })
}

// ---------- create_route ----------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct CreateRouteParams {
    /// Route path, e.g. "/users/:id".
    pub path: String,
    /// Component name to render.
    pub component: String,
    /// File containing the `#[derive(Routable)]` enum. Defaults to `src/router.rs` then `src/main.rs`.
    pub router_file: Option<String>,
    /// Absolute path to the Dioxus project root. Required when the MCP server was not
    /// started in the target project directory.
    pub project_root: Option<String>,
    /// Optional path-param fields for the variant. Each entry is `(name, type)`
    /// and lands as `Variant { name: type }` so Dioxus's Routable derive can
    /// extract the value from the URL. Omit for variants with no path params.
    #[serde(default)]
    pub params: Vec<(String, String)>,
}

pub async fn create_route(
    state: &Arc<State>,
    p: CreateRouteParams,
) -> Result<ScaffoldResult, String> {
    let crate_root = crate_root(state, p.project_root.as_deref()).await?;
    let router_file = match p.router_file.as_deref() {
        Some(rf) => crate_root.join(rf),
        None => find_routable(&crate_root).ok_or_else(|| {
            "could not find a Routable enum in src/; pass router_file".to_string()
        })?,
    };

    let src = std::fs::read_to_string(&router_file)
        .map_err(|e| format!("read {}: {e}", router_file.display()))?;
    let variant_name = p.component.to_pascal_case();
    let mut next_steps = vec![
        format!("ensure `{variant_name}` exists and is in scope at the routable enum"),
        "consider running `cargo fmt` on the router file".into(),
    ];
    match plan_route_insertion(&src, &variant_name, &p.path, &p.params)? {
        RouteInsertion::AlreadyMatches => Ok(ScaffoldResult {
            next_steps,
            ..Default::default()
        }),
        RouteInsertion::Insert { new_src, line } => {
            std::fs::write(&router_file, &new_src).map_err(|e| e.to_string())?;
            let rel = router_file
                .strip_prefix(&crate_root)
                .unwrap_or(&router_file)
                .display();
            next_steps.insert(
                0,
                format!("inserted `{variant_name}` route variant at `{rel}:{line}`"),
            );
            Ok(ScaffoldResult {
                files_modified: vec![router_file.clone()],
                next_steps,
                ..Default::default()
            })
        }
    }
}

#[cfg_attr(test, derive(Debug))]
enum RouteInsertion {
    /// The variant already exists and points at the same path. No-op.
    AlreadyMatches,
    /// Variant doesn't exist; this is the new source with the variant inserted.
    /// `line` is the 1-based line number of the inserted `#[route(...)]`
    /// attribute in `new_src` — surfaced in `next_steps` so callers can jump
    /// straight to the new variant in the routable enum.
    Insert { new_src: String, line: usize },
}

/// Inspect `src` for a `#[derive(Routable)]` enum and decide what to do for
/// `(variant_name, path)`:
/// - If a variant with the same name already maps to the same path → no-op.
/// - If a variant with the same name maps to a different path → conflict error.
/// - Otherwise → return the source with the variant inserted before the enum's
///   closing brace.
fn plan_route_insertion(
    src: &str,
    variant_name: &str,
    path: &str,
    params: &[(String, String)],
) -> Result<RouteInsertion, String> {
    let file = syn::parse_file(src).map_err(|e| format!("parse: {e}"))?;
    let routable = file
        .items
        .iter()
        .find_map(|it| match it {
            syn::Item::Enum(e) if e.attrs.iter().any(|a| has_derive(a, "Routable")) => Some(e),
            _ => None,
        })
        .ok_or_else(|| "no `#[derive(Routable)]` enum in source".to_string())?;
    let enum_name = routable.ident.to_string();

    for v in &routable.variants {
        if v.ident == variant_name {
            let existing_path = variant_route_path(v);
            return match existing_path {
                Some(p) if p == path => Ok(RouteInsertion::AlreadyMatches),
                Some(p) => Err(format!(
                    "route conflict: variant {variant_name} already maps to {p:?}, not {path:?}"
                )),
                None => Err(format!(
                    "variant {variant_name} already exists in {enum_name} but has no #[route(\"...\")] attribute"
                )),
            };
        }
        // Same path under a different variant name — Dioxus's Routable would
        // route the first match and silently shadow the second. Surface it
        // here so the user picks one.
        if let Some(p) = variant_route_path(v)
            && p == path
        {
            return Err(format!(
                "route conflict: path {path:?} is already mapped by variant {} in {enum_name}; \
                 rename one or change the path before re-running",
                v.ident
            ));
        }
    }

    let fields = if params.is_empty() {
        String::new()
    } else {
        let inner = params
            .iter()
            .map(|(n, t)| format!("{n}: {t}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(" {inner} ")
    };
    let variant = format!("    #[route(\"{path}\")]\n    {variant_name} {{{fields}}},\n");
    let needle_open = format!("enum {enum_name}");
    let Some(start) = src.find(&needle_open) else {
        return Err(format!("could not locate `enum {enum_name}` in source"));
    };
    let after_open = src[start..]
        .find('{')
        .map(|i| start + i + 1)
        .ok_or_else(|| "malformed enum".to_string())?;
    let mut depth = 1;
    let mut end = after_open;
    for (i, ch) in src[after_open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = after_open + i;
                    break;
                }
            }
            _ => {}
        }
    }
    let mut new_src = String::with_capacity(src.len() + variant.len());
    new_src.push_str(&src[..end]);
    if !src[..end].ends_with('\n') {
        new_src.push('\n');
    }
    // 1-based line where the inserted `#[route(...)]` attribute now lives in
    // `new_src`. Counting newlines in the prefix and adding 1 gives the line
    // number of the first character we're about to append.
    let line = new_src.bytes().filter(|&b| b == b'\n').count() + 1;
    new_src.push_str(&variant);
    new_src.push_str(&src[end..]);
    Ok(RouteInsertion::Insert { new_src, line })
}

/// Extract `(variant_name, path)` pairs from every variant in the
/// `#[derive(Routable)]` enum in `router_src`. Returns an empty list when the
/// file has no Routable enum (or fails to parse) — callers should treat the
/// missing-enum case the same as "no existing routes."
pub(crate) fn existing_route_paths(router_src: &str) -> Vec<(String, String)> {
    let Ok(file) = syn::parse_file(router_src) else {
        return Vec::new();
    };
    let Some(routable) = file.items.iter().find_map(|it| match it {
        syn::Item::Enum(e) if e.attrs.iter().any(|a| has_derive(a, "Routable")) => Some(e),
        _ => None,
    }) else {
        return Vec::new();
    };
    routable
        .variants
        .iter()
        .filter_map(|v| variant_route_path(v).map(|p| (v.ident.to_string(), p)))
        .collect()
}

pub(crate) fn variant_route_path(v: &syn::Variant) -> Option<String> {
    for a in &v.attrs {
        if !a.path().is_ident("route") {
            continue;
        }
        if let Ok(lit) = a.parse_args::<syn::LitStr>() {
            return Some(lit.value());
        }
    }
    None
}

pub(crate) fn has_derive(attr: &syn::Attribute, target: &str) -> bool {
    if !attr.path().is_ident("derive") {
        return false;
    }
    let mut found = false;
    let _ = attr.parse_nested_meta(|m| {
        if m.path.is_ident(target) {
            found = true;
        }
        Ok(())
    });
    found
}

/// Find the crate root .rs (src/main.rs preferred, then src/lib.rs).
pub(crate) fn find_crate_root_file(crate_root: &Path) -> Option<PathBuf> {
    for cand in &["src/main.rs", "src/lib.rs"] {
        let p = crate_root.join(cand);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Idempotently declare `pub mod {module};` in the crate root (src/main.rs or
/// src/lib.rs). Returns `Ok(Some(path))` if the file was modified, `Ok(None)`
/// if the declaration was already present, and `Ok(None)` if no crate root
/// could be located (silent no-op — callers fall back to a `next_steps` hint).
///
/// Insertion point: after the last existing `mod`/`pub mod` line if any, then
/// after the last `use` line, else at the top after inner attributes.
pub(crate) fn upsert_crate_mod(crate_root: &Path, module: &str) -> Result<Option<PathBuf>, String> {
    let Some(path) = find_crate_root_file(crate_root) else {
        return Ok(None);
    };
    let current = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;

    // Already declared (either `pub mod foo;` or `mod foo;`, with any leading
    // whitespace / attributes)?
    let needle_pub = format!("pub mod {module};");
    let needle_priv = format!("mod {module};");
    for raw in current.lines() {
        let t = raw.trim();
        if t == needle_pub || t == needle_priv {
            return Ok(None);
        }
    }

    let lines: Vec<&str> = current.lines().collect();
    // Find insertion line: after last `mod`/`pub mod` line, else after last
    // `use` line, else after the leading attribute/comment block.
    let mut insert_at = None;
    for (i, raw) in lines.iter().enumerate() {
        let t = raw.trim_start();
        if t.starts_with("mod ") || t.starts_with("pub mod ") {
            insert_at = Some(i + 1);
        }
    }
    if insert_at.is_none() {
        for (i, raw) in lines.iter().enumerate() {
            let t = raw.trim_start();
            if t.starts_with("use ") || t.starts_with("pub use ") {
                insert_at = Some(i + 1);
            }
        }
    }
    let insert_at = insert_at.unwrap_or_else(|| {
        // Skip a leading shebang, then any contiguous block of inner
        // attributes / doc comments / blank lines.
        let mut i = 0;
        if i < lines.len() && lines[i].starts_with("#!") && !lines[i].starts_with("#![") {
            i += 1;
        }
        while i < lines.len() {
            let t = lines[i].trim();
            if t.is_empty() || t.starts_with("#![") || t.starts_with("//!") || t.starts_with("//") {
                i += 1;
            } else {
                break;
            }
        }
        i
    });

    let mut new_lines: Vec<String> = lines.iter().map(|s| (*s).to_string()).collect();
    new_lines.insert(insert_at, format!("pub mod {module};"));
    let mut rebuilt = new_lines.join("\n");
    // Preserve trailing newline if the original had one.
    if current.ends_with('\n') && !rebuilt.ends_with('\n') {
        rebuilt.push('\n');
    }

    if rebuilt == current {
        return Ok(None);
    }
    std::fs::write(&path, rebuilt).map_err(|e| e.to_string())?;
    Ok(Some(path))
}

pub(crate) fn find_routable(crate_root: &Path) -> Option<PathBuf> {
    for cand in &["src/router.rs", "src/route.rs", "src/main.rs", "src/lib.rs"] {
        let p = crate_root.join(cand);
        if let Ok(s) = std::fs::read_to_string(&p)
            && s.contains("Routable")
        {
            return Some(p);
        }
    }
    // fall back: walk src/
    for entry in walkdir::WalkDir::new(crate_root.join("src"))
        .into_iter()
        .flatten()
    {
        if entry.path().extension().and_then(|x| x.to_str()) != Some("rs") {
            continue;
        }
        if let Ok(s) = std::fs::read_to_string(entry.path())
            && (s.contains("#[derive(Routable") || s.contains("derive(Routable"))
        {
            return Some(entry.path().to_path_buf());
        }
    }
    None
}

/// The server-fn template already wraps the return type in
/// `Result<_, ServerFnError>`. Reject callers that pre-wrap it themselves —
/// the resulting `Result<Result<_, ServerFnError>, ServerFnError>` compiles but
/// is silently wrong. Returns `Some(error_message)` if the caller's type is
/// already a server-fn result wrapper, else `None`.
pub(crate) fn check_inner_return_type(ret: &str) -> Option<String> {
    let parsed: syn::Type = match syn::parse_str(ret) {
        Ok(t) => t,
        // If it doesn't parse, leave validation to the compiler — we don't want
        // to swallow surprising errors here.
        Err(_) => return None,
    };
    let path = match &parsed {
        syn::Type::Path(tp) => &tp.path,
        _ => return None,
    };
    let last = path.segments.last()?;
    let ident = last.ident.to_string();
    let args = match &last.arguments {
        syn::PathArguments::AngleBracketed(a) => a,
        _ => return None,
    };
    let type_args: Vec<&syn::Type> = args
        .args
        .iter()
        .filter_map(|a| match a {
            syn::GenericArgument::Type(t) => Some(t),
            _ => None,
        })
        .collect();
    match (ident.as_str(), type_args.as_slice()) {
        ("ServerFnResult", [inner]) => {
            let inner_str = quote::quote!(#inner).to_string();
            Some(format!(
                "return_type `{ret}` already wraps the inner type in ServerFnResult; pass just `{inner_str}` instead — the template wraps it in `Result<_, ServerFnError>` for you."
            ))
        }
        ("Result", [inner, err]) => {
            let err_str = quote::quote!(#err).to_string();
            let err_norm = err_str.replace(' ', "");
            if err_norm.ends_with("ServerFnError") {
                let inner_str = quote::quote!(#inner).to_string();
                Some(format!(
                    "return_type `{ret}` already wraps the inner type in `Result<_, ServerFnError>`; pass just `{inner_str}` instead — the template wraps it for you."
                ))
            } else {
                None
            }
        }
        _ => None,
    }
}

// ---------- create_server_fn ----------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ArgSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct CreateServerFnParams {
    pub name: String,
    #[serde(default)]
    pub args: Vec<ArgSpec>,
    /// Defaults to `String`.
    pub return_type: Option<String>,
    /// HTTP method: "get" or "post". Defaults to "post" when args is non-empty,
    /// "get" otherwise.
    pub method: Option<String>,
    /// Route path under which the server fn is exposed. Defaults to
    /// "/api/{snake_name}".
    pub path: Option<String>,
    /// Absolute path to the Dioxus project root. Required when the MCP server was not
    /// started in the target project directory.
    pub project_root: Option<String>,
}

pub async fn create_server_fn(
    state: &Arc<State>,
    p: CreateServerFnParams,
) -> Result<ScaffoldResult, String> {
    let project = match p.project_root.as_deref() {
        Some(root) => crate::project::ProjectInfo::detect(std::path::Path::new(root)),
        None => state.project.lock().await.clone(),
    };
    let active = &project.dioxus_features;
    let fullstack_capable = active.iter().any(|f| f == "fullstack")
        || (active.iter().any(|f| f == "server") && active.iter().any(|f| f == "web"));
    if !fullstack_capable {
        return Err(
            "this project does not have `fullstack` (or `web`+`server`) enabled on the dioxus dep; \
             server fns require a fullstack setup. Run audit_feature_flags for guidance."
                .into(),
        );
    }

    let crate_root = project
        .manifest_dir()
        .ok_or_else(|| "no manifest dir".to_string())?;
    let server_dir = crate_root.join("src/server");
    std::fs::create_dir_all(&server_dir).map_err(|e| e.to_string())?;

    let snake = p.name.to_snake_case();
    let ret = p.return_type.unwrap_or_else(|| "String".into());
    if let Some(reason) = check_inner_return_type(&ret) {
        return Err(reason);
    }
    let method = match p.method.as_deref().map(str::to_ascii_lowercase) {
        Some(m) => {
            if !matches!(m.as_str(), "get" | "post") {
                return Err(format!("method must be \"get\" or \"post\", got {m:?}"));
            }
            m
        }
        None => {
            if p.args.is_empty() {
                "get".to_string()
            } else {
                "post".to_string()
            }
        }
    };
    let route_path = p.path.clone().unwrap_or_else(|| format!("/api/{snake}"));
    let target = server_dir.join(format!("{snake}.rs"));
    if target.exists() {
        return Err(format!("{} already exists", target.display()));
    }

    let mut env = Environment::new();
    env.add_template("server_fn", SERVER_FN_TPL).unwrap();
    let tpl = env.get_template("server_fn").unwrap();
    let rendered = tpl
        .render(context! {
            snake => snake.clone(),
            ret => ret.clone(),
            method => method,
            path => route_path,
            args => p.args.iter().map(|a| context!{ name => a.name.clone(), ty => a.ty.clone() }).collect::<Vec<_>>(),
        })
        .map_err(|e| e.to_string())?;
    std::fs::write(&target, rendered).map_err(|e| e.to_string())?;

    // ensure src/server/mod.rs declares it
    let mod_rs = server_dir.join("mod.rs");
    let upsert = upsert_mod_entry(&mod_rs, &snake, None, true)?;
    let (files_created, files_modified) = match upsert {
        ModUpsert::Created => (vec![target, mod_rs], vec![]),
        ModUpsert::Modified => (vec![target], vec![mod_rs]),
        ModUpsert::Unchanged => (vec![target], vec![]),
    };

    let next_steps = vec![
        format!("call `{snake}(...)` from a client component to invoke it"),
        "ensure `mod server;` is declared in your crate root".into(),
    ];

    Ok(ScaffoldResult {
        files_created,
        files_modified,
        next_steps,
        ..Default::default()
    })
}

/// Result of upserting an entry into a `mod.rs` file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModUpsert {
    /// The file was created from scratch.
    Created,
    /// The file existed and we added the entry (or re-sorted).
    Modified,
    /// The file already declared this module — no write.
    Unchanged,
}

/// Insert `pub mod {name}; pub use {name}::*;` into `mod_rs`, keeping all
/// `pub mod` / `pub use` entries sorted by name. Any non-entry lines (comments,
/// hand-written re-exports, etc.) are preserved verbatim at the top of the file.
///
/// When `allow_unused` is true, the file (whether newly created or being
/// rewritten) carries an `#![allow(unused_imports)]` shield: the blanket
/// `pub use foo::*;` re-export pattern routinely flags as `unused_imports`
/// when one of the synthesized items (e.g. a delete_* server fn) isn't called
/// by anything yet. Set to false for `src/components/mod.rs` where every
/// re-export is a real component the user will reference by name.
///
/// When `cfg_attr` is `Some(attr)`, each emitted `pub mod` / `pub use` line is
/// prefixed with that attribute on its own line — used for `src/state/mod.rs`
/// because store files are themselves `#![cfg(feature = "server")]` and the
/// module declarations need the same gate to not break the wasm build.
pub(crate) fn upsert_mod_entry(
    mod_rs: &Path,
    name: &str,
    cfg_attr: Option<&str>,
    allow_unused: bool,
) -> Result<ModUpsert, String> {
    if !mod_rs.exists() {
        let mut body = String::new();
        if allow_unused {
            body.push_str("#![allow(unused_imports)]\n");
        }
        for line in [format!("pub mod {name};"), format!("pub use {name}::*;")] {
            if let Some(cfg) = cfg_attr {
                body.push_str(cfg);
                body.push('\n');
            }
            body.push_str(&line);
            body.push('\n');
        }
        std::fs::write(mod_rs, body).map_err(|e| e.to_string())?;
        return Ok(ModUpsert::Created);
    }

    let current = std::fs::read_to_string(mod_rs).map_err(|e| e.to_string())?;
    let mut header: Vec<String> = Vec::new();
    let mut entries: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut header_done = false;
    let mut had_allow_unused = false;
    for raw in current.lines() {
        let line = raw.trim();
        if line == "#![allow(unused_imports)]" {
            had_allow_unused = true;
            continue;
        }
        // Drop outer cfg attributes — we re-emit them uniformly from cfg_attr.
        if line.starts_with("#[cfg(") {
            header_done = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix("pub mod ")
            && let Some(n) = rest.strip_suffix(';')
        {
            header_done = true;
            entries.entry(n.to_string()).or_default().push(raw.into());
            continue;
        }
        if let Some(rest) = line.strip_prefix("pub use ")
            && let Some(n) = rest.strip_suffix("::*;")
        {
            header_done = true;
            entries.entry(n.to_string()).or_default().push(raw.into());
            continue;
        }
        if !header_done {
            header.push(raw.into());
        }
        // Any non-entry line *after* entries started is dropped — we don't want
        // to scatter free-form comments through a sorted block. If a user has
        // such comments they should sit above the first entry.
    }

    entries
        .entry(name.to_string())
        .or_insert_with(|| vec![format!("pub mod {name};"), format!("pub use {name}::*;")]);

    let mut rebuilt = String::new();
    // The caller's `allow_unused` is authoritative: pass true to add the
    // attribute (or keep it if already there), pass false to drop it. This
    // lets callers (e.g. components/mod.rs) clean up the attribute from
    // previously-generated files on the next scaffold write.
    let _ = had_allow_unused;
    if allow_unused {
        rebuilt.push_str("#![allow(unused_imports)]\n");
    }
    for h in &header {
        rebuilt.push_str(h);
        rebuilt.push('\n');
    }
    for lines in entries.values() {
        for l in lines {
            if let Some(cfg) = cfg_attr {
                rebuilt.push_str(cfg);
                rebuilt.push('\n');
            }
            rebuilt.push_str(l);
            rebuilt.push('\n');
        }
    }

    if rebuilt == current {
        return Ok(ModUpsert::Unchanged);
    }
    std::fs::write(mod_rs, rebuilt).map_err(|e| e.to_string())?;
    Ok(ModUpsert::Modified)
}

#[cfg(test)]
mod plan_route_tests {
    use super::{RouteInsertion, plan_route_insertion};

    const BASE: &str = r#"use dioxus::prelude::*;

#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[route("/")]
    Home {},
    #[route("/users/:id")]
    User { id: i32 },
}
"#;

    #[test]
    fn inserts_new_variant() {
        let r = plan_route_insertion(BASE, "About", "/about", &[]).unwrap();
        match r {
            RouteInsertion::Insert { new_src, line } => {
                assert!(new_src.contains("#[route(\"/about\")]"));
                assert!(new_src.contains("About {}"));
                assert!(new_src.contains("Home {}"));
                assert!(new_src.contains("User { id: i32 }"));
                // The reported line should point at the `#[route("/about")]`
                // attribute of the inserted variant.
                let lines: Vec<&str> = new_src.lines().collect();
                assert_eq!(
                    lines.get(line - 1).copied(),
                    Some("    #[route(\"/about\")]"),
                    "line {line} should be the inserted #[route(...)], got: {:?}",
                    lines.get(line - 1)
                );
            }
            _ => panic!("expected Insert"),
        }
    }

    #[test]
    fn inserts_new_variant_with_params() {
        let r = plan_route_insertion(
            BASE,
            "EditUser",
            "/users/:id/edit",
            &[("id".into(), "i64".into())],
        )
        .unwrap();
        match r {
            RouteInsertion::Insert { new_src, .. } => {
                assert!(new_src.contains("#[route(\"/users/:id/edit\")]"));
                assert!(
                    new_src.contains("EditUser { id: i64 }"),
                    "expected variant with id field, got:\n{new_src}"
                );
            }
            _ => panic!("expected Insert"),
        }
    }

    #[test]
    fn skips_existing_variant_same_path() {
        let r = plan_route_insertion(BASE, "Home", "/", &[]).unwrap();
        assert!(matches!(r, RouteInsertion::AlreadyMatches));
    }

    #[test]
    fn errors_on_existing_variant_different_path() {
        let err = plan_route_insertion(BASE, "Home", "/landing", &[]).unwrap_err();
        assert!(err.contains("route conflict"), "got: {err}");
        assert!(err.contains("Home"));
    }

    #[test]
    fn errors_without_routable_enum() {
        let src = "pub enum NotRoutable { Foo, Bar }";
        let err = plan_route_insertion(src, "Foo", "/foo", &[]).unwrap_err();
        assert!(err.contains("Routable"));
    }

    #[test]
    fn errors_on_path_collision_with_different_variant() {
        // A new variant `Landing` at `/` collides with the existing
        // `Home {}` at `/`. The variant name is fresh so a name-only check
        // wouldn't catch it — the path-collision check should.
        let err = plan_route_insertion(BASE, "Landing", "/", &[]).unwrap_err();
        assert!(err.contains("route conflict"), "got: {err}");
        assert!(
            err.contains("Home"),
            "should name the colliding variant, got: {err}"
        );
        assert!(
            err.contains("\"/\""),
            "should quote the colliding path, got: {err}"
        );
    }
}

#[cfg(test)]
mod return_type_tests {
    use super::check_inner_return_type;

    #[test]
    fn accepts_bare_types() {
        assert!(check_inner_return_type("String").is_none());
        assert!(check_inner_return_type("Vec<Product>").is_none());
        assert!(check_inner_return_type("Option<i64>").is_none());
        assert!(check_inner_return_type("crate::model::Product").is_none());
    }

    #[test]
    fn rejects_result_serverfnerror() {
        let e = check_inner_return_type("Result<Vec<String>, ServerFnError>").unwrap();
        assert!(e.contains("already wraps"));
        assert!(e.contains("Vec < String >") || e.contains("Vec<String>"));
    }

    #[test]
    fn rejects_serverfnresult() {
        let e = check_inner_return_type("ServerFnResult<Vec<String>>").unwrap();
        assert!(e.contains("ServerFnResult"));
    }

    #[test]
    fn rejects_qualified_serverfnerror() {
        let e =
            check_inner_return_type("Result<Vec<String>, dioxus::prelude::ServerFnError>").unwrap();
        assert!(e.contains("already wraps"));
    }

    #[test]
    fn accepts_result_with_different_error() {
        assert!(check_inner_return_type("Result<Vec<String>, MyError>").is_none());
    }
}

#[cfg(test)]
mod mod_upsert_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn creates_when_missing() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("mod.rs");
        let r = upsert_mod_entry(&p, "foo", None, true).unwrap();
        assert_eq!(r, ModUpsert::Created);
        let body = std::fs::read_to_string(&p).unwrap();
        // With `allow_unused: true`, freshly-created mod.rs files carry an
        // `#![allow(unused_imports)]` shield so that wildcard re-exports of
        // as-yet-uncalled items (e.g. delete_* server fns generated alongside
        // their list/get siblings) don't trip `cargo check` warnings while
        // iterating.
        assert_eq!(
            body,
            "#![allow(unused_imports)]\npub mod foo;\npub use foo::*;\n"
        );
    }

    #[test]
    fn creates_without_allow_unused() {
        // `src/components/mod.rs` passes `allow_unused: false` because every
        // re-exported item is a real component the user will reference by
        // name — no wildcard footgun to shield against.
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("mod.rs");
        let r = upsert_mod_entry(&p, "foo", None, false).unwrap();
        assert_eq!(r, ModUpsert::Created);
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(body, "pub mod foo;\npub use foo::*;\n");
    }

    #[test]
    fn strips_existing_allow_unused_when_disabled() {
        // If a previously-generated mod.rs carries the attribute but the
        // current caller passes `allow_unused: false`, we clean it up — the
        // caller's directive is authoritative.
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("mod.rs");
        std::fs::write(
            &p,
            "#![allow(unused_imports)]\npub mod alpha;\npub use alpha::*;\n",
        )
        .unwrap();
        let r = upsert_mod_entry(&p, "beta", None, false).unwrap();
        assert_eq!(r, ModUpsert::Modified);
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(
            body,
            "pub mod alpha;\npub use alpha::*;\npub mod beta;\npub use beta::*;\n"
        );
    }

    #[test]
    fn inserts_sorted_into_existing() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("mod.rs");
        std::fs::write(
            &p,
            "pub mod alpha;\npub use alpha::*;\npub mod zeta;\npub use zeta::*;\n",
        )
        .unwrap();
        let r = upsert_mod_entry(&p, "mid", None, false).unwrap();
        assert_eq!(r, ModUpsert::Modified);
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(
            body,
            "pub mod alpha;\npub use alpha::*;\npub mod mid;\npub use mid::*;\npub mod zeta;\npub use zeta::*;\n"
        );
    }

    #[test]
    fn resorts_an_out_of_order_file() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("mod.rs");
        std::fs::write(
            &p,
            "pub mod zeta;\npub use zeta::*;\npub mod alpha;\npub use alpha::*;\n",
        )
        .unwrap();
        let r = upsert_mod_entry(&p, "alpha", None, false).unwrap();
        assert_eq!(r, ModUpsert::Modified);
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(
            body,
            "pub mod alpha;\npub use alpha::*;\npub mod zeta;\npub use zeta::*;\n"
        );
    }

    #[test]
    fn idempotent_when_already_present_and_sorted() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("mod.rs");
        let initial = "pub mod alpha;\npub use alpha::*;\npub mod beta;\npub use beta::*;\n";
        std::fs::write(&p, initial).unwrap();
        let r = upsert_mod_entry(&p, "alpha", None, false).unwrap();
        assert_eq!(r, ModUpsert::Unchanged);
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(body, initial);
    }

    #[test]
    fn preserves_header_comments() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("mod.rs");
        std::fs::write(
            &p,
            "// hand-written header\n//! crate doc\npub mod zeta;\npub use zeta::*;\n",
        )
        .unwrap();
        let r = upsert_mod_entry(&p, "alpha", None, false).unwrap();
        assert_eq!(r, ModUpsert::Modified);
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(
            body,
            "// hand-written header\n//! crate doc\npub mod alpha;\npub use alpha::*;\npub mod zeta;\npub use zeta::*;\n"
        );
    }

    #[test]
    fn cfg_attr_emitted_for_fresh_file() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("mod.rs");
        let r = upsert_mod_entry(
            &p,
            "product_store",
            Some("#[cfg(feature = \"server\")]"),
            true,
        )
        .unwrap();
        assert_eq!(r, ModUpsert::Created);
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(
            body,
            "#![allow(unused_imports)]\n\
             #[cfg(feature = \"server\")]\npub mod product_store;\n\
             #[cfg(feature = \"server\")]\npub use product_store::*;\n"
        );
    }

    #[test]
    fn cfg_attr_added_to_existing_entries() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("mod.rs");
        std::fs::write(
            &p,
            "#![allow(unused_imports)]\n\
             #[cfg(feature = \"server\")]\npub mod alpha;\n\
             #[cfg(feature = \"server\")]\npub use alpha::*;\n",
        )
        .unwrap();
        let r = upsert_mod_entry(&p, "beta", Some("#[cfg(feature = \"server\")]"), true).unwrap();
        assert_eq!(r, ModUpsert::Modified);
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(
            body,
            "#![allow(unused_imports)]\n\
             #[cfg(feature = \"server\")]\npub mod alpha;\n\
             #[cfg(feature = \"server\")]\npub use alpha::*;\n\
             #[cfg(feature = \"server\")]\npub mod beta;\n\
             #[cfg(feature = \"server\")]\npub use beta::*;\n"
        );
    }
}

#[cfg(test)]
mod component_template_tests {
    use super::*;

    fn render(template: &str) -> String {
        let body_tpl = component_body_for(template).expect("known template");
        let mut env = Environment::new();
        env.add_template("body", body_tpl).unwrap();
        let body = env
            .get_template("body")
            .unwrap()
            .render(context! {
                pascal => "ProductTable".to_string(),
                snake => "product_table".to_string(),
            })
            .unwrap();

        env.add_template("c", COMPONENT_TPL).unwrap();
        env.get_template("c")
            .unwrap()
            .render(context! {
                pascal => "ProductTable".to_string(),
                snake => "product_table".to_string(),
                has_props => false,
                props => Vec::<()>::new(),
                body => body,
            })
            .unwrap()
    }

    #[test]
    fn empty_template_matches_legacy_body() {
        let s = render("empty");
        assert!(s.contains(r#"div { class: "product_table","#));
        assert!(s.contains(r#""ProductTable component""#));
    }

    #[test]
    fn form_template_emits_form_with_submit_handler() {
        let s = render("form");
        assert!(s.contains("form { class: \"product_table\""));
        assert!(s.contains("onsubmit:"));
        assert!(s.contains("button { r#type: \"submit\""));
    }

    #[test]
    fn list_template_emits_ul_with_empty_state() {
        let s = render("list");
        assert!(s.contains("ul { class: \"product_table-items\""));
        assert!(s.contains("Empty list"));
    }

    #[test]
    fn crud_table_template_emits_table_skeleton() {
        let s = render("crud_table");
        assert!(s.contains("table { class: \"product_table-table\""));
        assert!(s.contains("thead {") && s.contains("tbody {"));
        assert!(s.contains("button { \"New\""));
    }

    #[test]
    fn resource_view_template_emits_article_with_actions() {
        let s = render("resource_view");
        assert!(s.contains("article { class: \"product_table\""));
        assert!(s.contains("dl { class: \"product_table-fields\""));
        assert!(s.contains("button { class: \"danger\", \"Delete\""));
    }

    #[test]
    fn unknown_template_is_rejected_with_helpful_message() {
        let err = component_body_for("dropdown").unwrap_err();
        assert!(err.contains("unknown template"));
        assert!(err.contains("crud_table"));
    }
}

pub(crate) async fn crate_root(
    state: &Arc<State>,
    project_root: Option<&str>,
) -> Result<PathBuf, String> {
    match project_root {
        Some(root) => {
            let info = crate::project::ProjectInfo::detect(std::path::Path::new(root));
            info.manifest_dir()
                .ok_or_else(|| format!("no Cargo.toml with a dioxus dep found under {root}"))
        }
        None => {
            let project = state.project.lock().await;
            project
                .manifest_dir()
                .ok_or_else(|| "no Cargo.toml found from project root".to_string())
        }
    }
}
