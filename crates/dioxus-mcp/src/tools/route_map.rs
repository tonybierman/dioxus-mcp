use std::path::PathBuf;
use std::sync::Arc;

use quote::ToTokens;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::state::State;
use crate::tools::scaffold::{crate_root, find_routable, has_derive};
use crate::tools::tighten_type;

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
    /// Line number of the `#[route(...)]` attribute in the source file.
    pub line: usize,
}

#[derive(Debug, Serialize)]
pub struct RouteMapReport {
    pub file: PathBuf,
    pub enum_name: String,
    pub routes: Vec<RouteEntry>,
}

pub async fn route_map(
    state: &Arc<State>,
    p: RouteMapParams,
) -> Result<RouteMapReport, String> {
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
        None => find_routable(&crate_root).ok_or_else(|| {
            "could not find a Routable enum in src/; pass router_file".to_string()
        })?,
    };

    let src = std::fs::read_to_string(&router_file)
        .map_err(|e| format!("read {}: {e}", router_file.display()))?;
    let file = syn::parse_file(&src).map_err(|e| format!("parse: {e}"))?;

    let routable_enum = file
        .items
        .iter()
        .find_map(|it| match it {
            syn::Item::Enum(e) if e.attrs.iter().any(|a| has_derive(a, "Routable")) => Some(e),
            _ => None,
        })
        .ok_or_else(|| {
            format!("no `#[derive(Routable)]` enum in {}", router_file.display())
        })?;

    let enum_name = routable_enum.ident.to_string();
    let mut layout_stack: Vec<String> = Vec::new();
    let mut nest_stack: Vec<String> = Vec::new();
    let mut routes: Vec<RouteEntry> = Vec::new();

    for variant in &routable_enum.variants {
        let mut route_for_variant: Option<(String, usize)> = None;

        for attr in &variant.attrs {
            let path = attr.path();
            if path.is_ident("layout") {
                if let Ok(p) = attr.parse_args::<syn::Path>() {
                    if let Some(seg) = p.segments.last() {
                        layout_stack.push(seg.ident.to_string());
                    }
                }
            } else if path.is_ident("end_layout") {
                layout_stack.pop();
            } else if path.is_ident("nest") {
                if let Ok(lit) = attr.parse_args::<syn::LitStr>() {
                    nest_stack.push(lit.value());
                }
            } else if path.is_ident("end_nest") {
                nest_stack.pop();
            } else if path.is_ident("route") {
                if let Ok(lit) = attr.parse_args::<syn::LitStr>() {
                    let line = attr.path().segments.first().map(|s| s.ident.span().start().line).unwrap_or(0);
                    route_for_variant = Some((lit.value(), line));
                }
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
            line,
        });
    }

    Ok(RouteMapReport {
        file: router_file,
        enum_name,
        routes,
    })
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
