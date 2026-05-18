use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use quote::ToTokens;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::state::State;
use crate::tools::ast::{ParseError, collect_parse_errors, walk_rs_files};
use crate::tools::scaffold::{crate_root, has_derive};
use crate::tools::tighten_type;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ProjectIndexParams {
    /// Directory to scan, relative to the crate root. Defaults to `src/`.
    pub path: Option<String>,
    /// Filter by kind: "component" or "server_fn". Omit to return both.
    pub kind: Option<String>,
    /// Absolute path to the Dioxus project root. Defaults to the path the MCP server was
    /// started in.
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PropEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub optional: bool,
}

#[derive(Debug, Serialize)]
pub struct ComponentEntry {
    pub name: String,
    pub file: PathBuf,
    pub line: usize,
    pub props: Vec<PropEntry>,
    /// True when props come from a separate `#[derive(Props)]` struct rather than inline fn args.
    pub via_props_struct: bool,
}

#[derive(Debug, Serialize)]
pub struct ServerArg {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
}

#[derive(Debug, Serialize)]
pub struct ServerFnEntry {
    pub name: String,
    /// The optional name in `#[server(Name)]`, when present.
    pub server_name: Option<String>,
    pub file: PathBuf,
    pub line: usize,
    pub args: Vec<ServerArg>,
    /// Extractor args declared inside the verb-macro attribute itself, e.g.
    /// `#[post("/api/cards", cookies: TypedHeader<Cookie>)]`. The Dioxus 0.7
    /// verb-macro binds these names into the fn scope without listing them
    /// in the fn signature, so they don't appear under `args`. Consumers
    /// that want to know "is this fn cookie-gated?" must inspect both.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub attr_args: Vec<ServerArg>,
    /// Inner type of `ServerFnResult<T>`, or the raw return type if the shape doesn't match.
    pub return_type: String,
    /// HTTP method for `#[get/post/put/delete/patch("/path")]` attribute-style server fns.
    /// None for legacy `#[server]` attribute form.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// Route path literal from the HTTP attribute, e.g. `/api/board` from `#[get("/api/board")]`.
    /// None for legacy `#[server]` attribute form.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_path: Option<String>,
}

impl ServerFnEntry {
    /// True iff a `cookies:` extractor appears in either the fn signature
    /// (legacy `#[server]` form) or the verb-macro attribute (the canonical
    /// 0.7-fullstack shape). Auditing tools should call this instead of
    /// re-implementing the predicate on `args` alone.
    pub fn is_cookie_gated(&self) -> bool {
        self.args.iter().any(is_cookie_arg) || self.attr_args.iter().any(is_cookie_arg)
    }
}

/// Shared cookie-extractor heuristic. Accepts either a bare `cookies` arg name
/// (the convention the DSL emits) or any arg whose type stringifies to mention
/// both `TypedHeader` and `Cookie` (the canonical type when users hand-write
/// the extractor with a non-`cookies` ident, e.g. `let session: TypedHeader<Cookie>`).
pub fn is_cookie_arg(a: &ServerArg) -> bool {
    if a.name == "cookies" {
        return true;
    }
    a.ty.contains("TypedHeader") && a.ty.contains("Cookie")
}

#[derive(Debug, Serialize)]
pub struct ProjectIndexReport {
    pub root: PathBuf,
    pub components: Vec<ComponentEntry>,
    pub server_fns: Vec<ServerFnEntry>,
    pub parse_errors: Vec<ParseError>,
}

