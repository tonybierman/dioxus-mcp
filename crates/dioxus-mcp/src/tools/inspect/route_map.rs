use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use proc_macro2::TokenTree;
use quote::ToTokens;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::visit::Visit;

use crate::state::State;
use crate::tools::ast::walk_rs_files;
use crate::tools::scaffold::{crate_root, find_routable, has_derive};
use crate::tools::tighten_type;

/// Known auth-guard HOC component names. A route whose body wraps in any of
/// these is treated as gated. Sourced from the DSL's `wrap_with` option
/// (`Protected`) plus the common-by-convention names other projects use.
const KNOWN_GUARD_HOCS: &[&str] = &[
    "Protected",
    "Guarded",
    "AuthGate",
    "RequireAuth",
    "Authenticated",
    "RequiresLogin",
];

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RouteMapParams {
    /// Path to the file containing the `#[derive(Routable)]` enum. Absolute, or relative
    /// to the crate root. When omitted, common locations (src/router.rs, src/route.rs,
    /// src/main.rs, src/lib.rs) are searched, then the rest of src/ is walked.
    pub router_file: Option<String>,
    /// Absolute path to the Dioxus project root. Defaults to the path the MCP server was
    /// started in.
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RouteParam {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
}

#[derive(Debug, Serialize)]
pub struct RouteEntry {
    /// URL pattern as written in `#[route("...")]`.
    pub path: String,
    /// `path` with any enclosing `#[nest("...")]` prefixes joined in.
    pub full_path: String,
    /// Variant identifier in the enum (the target component name).
    pub component: String,
    /// Fields on the variant — URL params, query params, etc.
    pub params: Vec<RouteParam>,
    /// Stack of `#[layout(Component)]` wrappers the route is nested under.
    pub layouts: Vec<String>,
    /// Stack of `#[nest("...")]` prefixes the route is nested under.
    pub nests: Vec<String>,
    /// Auth-guard HOCs the route component wraps its body in — e.g. `Protected`,
    /// `Guarded`, `AuthGate`. Empty when the route is ungated, or when the
    /// component definition couldn't be located (likely a third-party route).
    /// Answers "which routes require auth?" without grepping. The list is
    /// stable / alphabetised; multiple entries means the body is wrapped in
    /// more than one HOC (rare but possible — e.g. `Protected` + `FeatureGate`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guards: Vec<String>,
    /// Line number of the `#[route(...)]` attribute in the source file.
    pub line: usize,
}

#[derive(Debug, Serialize)]
pub struct RouteMapReport {
    /// File the `Routable` enum was found in. `None` when no enum exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
    /// Name of the `Routable` enum. Empty when no enum exists.
    pub enum_name: String,
    pub routes: Vec<RouteEntry>,
    /// Human-readable note set when the tool degraded instead of erroring
    /// (e.g. no `Routable` enum found — app may use server-side routing).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

pub async fn route_map(state: &Arc<State>, p: RouteMapParams) -> Result<RouteMapReport, String> {
    let crate_root = crate_root(state, p.project_root.as_deref()).await?;
    let router_file = match p.router_file.as_deref() {
        Some(rf) => {
            let path = PathBuf::from(rf);
            if path.is_absolute() {
                path
            } else {
                crate_root.join(rf)
            }
        }
        None => match find_routable(&crate_root) {
            Some(p) => p,
            None => {
                return Ok(RouteMapReport {
                    file: None,
                    enum_name: String::new(),
                    routes: Vec::new(),
                    note: Some(
                        "no Routable enum found in src/; app may use server-side routing or not yet declare routes"
                            .into(),
                    ),
                });
            }
        },
    };

    let src = std::fs::read_to_string(&router_file)
        .map_err(|e| format!("read {}: {e}", router_file.display()))?;
    let file = syn::parse_file(&src).map_err(|e| format!("parse: {e}"))?;

    let routable_enum = match file.items.iter().find_map(|it| match it {
        syn::Item::Enum(e) if e.attrs.iter().any(|a| has_derive(a, "Routable")) => Some(e),
        _ => None,
    }) {
        Some(e) => e,
        None => {
            return Ok(RouteMapReport {
                file: Some(router_file.clone()),
                enum_name: String::new(),
                routes: Vec::new(),
                note: Some(format!(
                    "no `#[derive(Routable)]` enum in {}",
                    router_file.display()
                )),
            });
        }
    };

    let enum_name = routable_enum.ident.to_string();
    let mut layout_stack: Vec<String> = Vec::new();
    let mut nest_stack: Vec<String> = Vec::new();
    let mut routes: Vec<RouteEntry> = Vec::new();

    for variant in &routable_enum.variants {
        let mut route_for_variant: Option<(String, usize)> = None;

        for attr in &variant.attrs {
            let path = attr.path();
            if path.is_ident("layout") {
                if let Ok(p) = attr.parse_args::<syn::Path>()
                    && let Some(seg) = p.segments.last()
                {
                    layout_stack.push(seg.ident.to_string());
                }
            } else if path.is_ident("end_layout") {
                layout_stack.pop();
            } else if path.is_ident("nest") {
                if let Ok(lit) = attr.parse_args::<syn::LitStr>() {
                    nest_stack.push(lit.value());
                }
            } else if path.is_ident("end_nest") {
                nest_stack.pop();
            } else if path.is_ident("route")
                && let Ok(lit) = attr.parse_args::<syn::LitStr>()
            {
                let line = attr
                    .path()
                    .segments
                    .first()
                    .map(|s| s.ident.span().start().line)
                    .unwrap_or(0);
                route_for_variant = Some((lit.value(), line));
            }
        }

        let Some((route_path, line)) = route_for_variant else {
            continue;
        };

        let params = match &variant.fields {
            syn::Fields::Named(named) => named
                .named
                .iter()
                .filter_map(|f| {
                    let name = f.ident.as_ref()?.to_string();
                    let ty = tighten_type(&f.ty.to_token_stream().to_string());
                    Some(RouteParam { name, ty })
                })
                .collect(),
            _ => Vec::new(),
        };

        let full_path = join_route_path(&nest_stack, &route_path);

        routes.push(RouteEntry {
            path: route_path,
            full_path,
            component: variant.ident.to_string(),
            params,
            layouts: layout_stack.clone(),
            nests: nest_stack.clone(),
            guards: Vec::new(),
            line,
        });
    }

    // Walk src/ once and index every `#[component] fn Name` body — we use
    // this to answer "is `Name` wrapped in a known auth HOC?" without
    // re-walking once per route. Failure to parse a file just leaves the
    // component off the index (the route silently falls back to "no guards").
    let component_guards = scan_component_guards(&crate_root);
    for route in &mut routes {
        if let Some(g) = component_guards.get(&route.component) {
            route.guards = g.clone();
        }
    }

    Ok(RouteMapReport {
        file: Some(router_file),
        enum_name,
        routes,
        note: None,
    })
}

/// Walk every `.rs` file under `crate_root/src`, find `#[component] fn Name`
/// items, and return a map of `Name → list of known auth-guard HOCs the body
/// wraps in`. A function maps to a non-empty list only when its body's rsx
/// invocation contains an outermost-position component ident matching
/// [`KNOWN_GUARD_HOCS`]. Components not in the index either don't exist in
/// this crate (third-party route?) or their file failed to parse — both safe
/// "unknown, fall back to no guards" outcomes.
fn scan_component_guards(crate_root: &std::path::Path) -> BTreeMap<String, Vec<String>> {
    let src_root = crate_root.join("src");
    let files = walk_rs_files(&src_root);
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
        for item in &ast.items {
            let syn::Item::Fn(f) = item else { continue };
            let is_component = f.attrs.iter().any(|a| {
                a.path()
                    .segments
                    .last()
                    .map(|s| s.ident == "component")
                    .unwrap_or(false)
            });
            if !is_component {
                continue;
            }
            let mut v = RsxGuardVisitor { guards: Vec::new() };
            v.visit_block(&f.block);
            v.guards.sort();
            v.guards.dedup();
            // Always insert — empty list means "found the component, no
            // guards detected". This is meaningfully different from
            // "component not found" (which leaves the key absent and the
            // caller has no way to tell guard-free apart from missing).
            out.insert(f.sig.ident.to_string(), v.guards);
        }
    }
    out
}

/// Walks every `rsx!{ … }` invocation under a component's body and collects
/// any outermost-position component ident that matches a known guard HOC
/// name. "Outermost position" is approximated by checking the *first*
/// `Ident` token of each contiguous statement-level chunk inside the rsx
/// token stream — that's the receiver of the wrapping `Protected { … }`
/// form the DSL emits. Nested guards (e.g. `div { Protected { … } }`) are
/// not collected because they don't actually gate route entry, only the
/// nested subtree.
struct RsxGuardVisitor {
    guards: Vec<String>,
}

impl<'ast> Visit<'ast> for RsxGuardVisitor {
    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        let is_rsx = m
            .path
            .segments
            .last()
            .map(|s| s.ident == "rsx")
            .unwrap_or(false);
        if !is_rsx {
            syn::visit::visit_macro(self, m);
            return;
        }
        collect_top_level_idents(m.tokens.clone(), &mut self.guards);
        syn::visit::visit_macro(self, m);
    }
}

