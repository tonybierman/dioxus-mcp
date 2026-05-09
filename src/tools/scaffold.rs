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
        div { class: "{{ snake }}",
            "{{ pascal }} component"
        }
    }
}
"#;

const SERVER_FN_TPL: &str = r#"use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct {{ pascal }}Result {
    pub ok: bool,
}

#[server({{ pascal }})]
pub async fn {{ snake }}(
{%- for a in args %}
    {{ a.name }}: {{ a.ty }},
{%- endfor %}
) -> ServerFnResult<{{ ret }}> {
    // Server-side code runs on Axum (Dioxus 0.7).
    Ok({% if ret == 'String' %}String::new(){% elif ret.startswith('Vec<') %}Vec::new(){% else %}Default::default(){% endif %})
}
"#;

#[derive(Debug, Serialize)]
pub struct ScaffoldResult {
    pub files_created: Vec<PathBuf>,
    pub files_modified: Vec<PathBuf>,
    pub next_steps: Vec<String>,
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

    let mut env = Environment::new();
    env.add_template("component", COMPONENT_TPL).unwrap();
    let tpl = env.get_template("component").unwrap();
    let rendered = tpl
        .render(context! {
            pascal => pascal.clone(),
            snake => snake.clone(),
            has_props => !p.props.is_empty(),
            props => p.props.iter().map(|p| context!{ name => p.name.clone(), ty => p.ty.clone(), optional => p.optional }).collect::<Vec<_>>(),
        })
        .map_err(|e| e.to_string())?;
    std::fs::write(&target, rendered).map_err(|e| e.to_string())?;

    // ensure mod.rs exports it
    let mod_rs = components_dir.join("mod.rs");
    let mut modified = Vec::new();
    let line = format!("pub mod {snake};\npub use {snake}::*;\n");
    if mod_rs.exists() {
        let mut current = std::fs::read_to_string(&mod_rs).map_err(|e| e.to_string())?;
        if !current.contains(&format!("pub mod {snake};")) {
            if !current.ends_with('\n') {
                current.push('\n');
            }
            current.push_str(&line);
            std::fs::write(&mod_rs, current).map_err(|e| e.to_string())?;
            modified.push(mod_rs.clone());
        }
    } else {
        std::fs::write(&mod_rs, line).map_err(|e| e.to_string())?;
    }

    let next_steps = vec![
        format!("`use crate::components::{pascal};` where you want to render it"),
        "wire `mod components;` into your crate root if it isn't already".into(),
    ];

    Ok(ScaffoldResult {
        files_created: if modified.contains(&mod_rs) {
            vec![target]
        } else {
            vec![target, mod_rs]
        },
        files_modified: modified,
        next_steps,
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
}

pub async fn create_route(
    state: &Arc<State>,
    p: CreateRouteParams,
) -> Result<ScaffoldResult, String> {
    let crate_root = crate_root(state, p.project_root.as_deref()).await?;
    let router_file = match p.router_file.as_deref() {
        Some(rf) => crate_root.join(rf),
        None => find_routable(&crate_root)
            .ok_or_else(|| "could not find a Routable enum in src/; pass router_file".to_string())?,
    };

    let src = std::fs::read_to_string(&router_file)
        .map_err(|e| format!("read {}: {e}", router_file.display()))?;
    let file = syn::parse_file(&src).map_err(|e| format!("parse: {e}"))?;

    let enum_name = file
        .items
        .iter()
        .find_map(|it| match it {
            syn::Item::Enum(e)
                if e.attrs.iter().any(|a| has_derive(a, "Routable")) =>
            {
                Some(e.ident.to_string())
            }
            _ => None,
        })
        .ok_or_else(|| {
            format!("no `#[derive(Routable)]` enum in {}", router_file.display())
        })?;

    let variant_name = p.component.to_pascal_case();
    let variant = format!(
        "    #[route(\"{}\")]\n    {variant} {{}},\n",
        p.path,
        variant = variant_name
    );

    // Insert before the final `}` of the enum block. Find the right one by scanning items.
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
    new_src.push_str(&variant);
    new_src.push_str(&src[end..]);
    std::fs::write(&router_file, &new_src).map_err(|e| e.to_string())?;

    Ok(ScaffoldResult {
        files_created: vec![],
        files_modified: vec![router_file.clone()],
        next_steps: vec![
            format!("ensure `{}` exists and is in scope at the routable enum", variant_name),
            "consider running `cargo fmt` on the router file".into(),
        ],
    })
}

fn has_derive(attr: &syn::Attribute, target: &str) -> bool {
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

fn find_routable(crate_root: &Path) -> Option<PathBuf> {
    for cand in &["src/router.rs", "src/route.rs", "src/main.rs", "src/lib.rs"] {
        let p = crate_root.join(cand);
        if let Ok(s) = std::fs::read_to_string(&p) {
            if s.contains("Routable") {
                return Some(p);
            }
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
        if let Ok(s) = std::fs::read_to_string(entry.path()) {
            if s.contains("#[derive(Routable") || s.contains("derive(Routable") {
                return Some(entry.path().to_path_buf());
            }
        }
    }
    None
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

    let pascal = p.name.to_pascal_case();
    let snake = p.name.to_snake_case();
    let ret = p.return_type.unwrap_or_else(|| "String".into());
    let target = server_dir.join(format!("{snake}.rs"));
    if target.exists() {
        return Err(format!("{} already exists", target.display()));
    }

    let mut env = Environment::new();
    env.add_template("server_fn", SERVER_FN_TPL).unwrap();
    let tpl = env.get_template("server_fn").unwrap();
    let rendered = tpl
        .render(context! {
            pascal => pascal.clone(),
            snake => snake.clone(),
            ret => ret.clone(),
            args => p.args.iter().map(|a| context!{ name => a.name.clone(), ty => a.ty.clone() }).collect::<Vec<_>>(),
        })
        .map_err(|e| e.to_string())?;
    std::fs::write(&target, rendered).map_err(|e| e.to_string())?;

    // ensure src/server/mod.rs declares it
    let mod_rs = server_dir.join("mod.rs");
    let line = format!("pub mod {snake};\npub use {snake}::*;\n");
    let mut modified = Vec::new();
    if mod_rs.exists() {
        let mut current = std::fs::read_to_string(&mod_rs).map_err(|e| e.to_string())?;
        if !current.contains(&format!("pub mod {snake};")) {
            if !current.ends_with('\n') {
                current.push('\n');
            }
            current.push_str(&line);
            std::fs::write(&mod_rs, current).map_err(|e| e.to_string())?;
            modified.push(mod_rs.clone());
        }
    } else {
        std::fs::write(&mod_rs, line).map_err(|e| e.to_string())?;
    }

    let next_steps = vec![
        format!("call `{snake}(...)` from a client component to invoke it"),
        "ensure `mod server;` is declared in your crate root".into(),
    ];

    Ok(ScaffoldResult {
        files_created: if modified.contains(&mod_rs) {
            vec![target]
        } else {
            vec![target, mod_rs]
        },
        files_modified: modified,
        next_steps,
    })
}

async fn crate_root(state: &Arc<State>, project_root: Option<&str>) -> Result<PathBuf, String> {
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