pub async fn project_index(
    state: &Arc<State>,
    p: ProjectIndexParams,
) -> Result<ProjectIndexReport, String> {
    let crate_root = crate_root(state, p.project_root.as_deref()).await?;
    let scan_dir = crate_root.join(p.path.as_deref().unwrap_or("src"));

    let want_components = p.kind.as_deref().map(|k| k == "component").unwrap_or(true);
    let want_server_fns = p.kind.as_deref().map(|k| k == "server_fn").unwrap_or(true);

    let mut components: Vec<ComponentEntry> = Vec::new();
    let mut server_fns: Vec<ServerFnEntry> = Vec::new();

    let files = walk_rs_files(&scan_dir);
    for sf in &files {
        let Ok(file) = &sf.ast else { continue };
        let path = sf.path.as_path();

        let mut props_structs: HashMap<String, &syn::ItemStruct> = HashMap::new();
        for it in &file.items {
            if let syn::Item::Struct(s) = it
                && s.attrs.iter().any(|a| has_derive(a, "Props"))
            {
                props_structs.insert(s.ident.to_string(), s);
            }
        }

        for it in &file.items {
            let syn::Item::Fn(f) = it else { continue };

            let has_component = f.attrs.iter().any(|a| last_seg_is(a.path(), "component"));
            let server_attr = f.attrs.iter().find(|a| last_seg_is(a.path(), "server"));
            let http_attr = f
                .attrs
                .iter()
                .find_map(|a| http_method_for(a.path()).map(|m| (a, m)));

            if want_components && has_component {
                components.push(build_component(f, path, &props_structs));
            }
            if want_server_fns {
                if let Some(attr) = server_attr {
                    server_fns.push(build_server_fn(f, attr, path));
                } else if let Some((attr, method)) = http_attr {
                    server_fns.push(build_http_server_fn(f, attr, method, path));
                }
            }
        }
    }

    components.sort_by(|a, b| a.name.cmp(&b.name));
    server_fns.sort_by(|a, b| a.name.cmp(&b.name));
    let parse_errors = collect_parse_errors(&files);

    Ok(ProjectIndexReport {
        root: scan_dir,
        components,
        server_fns,
        parse_errors,
    })
}

fn build_component(
    f: &syn::ItemFn,
    path: &Path,
    props_structs: &HashMap<String, &syn::ItemStruct>,
) -> ComponentEntry {
    let name = f.sig.ident.to_string();
    let line = f.sig.fn_token.span.start().line;

    let typed_args: Vec<&syn::PatType> = f
        .sig
        .inputs
        .iter()
        .filter_map(|i| match i {
            syn::FnArg::Typed(pt) => Some(pt),
            _ => None,
        })
        .collect();

    let mut via_props_struct = false;
    let mut props: Vec<PropEntry> = Vec::new();

    if typed_args.len() == 1
        && let Some(struct_name) = last_ident(&typed_args[0].ty)
        && let Some(s) = props_structs.get(&struct_name)
    {
        via_props_struct = true;
        props = extract_props_from_struct(s);
    }

    if !via_props_struct {
        for pt in &typed_args {
            props.push(PropEntry {
                name: pat_to_name(&pt.pat),
                ty: tighten_type(&pt.ty.to_token_stream().to_string()),
                optional: is_option_type(&pt.ty),
            });
        }
    }

    ComponentEntry {
        name,
        file: path.to_path_buf(),
        line,
        props,
        via_props_struct,
    }
}

fn build_server_fn(f: &syn::ItemFn, attr: &syn::Attribute, path: &Path) -> ServerFnEntry {
    let name = f.sig.ident.to_string();
    let line = f.sig.fn_token.span.start().line;

    let server_name = attr
        .parse_args::<syn::Path>()
        .ok()
        .and_then(|p| p.segments.last().map(|s| s.ident.to_string()));

    let args: Vec<ServerArg> = server_fn_args(f);
    let return_type = server_fn_return_type(f);

    ServerFnEntry {
        name,
        server_name,
        file: path.to_path_buf(),
        line,
        args,
        attr_args: Vec::new(),
        return_type,
        method: None,
        route_path: None,
    }
}

fn build_http_server_fn(
    f: &syn::ItemFn,
    attr: &syn::Attribute,
    method: String,
    path: &Path,
) -> ServerFnEntry {
    let name = f.sig.ident.to_string();
    let line = f.sig.fn_token.span.start().line;
    let (route_path, attr_args) = extract_http_attr(attr);

    let args: Vec<ServerArg> = server_fn_args(f);
    let return_type = server_fn_return_type(f);

    ServerFnEntry {
        name,
        server_name: None,
        file: path.to_path_buf(),
        line,
        args,
        attr_args,
        return_type,
        method: Some(method),
        route_path,
    }
}

fn server_fn_args(f: &syn::ItemFn) -> Vec<ServerArg> {
    f.sig
        .inputs
        .iter()
        .filter_map(|i| match i {
            syn::FnArg::Typed(pt) => Some(ServerArg {
                name: pat_to_name(&pt.pat),
                ty: tighten_type(&pt.ty.to_token_stream().to_string()),
            }),
            _ => None,
        })
        .collect()
}