/// Scan a token stream and record every PascalCase-cased ident that appears
/// at the top level (depth 0). The DSL emits guard wrappers as
/// `Protected { ... }` — `Protected` is the leading ident, followed by a
/// `Group` (the braced child block). We add the ident only when it matches
/// a known guard HOC, and only when it sits at depth 0 of the rsx body.
fn collect_top_level_idents(ts: proc_macro2::TokenStream, hits: &mut Vec<String>) {
    for tt in ts {
        if let TokenTree::Ident(i) = tt {
            let s = i.to_string();
            if KNOWN_GUARD_HOCS.contains(&s.as_str()) && !hits.iter().any(|h| h == &s) {
                hits.push(s);
            }
        }
        // Children of a group (the `{ ... }` body of the guard) are NOT
        // recursed into — a `Protected` deeper in the tree gates a
        // sub-region, not the route, and we don't want to over-report.
    }
}

fn join_route_path(nests: &[String], path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for nest in nests {
        parts.extend(nest.split('/').filter(|s| !s.is_empty()));
    }
    parts.extend(path.split('/').filter(|s| !s.is_empty()));
    if parts.is_empty() {
        "/".into()
    } else {
        format!("/{}", parts.join("/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guards_for(src: &str) -> Vec<String> {
        let file = syn::parse_file(src).expect("parse");
        let f = file
            .items
            .iter()
            .find_map(|i| match i {
                syn::Item::Fn(f) => Some(f),
                _ => None,
            })
            .expect("fn item");
        let mut v = RsxGuardVisitor { guards: Vec::new() };
        v.visit_block(&f.block);
        v.guards.sort();
        v.guards.dedup();
        v.guards
    }

    /// Standup's `BoardScreen` wraps its body in `Protected { ... }` —
    /// the canonical DSL `wrap_with: Protected` shape. Before the fix
    /// `route_map` showed `/` as ungated; the body's guard ident now
    /// shows up under `guards`.
    #[test]
    fn detects_protected_hoc_at_outermost_position() {
        let g = guards_for(
            r#"
#[component]
fn BoardScreen() -> Element {
    rsx!{
        Protected {
            div { "board" }
        }
    }
}
"#,
        );
        assert_eq!(g, vec!["Protected".to_string()]);
    }

    /// Routes with no guard HOC return an empty list — leaving the field
    /// off in the serialized response thanks to the `skip_serializing_if`
    /// attribute on `RouteEntry::guards`.
    #[test]
    fn ungated_route_has_no_guards() {
        let g = guards_for(
            r#"
#[component]
fn Home() -> Element {
    rsx!{ div { "home" } }
}
"#,
        );
        assert!(g.is_empty(), "ungated route should report no guards: {g:?}");
    }

    /// Aliases beyond `Protected` (`Guarded`, `AuthGate`, `RequireAuth`,
    /// `Authenticated`, `RequiresLogin`) are detected too — these are the
    /// common-by-convention names other Dioxus projects use.
    #[test]
    fn detects_alternate_guard_aliases() {
        for alias in [
            "Guarded",
            "AuthGate",
            "RequireAuth",
            "Authenticated",
            "RequiresLogin",
        ] {
            let src = format!(
                r#"
#[component]
fn Screen() -> Element {{
    rsx!{{
        {alias} {{
            div {{ "x" }}
        }}
    }}
}}
"#
            );
            let g = guards_for(&src);
            assert_eq!(
                g,
                vec![alias.to_string()],
                "alias {alias} should be detected"
            );
        }
    }

    /// A `Protected` ident nested DEEP inside the rsx (under a `div`) does
    /// not gate route entry — only the leaves below it — so we don't list
    /// it as a route guard.
    #[test]
    fn ignores_nested_guard_inside_other_element() {
        let g = guards_for(
            r#"
#[component]
fn Demo() -> Element {
    rsx!{
        div {
            Protected {
                span { "secret" }
            }
        }
    }
}
"#,
        );
        assert!(
            g.is_empty(),
            "guard nested under a non-guard parent must not be reported: {g:?}",
        );
    }
}
