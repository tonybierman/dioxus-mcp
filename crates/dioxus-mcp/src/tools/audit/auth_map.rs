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
use crate::tools::ast::walk_rs_files;
use crate::tools::inspect::project_index::{ProjectIndexParams, project_index};
use crate::tools::inspect::route_map::{RouteMapParams, route_map};
use crate::tools::scaffold::crate_root;
use syn::visit::Visit;

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
pub struct UnwrapOrDefaultHint {
    pub server_fn: String,
    pub file: PathBuf,
    pub line: usize,
    /// String the `.get(...)` call resolved against — typically `"sid"`,
    /// `"session"`, `"authorization"` etc. Surfaced so reviewers can
    /// confirm which header is being defaulted.
    pub key: String,
    pub confidence: &'static str,
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
    /// Low-confidence hints: server fns that call `.unwrap_or_default()`
    /// on a security header value (a cookie or `authorization` lookup).
    /// In practice the `""` branch is unreachable when the handler is
    /// already gated upstream by `user_from_cookies`, but the pattern is
    /// fragile — a refactor that drops the gate would silently turn the
    /// handler into one that accepts anonymous requests.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unwrap_or_default_hints: Vec<UnwrapOrDefaultHint>,
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

    let unwrap_or_default_hints =
        collect_unwrap_or_default_hints(state, p.project_root.as_deref()).await?;

    Ok(AuthMapReport {
        routes,
        server_fns,
        gated_route_count,
        gated_server_fn_count,
        mismatches,
        unwrap_or_default_hints,
    })
}

/// Walk every server fn body looking for `.unwrap_or_default()` chained
/// onto a security-header lookup. The detection is intentionally narrow:
///
/// * The receiver is `<ident>.get("...")` where `<ident>` matches a known
///   header / cookie binding (`cookies`, `headers`, `cookie_jar`, …) OR
///   the call chain bottoms out at `cookies` / `cookie_jar`.
/// * The key string matches a known security header (`sid`, `session`,
///   `auth`, `authorization`, `bearer`, …) or contains a session keyword.
///
/// We don't try to prove unreachability of the default branch — that
/// would require a control-flow analysis. The hint is `low` confidence by
/// construction so the reviewer can confirm or dismiss.
async fn collect_unwrap_or_default_hints(
    state: &Arc<State>,
    project_root: Option<&str>,
) -> Result<Vec<UnwrapOrDefaultHint>, String> {
    let root = crate_root(state, project_root).await?;
    let src_root = root.join("src");
    let files = walk_rs_files(&src_root);

    let mut hints: Vec<UnwrapOrDefaultHint> = Vec::new();
    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
        for item in &ast.items {
            let syn::Item::Fn(f) = item else { continue };
            if !is_server_fn(f) {
                continue;
            }
            let server_fn_name = f.sig.ident.to_string();
            let mut v = UnwrapVisitor {
                file: sf.path.clone(),
                server_fn: server_fn_name,
                hits: Vec::new(),
            };
            v.visit_block(&f.block);
            hints.extend(v.hits);
        }
    }
    hints.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.line.cmp(&b.line))
            .then(a.server_fn.cmp(&b.server_fn))
    });
    Ok(hints)
}

fn is_server_fn(f: &syn::ItemFn) -> bool {
    f.attrs.iter().any(|a| {
        let last = a.path().segments.last().map(|s| s.ident.to_string());
        matches!(
            last.as_deref(),
            Some("server" | "get" | "post" | "put" | "delete" | "patch")
        )
    })
}

struct UnwrapVisitor {
    file: PathBuf,
    server_fn: String,
    hits: Vec<UnwrapOrDefaultHint>,
}

impl<'ast> Visit<'ast> for UnwrapVisitor {
    fn visit_expr_method_call(&mut self, e: &'ast syn::ExprMethodCall) {
        if e.method == "unwrap_or_default" || e.method == "unwrap_or" {
            // The chain we're after: `<root>.get("key").unwrap_or_default()`.
            // `e.receiver` is the `.get(...)` call; walk down to find the
            // root ident and the key.
            if let syn::Expr::MethodCall(inner) = &*e.receiver
                && inner.method == "get"
            {
                let root_ident = root_path_ident(&inner.receiver);
                let key = inner.args.first().and_then(|a| match a {
                    syn::Expr::Lit(l) => match &l.lit {
                        syn::Lit::Str(s) => Some(s.value()),
                        _ => None,
                    },
                    _ => None,
                });
                if let (Some(root), Some(key)) = (root_ident, key)
                    && receiver_is_security_binding(&root)
                    && key_is_security_header(&key)
                {
                    self.hits.push(UnwrapOrDefaultHint {
                        server_fn: self.server_fn.clone(),
                        file: self.file.clone(),
                        line: e.method.span().start().line,
                        key: key.clone(),
                        confidence: "low",
                        message: format!(
                            "`{root}.get({key:?}).{method}()` defaults a security header value \
                             to the empty string. If an upstream `user_from_cookies` gate is in \
                             place this branch is unreachable, but the pattern is fragile — a \
                             refactor that drops the gate would silently let anonymous requests \
                             through. Replace the unwrap with an explicit `match` (or a `?` on \
                             an `Option`-returning helper) so the missing-header path is a \
                             compile-time-tracked error path.",
                            root = root,
                            key = key,
                            method = e.method,
                        ),
                    });
                }
            }
        }
        syn::visit::visit_expr_method_call(self, e);
    }
}