fn server_fn_return_type(f: &syn::ItemFn) -> String {
    match &f.sig.output {
        syn::ReturnType::Type(_, ty) => {
            if let Some(inner) = unwrap_server_fn_result(ty) {
                tighten_type(&inner.to_token_stream().to_string())
            } else {
                tighten_type(&ty.to_token_stream().to_string())
            }
        }
        syn::ReturnType::Default => "()".into(),
    }
}

fn http_method_for(p: &syn::Path) -> Option<String> {
    let last = p.segments.last()?;
    let name = last.ident.to_string();
    if matches!(name.as_str(), "get" | "post" | "put" | "delete" | "patch") {
        Some(name)
    } else {
        None
    }
}

/// Parse a verb-macro attribute body into its route path and extractor list.
///
/// Accepts both shapes:
///   - `#[get("/api/board")]` → `(Some("/api/board"), [])`
///   - `#[get("/api/board", cookies: TypedHeader<Cookie>)]`
///     → `(Some("/api/board"), [ServerArg{name:"cookies", ty:"TypedHeader<Cookie>"}])`
///
/// Extractors live only inside the macro attribute — the 0.7 verb-macro binds
/// them into the fn scope itself, and putting them on the fn signature breaks
/// `FromRequest` for the body tuple. Surfacing them under `attr_args` is how
/// auditing tools (auth_map, openapi_spec) recognise cookie-gated handlers.
fn extract_http_attr(attr: &syn::Attribute) -> (Option<String>, Vec<ServerArg>) {
    let meta = match &attr.meta {
        syn::Meta::List(l) => l,
        _ => return (None, Vec::new()),
    };
    match syn::parse2::<HttpAttrBody>(meta.tokens.clone()) {
        Ok(parsed) => (
            Some(parsed.path.value()),
            parsed
                .extractors
                .into_iter()
                .map(|e| ServerArg {
                    name: e.name.to_string(),
                    ty: tighten_type(&e.ty.to_token_stream().to_string()),
                })
                .collect(),
        ),
        Err(_) => (None, Vec::new()),
    }
}

struct HttpAttrBody {
    path: syn::LitStr,
    extractors: Vec<HttpExtractor>,
}

struct HttpExtractor {
    name: syn::Ident,
    ty: syn::Type,
}

impl syn::parse::Parse for HttpAttrBody {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let path: syn::LitStr = input.parse()?;
        let mut extractors = Vec::new();
        while !input.is_empty() {
            let _: syn::Token![,] = input.parse()?;
            if input.is_empty() {
                break;
            }
            extractors.push(input.parse()?);
        }
        Ok(Self { path, extractors })
    }
}

impl syn::parse::Parse for HttpExtractor {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let name: syn::Ident = input.parse()?;
        let _: syn::Token![:] = input.parse()?;
        let ty: syn::Type = input.parse()?;
        Ok(Self { name, ty })
    }
}

fn extract_props_from_struct(s: &syn::ItemStruct) -> Vec<PropEntry> {
    let syn::Fields::Named(named) = &s.fields else {
        return Vec::new();
    };
    named
        .named
        .iter()
        .filter_map(|f| {
            let name = f.ident.as_ref()?.to_string();
            let ty = tighten_type(&f.ty.to_token_stream().to_string());
            let props_attr_default = f.attrs.iter().any(|a| {
                if !a.path().is_ident("props") {
                    return false;
                }
                let mut found = false;
                let _ = a.parse_nested_meta(|m| {
                    if m.path.is_ident("default") || m.path.is_ident("optional") {
                        found = true;
                    }
                    Ok(())
                });
                found
            });
            Some(PropEntry {
                name,
                ty,
                optional: props_attr_default || is_option_type(&f.ty),
            })
        })
        .collect()
}

fn last_seg_is(p: &syn::Path, name: &str) -> bool {
    p.segments.last().map(|s| s.ident == name).unwrap_or(false)
}

fn last_ident(ty: &syn::Type) -> Option<String> {
    if let syn::Type::Path(tp) = ty {
        tp.path.segments.last().map(|s| s.ident.to_string())
    } else {
        None
    }
}

fn pat_to_name(pat: &syn::Pat) -> String {
    match pat {
        syn::Pat::Ident(i) => i.ident.to_string(),
        syn::Pat::Type(t) => pat_to_name(&t.pat),
        _ => "_".into(),
    }
}

fn is_option_type(ty: &syn::Type) -> bool {
    if let syn::Type::Path(tp) = ty
        && let Some(seg) = tp.path.segments.last()
    {
        return seg.ident == "Option";
    }
    false
}

