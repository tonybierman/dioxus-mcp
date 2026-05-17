//! `describe_component`: returns the full prop / event surface for a Dioxus
//! 0.7 catalog component.
//!
//! Why this exists: `list_components` gives one-liners, but authoring rsx!
//! against a widget requires the prop names, types, defaults, event-handler
//! signatures, and the `extends` surface — and many wrappers forward their
//! prop struct from `dioxus_primitives`, so a single read is never enough.
//! This tool walks the upstream template AND the underlying primitive's
//! `*Props` so the caller doesn't have to chase into
//! `~/.cargo/git/checkouts/components-…/primitives/src/<name>.rs` by hand.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use heck::ToPascalCase;
use quote::ToTokens;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::execute::DX_COMPONENT_CATALOG_ENTRIES;
use crate::state::State;
use crate::tools::{ambiguous_attrs_for_element, resolve_in_project, tighten_type};

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct DescribeComponentParams {
    /// snake_case catalog name (e.g. `button`, `date_picker`). Call
    /// `list_components` first if you're unsure which entry to ask about.
    pub name: String,
    /// Optional project root override. Defaults to the detected manifest dir.
    /// Only used to locate a project-local install when the upstream cargo
    /// checkout isn't available.
    #[serde(default)]
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct PropEntry {
    pub name: String,
    pub ty: String,
    /// `Option<T>` in source — the callsite may omit it entirely.
    pub optional: bool,
    /// `#[props(default)]` or `#[props(default = ...)]` is present.
    pub has_default: bool,
    /// Source-form expression when given as `default = <expr>`.
    pub default: Option<String>,
    /// `#[props(extends=...)]` targets, e.g. `["GlobalAttributes", "input"]`.
    pub extends: Vec<String>,
    /// Heuristically detected: any `EventHandler<…>` or `Callback<…>` type.
    pub event_handler: bool,
    /// Field/parameter doc comment (joined `///` lines).
    pub doc: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct VariantEnum {
    pub name: String,
    pub default: Option<String>,
    pub variants: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct PrimitiveRef {
    /// Type path as written in the wrapper, e.g. `CheckboxProps` or
    /// `combobox::ComboboxProps`.
    pub path: String,
    /// Resolved file in the upstream primitives crate (best-effort).
    pub source: Option<PathBuf>,
    pub props: Vec<PropEntry>,
    /// Every pub enum defined in the same primitive file. Used by the
    /// top-level `referenced_enums` pass to inline variants of types like
    /// `CheckboxState` that show up inside a prop type and would otherwise
    /// force the caller to grep `~/.cargo/git/checkouts/components-*`.
    #[serde(skip_serializing)]
    pub same_file_enums: Vec<VariantEnum>,
}

#[derive(Debug, Serialize)]
pub struct DescribeComponentResult {
    pub name: String,
    pub description: String,
    /// `use crate::components::<name>::<Pascal>;`
    pub import: String,
    /// `"upstream"` = cargo git checkout, `"project"` = project-local install.
    pub source_kind: &'static str,
    pub source: PathBuf,
    /// Pretty-printed component fn signature (attrs + sig, no body).
    pub signature: String,
    /// `"inline"` = the wrapper fn lists each prop explicitly; `"primitive"`
    /// = the wrapper just forwards `props: <SomeProps>` and these entries
    /// were copied from that resolved primitive struct so a first read isn't
    /// misleadingly empty. When `"primitive"`, `primitive.props` mirrors
    /// this list one-for-one — kept for callers who want the type path too.
    pub props_source: &'static str,
    pub props: Vec<PropEntry>,
    pub variants: Vec<VariantEnum>,
    /// Aggregated extends across every prop (deduped).
    pub extends: Vec<String>,
    /// Names of every prop whose type contains `EventHandler<…>` or `Callback<…>`.
    pub event_handlers: Vec<String>,
    /// Primitive `*Props` looked up via the wrapper's `use dioxus_primitives::…`.
    pub primitive: Option<PrimitiveRef>,
    /// docs.md from the upstream template (absent when reading a project
    /// install — `dx components add` strips docs.md on copy).
    pub docs: Option<String>,
    /// Verbatim `use` statements from the wrapper file (helps callers wire
    /// imports without guessing).
    pub uses: Vec<String>,
    /// Enums whose name appears inside one of the prop types (e.g.
    /// `CheckboxState` inside `ReadSignal<Option<CheckboxState>>`),
    /// resolved from either the wrapper file or the upstream primitive file.
    /// Saves callers a grep into `~/.cargo/git/checkouts/components-*`.
    pub referenced_enums: Vec<VariantEnum>,
    /// Attribute names that trip E0034 on this component because both
    /// `GlobalAttributesExtension` and the element-specific extension trait
    /// (looked up from `extends`) define them. Always use the explicit
    /// attribute-literal syntax for these (`"autofocus": "true"`). When
    /// empty, every documented HTML attribute is safe to set directly via
    /// `attr: value` syntax.
    pub ambiguous_attributes: Vec<String>,
}

pub async fn describe_component(
    state: &Arc<State>,
    p: DescribeComponentParams,
) -> Result<DescribeComponentResult, String> {
    let name = p.name.trim();
    if name.is_empty() {
        return Err("name is required (snake_case catalog name)".into());
    }
    let entry = DX_COMPONENT_CATALOG_ENTRIES
        .iter()
        .find(|(n, _, _)| *n == name);
    let description = entry
        .map(|(_, d, _)| (*d).to_string())
        .unwrap_or_else(|| String::from("(not in the official Dioxus 0.7 catalog)"));

    let upstream = locate_upstream_component(name);
    let project_path = locate_project_component(state, &p.project_root, name).await;

    let (source, source_kind) = match (upstream.as_ref(), project_path.as_ref()) {
        (Some(p), _) => (p.clone(), "upstream"),
        (None, Some(p)) => (p.clone(), "project"),
        _ => {
            return Err(format!(
                "could not locate component template for `{name}` in upstream cargo \
                 checkout (~/.cargo/git/checkouts/components-*) or project \
                 src/components/{name}/component.rs — run `dx components add {name}` first, \
                 or invoke this tool after the cargo registry has fetched the components repo"
            ));
        }
    };

    describe_from_dir(
        name,
        &description,
        &source,
        source_kind,
        upstream.as_deref(),
    )
}

/// Parse `<dir>/component.rs` and `<dir>/docs.md`, optionally resolving the
/// primitive's `*Props` in the upstream primitives crate. Pulled out so the
/// unit tests can drive it with a fully-synthetic component dir.
fn describe_from_dir(
    name: &str,
    description: &str,
    source: &Path,
    source_kind: &'static str,
    upstream_dir: Option<&Path>,
) -> Result<DescribeComponentResult, String> {
    let comp_path = source.join("component.rs");
    let src = std::fs::read_to_string(&comp_path)
        .map_err(|e| format!("read {}: {e}", comp_path.display()))?;
    let file = syn::parse_file(&src).map_err(|e| format!("parse {}: {e}", comp_path.display()))?;

    let parsed = extract_component(&file)?;
    let docs = std::fs::read_to_string(source.join("docs.md"))
        .ok()
        .filter(|s| !s.trim().is_empty());

    // Resolve the primitive's *Props. Two cases:
    //   a) wrapper defines its own ComponentProps struct → look it up in the
    //      same file (we already grabbed it from `extra_props_structs`).
    //   b) wrapper forwards primitive props (`props: CheckboxProps`) → look
    //      it up in the upstream primitives/src/ tree.
    let primitive = parsed.primitive_type.as_ref().and_then(|type_path| {
        let last = type_path.rsplit("::").next().unwrap_or(type_path.as_str());
        // First: same-file struct (combobox-style wrappers). The wrapper's own
        // pub enums are already in `parsed.variants`; we attach them here too
        // so the `referenced_enums` pass below sees a single source of truth.
        if let Some(props) = parsed.extra_props_structs.get(last) {
            return Some(PrimitiveRef {
                path: type_path.clone(),
                source: Some(comp_path.clone()),
                props: props.clone(),
                same_file_enums: parsed.variants.clone(),
            });
        }
        // Fallback: upstream primitives crate.
        // `upstream_dir` is `<repo>/preview/src/components/<name>` (4 hops).
        let primitives_root = upstream_dir
            .and_then(|d| d.ancestors().nth(4))
            .map(|p| p.join("primitives/src"));
        primitives_root.and_then(|root| resolve_primitive(&root, type_path))
    });

    let import = format!("use crate::components::{name}::{};", name.to_pascal_case());

    // Flatten wrapper-prop forwarding: when the wrapper just takes `props:
    // SomeProps` and nothing else, the parsed inline props is empty and all
    // the real prop data is hiding under `primitive.props`. Callers reading
    // `props: []` would think the widget has no props. Promote the
    // primitive's props to the top level and mark the source.
    let (props, props_source): (Vec<PropEntry>, &'static str) = if parsed.props.is_empty()
        && let Some(prim) = primitive.as_ref()
        && !prim.props.is_empty()
    {
        (prim.props.clone(), "primitive")
    } else {
        (parsed.props, "inline")
    };

    let mut extends: Vec<String> = props.iter().flat_map(|p| p.extends.clone()).collect();
    extends.sort();
    extends.dedup();
    let event_handlers: Vec<String> = props
        .iter()
        .filter(|p| p.event_handler)
        .map(|p| p.name.clone())
        .collect();

    // Inline-resolve enums that appear inside any prop type. Candidates come
    // from the wrapper's own pub enums + any enums in the resolved primitive
    // file. Filtered to names that actually appear in a prop type string so
    // we don't dump every enum the primitive file happens to export.
    let referenced_enums = build_referenced_enums(&props, &primitive, &parsed.variants);

    // The `extends` list carries every element-specific extension trait the
    // wrapper opts into (e.g. `["GlobalAttributes", "button"]`). Surface the
    // E0034-ambiguous setters for each so callers don't have to guess which
    // attrs need the literal-string syntax.
    let mut ambiguous_attributes: Vec<String> = extends
        .iter()
        .flat_map(|e| {
            ambiguous_attrs_for_element(e)
                .iter()
                .map(|a| (*a).to_string())
        })
        .collect();
    ambiguous_attributes.sort();
    ambiguous_attributes.dedup();

    Ok(DescribeComponentResult {
        name: name.to_string(),
        description: description.to_string(),
        import,
        source_kind,
        source: source.to_path_buf(),
        signature: parsed.signature,
        props_source,
        props,
        variants: parsed.variants,
        extends,
        event_handlers,
        primitive,
        docs,
        uses: parsed.uses,
        referenced_enums,
        ambiguous_attributes,
    })
}

fn build_referenced_enums(
    props: &[PropEntry],
    primitive: &Option<PrimitiveRef>,
    wrapper_variants: &[VariantEnum],
) -> Vec<VariantEnum> {
    let mut referenced: Vec<String> = Vec::new();
    for p in props {
        for ident in extract_type_idents(&p.ty) {
            if !referenced.contains(&ident) {
                referenced.push(ident);
            }
        }
    }
    if let Some(prim) = primitive {
        for p in &prim.props {
            for ident in extract_type_idents(&p.ty) {
                if !referenced.contains(&ident) {
                    referenced.push(ident);
                }
            }
        }
    }
    let mut pool: Vec<&VariantEnum> = Vec::new();
    pool.extend(wrapper_variants.iter());
    if let Some(prim) = primitive {
        pool.extend(prim.same_file_enums.iter());
    }
    let mut out: Vec<VariantEnum> = Vec::new();
    for name in &referenced {
        if let Some(v) = pool.iter().find(|v| v.name == *name) {
            // Dedup by name in case the wrapper and primitive both define it.
            if !out.iter().any(|x| x.name == v.name) {
                out.push((*v).clone());
            }
        }
    }
    out
}

struct ParsedComponent {
    signature: String,
    props: Vec<PropEntry>,
    variants: Vec<VariantEnum>,
    /// `(props: SomeProps)` → `Some("SomeProps")` (or path form).
    primitive_type: Option<String>,
    /// Every `pub struct *Props` found in the file, keyed by ident.
    extra_props_structs: std::collections::BTreeMap<String, Vec<PropEntry>>,
    uses: Vec<String>,
}

fn extract_component(file: &syn::File) -> Result<ParsedComponent, String> {
    let mut component_fn: Option<syn::ItemFn> = None;
    let mut variants = Vec::new();
    let mut uses = Vec::new();
    let mut extra_props_structs = std::collections::BTreeMap::new();
    for item in &file.items {
        match item {
            syn::Item::Fn(f) => {
                let is_component = f.attrs.iter().any(|a| {
                    a.path()
                        .segments
                        .last()
                        .map(|s| s.ident == "component")
                        .unwrap_or(false)
                });
                if is_component && component_fn.is_none() {
                    component_fn = Some(f.clone());
                }
            }
            syn::Item::Enum(e) if matches!(e.vis, syn::Visibility::Public(_)) => {
                variants.push(extract_enum(e));
            }
            syn::Item::Struct(s) if matches!(s.vis, syn::Visibility::Public(_)) => {
                let name = s.ident.to_string();
                if name.ends_with("Props")
                    && let Some(props) = struct_to_props(s)
                {
                    extra_props_structs.insert(name, props);
                }
            }
            syn::Item::Use(u) => {
                uses.push(tighten_type(&u.to_token_stream().to_string()));
            }
            _ => {}
        }
    }
    let component_fn =
        component_fn.ok_or_else(|| "no `#[component]` fn in this file".to_string())?;
    let signature = format_fn_sig(&component_fn);
    let (props, primitive_type) = extract_props(&component_fn);

    Ok(ParsedComponent {
        signature,
        props,
        variants,
        primitive_type,
        extra_props_structs,
        uses,
    })
}

fn format_fn_sig(f: &syn::ItemFn) -> String {
    let sig = &f.sig;
    let attrs: Vec<_> = f
        .attrs
        .iter()
        .filter(|a| {
            let n = a
                .path()
                .segments
                .last()
                .map(|s| s.ident.to_string())
                .unwrap_or_default();
            n == "component" || n == "props" || n == "doc"
        })
        .collect();
    let q = quote::quote! { #(#attrs)* #sig; };
    tighten_type(&q.to_string())
}

fn extract_props(f: &syn::ItemFn) -> (Vec<PropEntry>, Option<String>) {
    let mut props = Vec::new();
    let mut primitive_type = None;
    for input in &f.sig.inputs {
        let syn::FnArg::Typed(pat) = input else {
            continue;
        };
        let name = match pat.pat.as_ref() {
            syn::Pat::Ident(i) => i.ident.to_string(),
            _ => continue,
        };
        let ty = tighten_type(&pat.ty.to_token_stream().to_string());
        // Forwarded-primitive case: `props: SomeProps` (no attrs).
        if name == "props" && pat.attrs.is_empty() {
            primitive_type = Some(ty.clone());
            continue;
        }
        let (has_default, default, extends, event_handler, doc) =
            collect_prop_attrs(&pat.attrs, &ty);
        props.push(PropEntry {
            name,
            ty: ty.clone(),
            optional: ty.starts_with("Option<"),
            has_default,
            default,
            extends,
            event_handler,
            doc,
        });
    }
    (props, primitive_type)
}

fn struct_to_props(s: &syn::ItemStruct) -> Option<Vec<PropEntry>> {
    let syn::Fields::Named(fields) = &s.fields else {
        return None;
    };
    let mut out = Vec::new();
    for f in &fields.named {
        let name = f.ident.as_ref()?.to_string();
        let ty = tighten_type(&f.ty.to_token_stream().to_string());
        let (has_default, default, extends, event_handler, doc) = collect_prop_attrs(&f.attrs, &ty);
        out.push(PropEntry {
            name,
            ty: ty.clone(),
            optional: ty.starts_with("Option<"),
            has_default,
            default,
            extends,
            event_handler,
            doc,
        });
    }
    Some(out)
}

fn collect_prop_attrs(
    attrs: &[syn::Attribute],
    ty: &str,
) -> (bool, Option<String>, Vec<String>, bool, Option<String>) {
    let mut has_default = false;
    let mut default: Option<String> = None;
    let mut extends: Vec<String> = Vec::new();
    let mut doc: Option<String> = None;
    for a in attrs {
        let p = a
            .path()
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default();
        if p == "props" {
            let _ = a.parse_nested_meta(|m| {
                let key = m
                    .path
                    .get_ident()
                    .map(|i| i.to_string())
                    .unwrap_or_default();
                match key.as_str() {
                    "default" => {
                        has_default = true;
                        if m.input.peek(syn::Token![=]) {
                            let val: syn::Expr = m.value()?.parse()?;
                            default = Some(tighten_type(&val.to_token_stream().to_string()));
                        }
                    }
                    "extends" => {
                        let val: syn::Expr = m.value()?.parse()?;
                        extends.push(tighten_type(&val.to_token_stream().to_string()));
                    }
                    _ => {}
                }
                Ok(())
            });
        } else if p == "doc"
            && let syn::Meta::NameValue(nv) = &a.meta
            && let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) = &nv.value
        {
            let t = s.value().trim().to_string();
            let buf = doc.get_or_insert_with(String::new);
            if !buf.is_empty() {
                buf.push('\n');
            }
            buf.push_str(&t);
        }
    }
    let event_handler = ty.contains("EventHandler<") || ty.contains("Callback<");
    (has_default, default, extends, event_handler, doc)
}

fn extract_enum(e: &syn::ItemEnum) -> VariantEnum {
    let name = e.ident.to_string();
    let mut default = None;
    let mut variants = Vec::new();
    for v in &e.variants {
        let is_default = v.attrs.iter().any(|a| {
            a.path()
                .segments
                .last()
                .map(|s| s.ident == "default")
                .unwrap_or(false)
        });
        if is_default {
            default = Some(v.ident.to_string());
        }
        variants.push(v.ident.to_string());
    }
    VariantEnum {
        name,
        default,
        variants,
    }
}

fn resolve_primitive(primitives_root: &Path, type_path: &str) -> Option<PrimitiveRef> {
    let last = type_path.rsplit("::").next()?;
    // Search every .rs file (also one level deep, for e.g. combobox/).
    let mut candidates: Vec<PathBuf> = Vec::new();
    collect_rs_files(primitives_root, &mut candidates, 2);
    for path in candidates {
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(props) = parse_primitive_props(&src, last) {
            let same_file_enums = parse_primitive_enums(&src);
            return Some(PrimitiveRef {
                path: type_path.to_string(),
                source: Some(path),
                props,
                same_file_enums,
            });
        }
    }
    None
}

fn collect_rs_files(root: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && depth > 0 {
            collect_rs_files(&path, out, depth - 1);
        } else if path.extension().and_then(|x| x.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn parse_primitive_props(src: &str, type_name: &str) -> Option<Vec<PropEntry>> {
    let file = syn::parse_file(src).ok()?;
    for item in &file.items {
        let syn::Item::Struct(s) = item else {
            continue;
        };
        if s.ident != type_name {
            continue;
        }
        return struct_to_props(s);
    }
    None
}

fn parse_primitive_enums(src: &str) -> Vec<VariantEnum> {
    let Ok(file) = syn::parse_file(src) else {
        return Vec::new();
    };
    file.items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Enum(e) if matches!(e.vis, syn::Visibility::Public(_)) => {
                Some(extract_enum(e))
            }
            _ => None,
        })
        .collect()
}

/// Pick PascalCase identifiers out of a type string. Filters out the common
/// container types (`Option`, `Vec`, `ReadSignal`, `Callback`, …) so the
/// caller only sees enum-shaped candidates worth resolving.
fn extract_type_idents(ty: &str) -> Vec<String> {
    const SKIP: &[&str] = &[
        "Option",
        "Vec",
        "Box",
        "Rc",
        "Arc",
        "Cell",
        "RefCell",
        "Cow",
        "HashMap",
        "HashSet",
        "BTreeMap",
        "BTreeSet",
        "ReadSignal",
        "Signal",
        "WriteSignal",
        "Memo",
        "Resource",
        "Element",
        "Event",
        "EventHandler",
        "Callback",
        "Attribute",
        "Children",
        "String",
        "PathBuf",
        "Result",
        "GlobalAttributes",
    ];
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for ch in ty.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            current.push(ch);
        } else {
            if !current.is_empty() {
                push_if_enum_candidate(&mut out, &current, SKIP);
            }
            current.clear();
        }
    }
    if !current.is_empty() {
        push_if_enum_candidate(&mut out, &current, SKIP);
    }
    out
}

fn push_if_enum_candidate(out: &mut Vec<String>, ident: &str, skip: &[&str]) {
    if !ident.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
        return;
    }
    if skip.contains(&ident) {
        return;
    }
    if !out.iter().any(|x| x == ident) {
        out.push(ident.to_string());
    }
}

fn locate_upstream_component(name: &str) -> Option<PathBuf> {
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))?;
    let dir = cargo_home.join("git/checkouts");
    let entries = std::fs::read_dir(&dir).ok()?;
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let fname = entry.file_name();
        let s = fname.to_str().unwrap_or("");
        if !s.starts_with("components-") {
            continue;
        }
        let Ok(inner) = std::fs::read_dir(entry.path()) else {
            continue;
        };
        for sub in inner.flatten() {
            if !sub.path().is_dir() {
                continue;
            }
            let mtime = sub
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let candidate = sub.path().join("preview/src/components").join(name);
            if !candidate.exists() {
                continue;
            }
            if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
                best = Some((mtime, candidate));
            }
        }
    }
    best.map(|(_, p)| p)
}

