//! `auth_map`: one-shot answer to "which routes and server fns require
//! authentication, and which don't?"
//!
//! Cross-references two existing audits:
//!   - `route_map` for `routes[].guards` (route-component HOCs).
//!   - `project_index` for server-fn signatures (cookie-extractor detection).
//!
//! Returns a flat list per surface plus a `mismatches` block calling out
//! likely gaps — a route that's gated client-side but whose backing server
//! fn isn't, or vice-versa. A clean report means client and server agree
//! about which slices need a session.

use std::path::PathBuf;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::state::State;
use crate::tools::inspect::project_index::{ProjectIndexParams, project_index};
use crate::tools::inspect::route_map::{RouteMapParams, route_map};

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AuthMapParams {
    /// Absolute path to the Dioxus project root. Defaults to the cwd the
    /// MCP server was started in.
    pub project_root: Option<String>,
    /// Forwarded to `route_map`; usually unset.
    pub router_file: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RouteAuth {
    pub component: String,
    pub full_path: String,
    /// Auth-guard HOCs the route component wraps in, if any. Empty list
    /// means the route is unguarded at the component level.
    pub guards: Vec<String>,
    /// `true` iff `guards` is non-empty. Surfaced as a top-level boolean so
    /// callers can filter quickly without iterating the list.
    pub gated: bool,
}

#[derive(Debug, Serialize)]
pub struct ServerFnAuth {
    pub name: String,
    /// `method:path` route the server fn is mounted on, when available.
    /// Falls back to the bare function name for legacy `#[server]` shapes.
    pub route: String,
    pub file: PathBuf,
    pub line: usize,
    /// `true` iff the signature OR the verb-macro attribute includes a
    /// `cookies:` extractor (or any arg typed `TypedHeader<…Cookie…>`). The
    /// convention in 0.7-fullstack apps is that the handler body then does
    /// `user_from_cookies(&cookies)` and rejects unauthenticated calls.
    pub cookie_gated: bool,
}

#[derive(Debug, Serialize)]
pub struct AuthMismatch {
    pub kind: &'static str,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct AuthMapReport {
    pub routes: Vec<RouteAuth>,
    pub server_fns: Vec<ServerFnAuth>,
    /// Headline counts so callers can grep one number to answer
    /// "is anything gated at all?".
    pub gated_route_count: usize,
    pub gated_server_fn_count: usize,
    /// Likely client/server auth-gap reports. Empty when every gated route
    /// has cookie-gated server fns and vice-versa.
    pub mismatches: Vec<AuthMismatch>,
}

pub async fn auth_map(state: &Arc<State>, p: AuthMapParams) -> Result<AuthMapReport, String> {
    let routes_report = route_map(
        state,
        RouteMapParams {
            router_file: p.router_file.clone(),
            project_root: p.project_root.clone(),
        },
    )
    .await?;
    let index = project_index(
        state,
        ProjectIndexParams {
            path: None,
            kind: None,
            project_root: p.project_root.clone(),
        },
    )
    .await?;

    let routes: Vec<RouteAuth> = routes_report
        .routes
        .iter()
        .map(|r| RouteAuth {
            component: r.component.clone(),
            full_path: r.full_path.clone(),
            guards: r.guards.clone(),
            gated: !r.guards.is_empty(),
        })
        .collect();
    let gated_route_count = routes.iter().filter(|r| r.gated).count();

    let server_fns: Vec<ServerFnAuth> = index
        .server_fns
        .iter()
        .map(|sf| ServerFnAuth {
            name: sf.name.clone(),
            route: server_fn_route(sf),
            file: sf.file.clone(),
            line: sf.line,
            cookie_gated: sf.is_cookie_gated(),
        })
        .collect();
    let gated_server_fn_count = server_fns.iter().filter(|s| s.cookie_gated).count();

    // Heuristic mismatch detection: a gated route exists but NO server fn is
    // cookie-gated. Inverse: cookie-gated server fns exist but NO route is
    // guarded. Neither flag is necessarily a bug — purely client-side apps
    // gate routes only, and B2B APIs gate server fns without a UI surface —
    // but both extremes warrant a reviewer's attention.
    let mut mismatches: Vec<AuthMismatch> = Vec::new();
    if gated_route_count > 0 && gated_server_fn_count == 0 {
        mismatches.push(AuthMismatch {
            kind: "routes_gated_server_fns_not",
            message: format!(
                "{gated_route_count} route(s) wrap in an auth HOC but no server fn takes a \
                 `cookies:` extractor — the gate is cosmetic, callers can hit the API directly. \
                 Add `cookies: TypedHeader<Cookie>` to the gated handlers and check \
                 `user_from_cookies` before returning data."
            ),
        });
    }
    if gated_server_fn_count > 0 && gated_route_count == 0 && !routes.is_empty() {
        mismatches.push(AuthMismatch {
            kind: "server_fns_gated_routes_not",
            message: format!(
                "{gated_server_fn_count} server fn(s) check cookies but no route wraps in an \
                 auth HOC — anonymous visitors will hit the gated handler and see the bare \
                 `user_from_cookies` failure path. Wrap the relevant route components in \
                 `Protected` (or your project's equivalent) so the client redirects to login \
                 before the request fires."
            ),
        });
    }

    Ok(AuthMapReport {
        routes,
        server_fns,
        gated_route_count,
        gated_server_fn_count,
        mismatches,
    })
}

fn server_fn_route(sf: &crate::tools::inspect::project_index::ServerFnEntry) -> String {
    match (sf.method.as_deref(), sf.route_path.as_deref()) {
        (Some(m), Some(p)) => format!("{}: {p}", m.to_uppercase()),
        (None, Some(p)) => p.to_string(),
        _ => sf.name.clone(),
    }
}