fn unwrap_server_fn_result(ty: &syn::Type) -> Option<&syn::Type> {
    let syn::Type::Path(tp) = ty else { return None };
    let seg = tp.path.segments.last()?;
    if seg.ident != "ServerFnResult" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    for a in &args.args {
        if let syn::GenericArgument::Type(t) = a {
            return Some(t);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http_attr(src: &str) -> syn::Attribute {
        let file: syn::File =
            syn::parse_str(&format!("{src}\nfn placeholder() {{}}\n")).expect("parses");
        let syn::Item::Fn(f) = file.items.into_iter().next().expect("fn item") else {
            panic!("expected fn item")
        };
        f.attrs.into_iter().next().expect("attribute present")
    }

    /// Plain `#[get("/api/board")]` — no extractors. The path round-trips
    /// and the attr_arg list is empty.
    #[test]
    fn extract_http_attr_path_only() {
        let attr = http_attr(r#"#[get("/api/board")]"#);
        let (path, attr_args) = extract_http_attr(&attr);
        assert_eq!(path.as_deref(), Some("/api/board"));
        assert!(attr_args.is_empty(), "no extractors: {attr_args:?}");
    }

    /// iter03's canonical shape: `cookies:` lives inside the macro
    /// attribute, not the fn signature. Before the fix this came back
    /// as zero attr_args and the handler read as ungated.
    #[test]
    fn extract_http_attr_with_cookie_extractor() {
        let attr = http_attr(
            r#"#[post("/api/cards/create", cookies: axum_extra::TypedHeader<axum_extra::headers::Cookie>)]"#,
        );
        let (path, attr_args) = extract_http_attr(&attr);
        assert_eq!(path.as_deref(), Some("/api/cards/create"));
        assert_eq!(attr_args.len(), 1, "one extractor: {attr_args:?}");
        assert_eq!(attr_args[0].name, "cookies");
        assert!(
            attr_args[0].ty.contains("TypedHeader") && attr_args[0].ty.contains("Cookie"),
            "type should preserve TypedHeader<…Cookie…>: {}",
            attr_args[0].ty,
        );
    }

    /// Multiple extractor args, comma-separated. We don't expect this in
    /// the wild today, but the parser should not choke if a user adds a
    /// second axum extractor alongside the cookie header.
    #[test]
    fn extract_http_attr_multiple_extractors() {
        let attr = http_attr(r#"#[get("/api/me", cookies: TypedHeader<Cookie>, jar: CookieJar)]"#);
        let (_path, attr_args) = extract_http_attr(&attr);
        assert_eq!(attr_args.len(), 2, "two extractors: {attr_args:?}");
        let names: Vec<&str> = attr_args.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["cookies", "jar"]);
    }

    /// `is_cookie_gated` MUST return true when the cookie extractor lives
    /// only in `attr_args` — this is the regression that the TODO called
    /// out for iter03's whole-app sample.
    #[test]
    fn is_cookie_gated_picks_up_attr_args() {
        let sf = ServerFnEntry {
            name: "create_card".into(),
            server_name: None,
            file: std::path::PathBuf::from("src/cards.rs"),
            line: 12,
            args: Vec::new(),
            attr_args: vec![ServerArg {
                name: "cookies".into(),
                ty: "axum_extra :: TypedHeader < axum_extra :: headers :: Cookie >".into(),
            }],
            return_type: "Result<u32, ServerFnError>".into(),
            method: Some("post".into()),
            route_path: Some("/api/cards/create".into()),
        };
        assert!(
            sf.is_cookie_gated(),
            "attribute-style cookies must count as gated",
        );
    }

    /// Counter-test: a handler with neither signature nor attribute
    /// extractors stays ungated.
    #[test]
    fn is_cookie_gated_false_when_no_cookies() {
        let sf = ServerFnEntry {
            name: "list_cards".into(),
            server_name: None,
            file: std::path::PathBuf::from("src/cards.rs"),
            line: 5,
            args: vec![ServerArg {
                name: "board_id".into(),
                ty: "u32".into(),
            }],
            attr_args: Vec::new(),
            return_type: "Result<Vec<Card>, ServerFnError>".into(),
            method: Some("get".into()),
            route_path: Some("/api/cards".into()),
        };
        assert!(!sf.is_cookie_gated());
    }
}