async fn locate_project_component(
    state: &Arc<State>,
    project_root: &Option<String>,
    name: &str,
) -> Option<PathBuf> {
    let candidate = resolve_in_project(
        state,
        &format!("src/components/{name}"),
        project_root.as_deref(),
    )
    .await;
    if candidate.join("component.rs").exists() {
        Some(candidate)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_files(dir: &Path, files: &[(&str, &str)]) {
        for (rel, body) in files {
            let path = dir.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, body).unwrap();
        }
    }

    #[test]
    fn parses_inline_props_and_variants() {
        let dir = tempdir().unwrap();
        let src = r#"
use dioxus::prelude::*;
use dioxus_primitives::dioxus_attributes::attributes;

#[derive(Copy, Clone, PartialEq, Default)]
#[non_exhaustive]
pub enum ButtonVariant {
    #[default]
    Primary,
    Secondary,
}

#[component]
pub fn Button(
    /// Visual variant.
    #[props(default)] variant: ButtonVariant,
    #[props(extends=GlobalAttributes)]
    #[props(extends=button)]
    attributes: Vec<Attribute>,
    onclick: Option<EventHandler<MouseEvent>>,
    children: Element,
) -> Element {
    rsx! { button {} }
}
"#;
        write_files(dir.path(), &[("component.rs", src)]);
        let r =
            describe_from_dir("button", "desc", dir.path(), "upstream", None).expect("describe ok");
        assert_eq!(r.name, "button");
        assert_eq!(r.props_source, "inline");
        // Inline props: variant, attributes, onclick, children — `attributes`
        // is included because `extends` is meta-info, not a skip signal.
        let names: Vec<&str> = r.props.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"variant"));
        assert!(names.contains(&"onclick"));
        assert!(names.contains(&"attributes"));
        // event_handlers detected by Option<EventHandler<...>>
        assert!(r.event_handlers.contains(&"onclick".into()));
        // extends aggregated
        assert!(r.extends.iter().any(|e| e == "GlobalAttributes"));
        assert!(r.extends.iter().any(|e| e == "button"));
        // variants
        let v = r
            .variants
            .iter()
            .find(|v| v.name == "ButtonVariant")
            .unwrap();
        assert_eq!(v.default.as_deref(), Some("Primary"));
        assert!(v.variants.contains(&"Primary".into()));
        // doc string captured
        let variant = r.props.iter().find(|p| p.name == "variant").unwrap();
        assert_eq!(variant.doc.as_deref(), Some("Visual variant."));
        // has_default flag
        assert!(variant.has_default);
    }

    #[test]
    fn forwarded_primitive_props_picked_up_from_same_file() {
        let dir = tempdir().unwrap();
        // Combobox-style: wrapper defines a Props struct AND forwards it.
        let src = r#"
use dioxus::prelude::*;

#[derive(Props, Clone, PartialEq)]
pub struct ComboboxProps {
    /// The currently-selected value.
    #[props(default)]
    pub value: ReadSignal<Option<String>>,
    #[props(default = Callback::new(|_| {}))]
    pub on_value_change: Callback<Option<String>>,
    #[props(extends = GlobalAttributes)]
    pub attributes: Vec<Attribute>,
}

#[component]
pub fn Combobox(props: ComboboxProps) -> Element {
    rsx! { div {} }
}
"#;
        write_files(dir.path(), &[("component.rs", src)]);
        let r = describe_from_dir("combobox", "desc", dir.path(), "upstream", None)
            .expect("describe ok");
        // Wrapper forwards `props: ComboboxProps` — the inline `props` list is
        // empty in source, so the primitive's props are promoted to the top
        // level for callers' first read.
        assert_eq!(r.props_source, "primitive");
        let names: Vec<&str> = r.props.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"value"));
        assert!(names.contains(&"on_value_change"));
        // `event_handlers` and `extends` are aggregated from the promoted props.
        assert!(r.event_handlers.contains(&"on_value_change".into()));
        assert!(r.extends.iter().any(|e| e == "GlobalAttributes"));
        // Primitive ref still surfaces the type path (and resolved location).
        let prim = r.primitive.expect("primitive resolved");
        assert_eq!(prim.path, "ComboboxProps");
        let pnames: Vec<&str> = prim.props.iter().map(|p| p.name.as_str()).collect();
        assert!(pnames.contains(&"value"));
        assert!(pnames.contains(&"on_value_change"));
        // event handler detected via Callback<…>
        let ovc = prim
            .props
            .iter()
            .find(|p| p.name == "on_value_change")
            .unwrap();
        assert!(ovc.event_handler);
        // default expression captured verbatim
        assert!(
            ovc.default
                .as_deref()
                .unwrap_or("")
                .contains("Callback::new")
        );
    }

    #[test]
    fn primitive_resolved_from_upstream_primitives_tree() {
        let root = tempdir().unwrap();
        let dir = root.path().join("preview/src/components/checkbox");
        let primitives = root.path().join("primitives/src");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&primitives).unwrap();
        let wrapper = r#"