fn root_path_ident(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Path(p) if p.path.segments.len() == 1 => {
            Some(p.path.segments[0].ident.to_string())
        }
        syn::Expr::Paren(p) => root_path_ident(&p.expr),
        syn::Expr::Reference(r) => root_path_ident(&r.expr),
        syn::Expr::MethodCall(mc) => root_path_ident(&mc.receiver),
        _ => None,
    }
}

fn receiver_is_security_binding(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "cookies" | "cookie_jar" | "jar" | "headers" | "header_map" | "auth"
    ) || lower.contains("cookie")
        || lower.contains("header")
}

fn key_is_security_header(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "sid"
            | "session"
            | "session_id"
            | "sessionid"
            | "auth"
            | "authorization"
            | "bearer"
            | "csrf"
            | "csrf_token"
            | "csrftoken"
            | "x-auth-token"
    ) || lower.contains("session")
        || lower.contains("token")
        || lower.contains("auth")
}

fn server_fn_route(sf: &crate::tools::inspect::project_index::ServerFnEntry) -> String {
    match (sf.method.as_deref(), sf.route_path.as_deref()) {
        (Some(m), Some(p)) => format!("{}: {p}", m.to_uppercase()),
        (None, Some(p)) => p.to_string(),
        _ => sf.name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> Vec<UnwrapOrDefaultHint> {
        let file: syn::File = syn::parse_str(src).unwrap();
        let mut hints: Vec<UnwrapOrDefaultHint> = Vec::new();
        for item in &file.items {
            let syn::Item::Fn(f) = item else { continue };
            if !is_server_fn(f) {
                continue;
            }
            let mut v = UnwrapVisitor {
                file: PathBuf::from("ping.rs"),
                server_fn: f.sig.ident.to_string(),
                hits: Vec::new(),
            };
            v.visit_block(&f.block);
            hints.extend(v.hits);
        }
        hints
    }

    /// iter03's `ping_presence` shape: `cookies.get("sid").unwrap_or_default().to_string()`
    /// after a `user_from_cookies` gate. Must surface as a low-confidence
    /// hint.
    #[test]
    fn flags_cookie_unwrap_or_default() {
        let hints = run(r#"#[post("/api/ping", cookies: TypedHeader<Cookie>)]
async fn ping_presence(name: String) -> Result<(), ServerFnError> {
    let sid = cookies.get("sid").unwrap_or_default().to_string();
    let _ = sid;
    Ok(())
}
"#);
        assert_eq!(hints.len(), 1, "must fire: {hints:?}");
        assert_eq!(hints[0].server_fn, "ping_presence");
        assert_eq!(hints[0].key, "sid");
        assert_eq!(hints[0].confidence, "low");
    }

    /// A `.get("theme").unwrap_or_default()` on a cookie jar isn't a
    /// security default — the `theme` key isn't a session header. Must
    /// stay silent.
    #[test]
    fn ignores_unwrap_on_non_security_key() {
        let hints = run(r#"#[get("/api/pref", cookies: TypedHeader<Cookie>)]
async fn get_pref() -> Result<(), ServerFnError> {
    let theme = cookies.get("theme").unwrap_or_default().to_string();
    let _ = theme;
    Ok(())
}
"#);
        assert!(
            hints.is_empty(),
            "non-security key must not fire: {hints:?}"
        );
    }

    /// `unwrap_or` with a non-default sentinel — also flagged. Same
    /// pattern, just a different default; the reviewer should still
    /// confirm the upstream gate.
    #[test]
    fn flags_unwrap_or_with_explicit_default() {
        let hints = run(r#"#[get("/api/me", cookies: TypedHeader<Cookie>)]
async fn me() -> Result<(), ServerFnError> {
    let sid = cookies.get("sid").unwrap_or("");
    let _ = sid;
    Ok(())
}
"#);
        assert_eq!(hints.len(), 1, "unwrap_or also counts: {hints:?}");
    }

    /// Not a server fn — even if it has the same shape inside its body,
    /// the lint should not fire on helper code.
    #[test]
    fn ignores_helper_fns() {
        let hints = run(r#"fn helper(cookies: &HeaderMap) -> String {
    cookies.get("sid").unwrap_or_default().to_string()
}
"#);
        assert!(hints.is_empty(), "helper fn must not fire: {hints:?}");
    }
}