use dioxus::prelude::*;
use dioxus_primitives::checkbox::CheckboxProps;

#[component]
pub fn Checkbox(props: CheckboxProps) -> Element {
    rsx! { input {} }
}
"#;
        let primitive = r#"
use dioxus::prelude::*;

#[derive(Props, Clone, PartialEq)]
pub struct CheckboxProps {
    /// Whether checked.
    #[props(default)]
    pub checked: ReadSignal<Option<bool>>,
    #[props(default)]
    pub on_checked_change: Callback<bool>,
}
"#;
        std::fs::write(dir.join("component.rs"), wrapper).unwrap();
        std::fs::write(primitives.join("checkbox.rs"), primitive).unwrap();
        let r = describe_from_dir("checkbox", "desc", &dir, "upstream", Some(&dir))
            .expect("describe ok");
        // Top-level props were promoted from the upstream primitive struct.
        assert_eq!(r.props_source, "primitive");
        let top: Vec<&str> = r.props.iter().map(|p| p.name.as_str()).collect();
        assert!(top.contains(&"checked"));
        assert!(top.contains(&"on_checked_change"));
        let prim = r.primitive.expect("primitive resolved");
        let names: Vec<&str> = prim.props.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"checked"));
        assert!(names.contains(&"on_checked_change"));
    }

    #[test]
    fn inlines_referenced_enum_from_primitive_file() {
        // Real case from todo_mvc: the agent saw `checked: ReadSignal<Option<CheckboxState>>`
        // but had to grep the primitive to find CheckboxState's variants. The
        // resolver should pick up the same-file enum automatically.
        let root = tempdir().unwrap();
        let dir = root.path().join("preview/src/components/checkbox");
        let primitives = root.path().join("primitives/src");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&primitives).unwrap();
        let wrapper = r#"
use dioxus::prelude::*;
use dioxus_primitives::checkbox::CheckboxProps;

#[component]
pub fn Checkbox(props: CheckboxProps) -> Element {
    rsx! { input {} }
}
"#;
        let primitive = r#"
use dioxus::prelude::*;

#[derive(Copy, Clone, PartialEq, Default)]
pub enum CheckboxState {
    #[default]
    Unchecked,
    Checked,
    Indeterminate,
}

#[derive(Props, Clone, PartialEq)]
pub struct CheckboxProps {
    #[props(default)]
    pub checked: ReadSignal<Option<CheckboxState>>,
    #[props(default)]
    pub on_checked_change: Callback<CheckboxState>,
}
"#;
        std::fs::write(dir.join("component.rs"), wrapper).unwrap();
        std::fs::write(primitives.join("checkbox.rs"), primitive).unwrap();
        let r = describe_from_dir("checkbox", "desc", &dir, "upstream", Some(&dir))
            .expect("describe ok");
        let cs = r
            .referenced_enums
            .iter()
            .find(|v| v.name == "CheckboxState")
            .expect("CheckboxState inlined into referenced_enums");
        assert_eq!(cs.default.as_deref(), Some("Unchecked"));
        assert!(cs.variants.contains(&"Checked".into()));
        assert!(cs.variants.contains(&"Indeterminate".into()));
    }

    #[test]
    fn surfaces_ambiguous_attributes_for_button_input() {
        // The Button template extends `button`, so `autofocus` is E0034-ambiguous
        // — callers must use the string-key form `"autofocus": "true"`.
        let dir = tempdir().unwrap();
        let src = r#"
use dioxus::prelude::*;

#[component]
pub fn Button(
    #[props(extends=GlobalAttributes)]
    #[props(extends=button)]
    attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    rsx! { button {} }
}
"#;
        write_files(dir.path(), &[("component.rs", src)]);
        let r =
            describe_from_dir("button", "desc", dir.path(), "upstream", None).expect("describe ok");
        assert!(r.extends.iter().any(|e| e == "button"));
        assert_eq!(r.ambiguous_attributes, vec!["autofocus".to_string()]);
    }

    #[test]
    fn no_ambiguous_attributes_when_no_element_extension() {
        // A component that only extends GlobalAttributes (no element-specific
        // trait) has no ambiguous attrs — every documented attr is safe.
        let dir = tempdir().unwrap();
        let src = r#"
use dioxus::prelude::*;

#[component]
pub fn Card(
    #[props(extends=GlobalAttributes)]
    attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    rsx! { div {} }
}
"#;
        write_files(dir.path(), &[("component.rs", src)]);
        let r =
            describe_from_dir("card", "desc", dir.path(), "upstream", None).expect("describe ok");
        assert!(r.ambiguous_attributes.is_empty());
    }

    #[test]
    fn inlines_referenced_enum_from_wrapper_file() {
        // Same-file (combobox-style) wrappers: the enum lives next to the
        // props struct. Confirm the resolver picks that up too.
        let dir = tempdir().unwrap();
        let src = r#"
use dioxus::prelude::*;

#[derive(Copy, Clone, PartialEq, Default)]
pub enum ButtonSize {
    #[default]
    Md,
    Sm,
    Lg,
}

#[component]
pub fn Button(
    #[props(default)] size: ButtonSize,
    children: Element,
) -> Element {
    rsx! { button {} }
}
"#;
        write_files(dir.path(), &[("component.rs", src)]);
        let r =
            describe_from_dir("button", "desc", dir.path(), "upstream", None).expect("describe ok");
        let bs = r
            .referenced_enums
            .iter()
            .find(|v| v.name == "ButtonSize")
            .expect("ButtonSize inlined into referenced_enums");
        assert!(bs.variants.contains(&"Sm".into()));
        assert!(bs.variants.contains(&"Lg".into()));
    }
}
