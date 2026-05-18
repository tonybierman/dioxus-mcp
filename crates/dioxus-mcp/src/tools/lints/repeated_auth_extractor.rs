//! `repeated_auth_extractor`: flag an auth-shaped helper fn that's
//! called from ≥3 distinct server fn bodies — `user_from_cookies(&cookies)`,
//! `SESSIONS.lock().get(sid)`, etc. The repetition is the signal that the
//! app wants an Axum `FromRequestParts` extractor: write the auth logic
//! once, get a `Session` / `User` type-level guarantee everywhere it's
//! needed.
//!
//! Detected shape (iter03, 6 of 8 server fns):
//!
//! ```ignore
//! #[get("/api/board", cookies: TypedHeader<Cookie>)]
//! pub async fn fetch_board() -> Result<…, ServerFnError> {
//!     if user_from_cookies(&cookies).is_none() { return Err(…); }
//!     // …
//! }
//! ```
//!
//! Detection:
//!   * For every `#[server]`/`#[get]`/`#[post]`/`#[put]`/`#[delete]`/`#[patch]`
//!     fn, walk the body collecting call-expression names.
//!   * Bucket by callee tail (`user_from_cookies`, `SESSIONS::lock`, …).
//!   * Emit a finding when the same auth-shaped callee appears in ≥3
//!     distinct server fns. "Auth-shaped" = the name (case-insensitive)
//!     contains `user`, `auth`, `session`, or `cookie` AND the bucket
//!     isn't `who_am_i` / `login` (those are the auth endpoints themselves).
//!
//! Severity `info`. The duplication isn't a correctness bug — it's a
//! review prompt: when the same identity check ships across the entire
//! server surface, the next change (rate limiting, role checks, audit
//! logging) has to land in N files. Extract once, fan it out.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::visit::Visit;

use crate::state::State;
use crate::tools::ast::{ParseError, collect_parse_errors, walk_rs_files};
use crate::tools::scaffold::crate_root;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RepeatedAuthExtractorParams {
    pub project_root: Option<String>,
    /// Minimum number of distinct server fns that must share the
    /// extractor call for a finding to emit. Default `3`; lower to `2`
    /// to surface pairs early.
    #[serde(default)]
    pub min_call_sites: Option<usize>,
}

#[derive(Debug, Serialize, Clone)]
pub struct CallSite {
    pub file: PathBuf,
    pub line: usize,
    /// Name of the enclosing server fn (`fetch_board`, `move_card`, …).
    pub server_fn: String,
}

#[derive(Debug, Serialize)]
pub struct RepeatedAuthFinding {
    pub code: &'static str,
    pub severity: &'static str,
    /// The shared helper's name. We surface the last path segment
    /// (e.g. `user_from_cookies`) so the message is readable; the full
    /// path is in each site's `file:line` if needed.
    pub callee: String,
    pub sites: Vec<CallSite>,
    pub message: String,
    pub fix: String,
}

#[derive(Debug, Serialize)]
pub struct RepeatedAuthExtractorReport {
    pub findings: Vec<RepeatedAuthFinding>,
    pub parse_errors: Vec<ParseError>,
}

pub async fn repeated_auth_extractor(
    state: &Arc<State>,
    p: RepeatedAuthExtractorParams,
) -> Result<RepeatedAuthExtractorReport, String> {
    let root = crate_root(state, p.project_root.as_deref()).await?;
    let src_root = root.join("src");
    let files = walk_rs_files(&src_root);
    let min_sites = p.min_call_sites.unwrap_or(3).max(2);

    // callee_name -> [CallSite]; one entry per (server_fn, callee) pair
    // so a fn that hits the same extractor twice doesn't inflate the
    // bucket.
    let mut buckets: HashMap<String, Vec<CallSite>> = HashMap::new();
    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
        for item in &ast.items {
            let syn::Item::Fn(f) = item else { continue };
            if !is_server_fn(&f.attrs) {
                continue;
            }
            let mut collector = CallCollector::default();
            collector.visit_block(&f.block);
            let mut seen_here: std::collections::HashSet<String> = std::collections::HashSet::new();
            for call in &collector.calls {
                if !is_auth_shaped(&call.callee) {
                    continue;
                }
                if seen_here.insert(call.callee.clone()) {
                    buckets
                        .entry(call.callee.clone())
                        .or_default()
                        .push(CallSite {
                            file: sf.path.clone(),
                            line: call.line,
                            server_fn: f.sig.ident.to_string(),
                        });
                }
            }
        }
    }

    let mut findings: Vec<RepeatedAuthFinding> = Vec::new();
    for (callee, sites) in buckets {
        if sites.len() < min_sites {
            continue;
        }
        if is_auth_endpoint_name(&callee) {
            // `login`, `who_am_i`, `logout` etc. — those ARE the auth
            // endpoints; their call sites aren't duplication, they're
            // the API surface.
            continue;
        }
        let n = sites.len();
        let summary: Vec<String> = sites
            .iter()
            .map(|s| format!("{}:{} ({})", s.file.display(), s.line, s.server_fn))
            .collect();
        findings.push(RepeatedAuthFinding {
            code: "repeated_auth_extractor",
            severity: "info",
            callee: callee.clone(),
            sites: sites.clone(),
            message: format!(
                "`{callee}(…)` is called from {n} server fns: {locs}. The same identity \
                 check is repeated across the server surface — when the next change \
                 ships (rate limit, role check, audit log) it has to land in {n} files.",
                locs = summary.join(", "),
            ),
            fix: format!(
                "Extract an Axum extractor: write `pub struct AuthUser(pub String);` in \
                 `src/server/auth.rs` with `impl FromRequestParts<S> for AuthUser` doing \
                 the same `{callee}` work, then declare server fns as e.g. \
                 `#[get(\"/api/board\", user: AuthUser)] pub async fn fetch_board(user: \
                 AuthUser) -> …`. The extractor runs once, returns a typed handle, and \
                 every endpoint that needs auth gets it for free."
            ),
        });
    }

    findings.sort_by(|a, b| a.callee.cmp(&b.callee));

    Ok(RepeatedAuthExtractorReport {
        findings,
        parse_errors: collect_parse_errors(&files),
    })
}

fn is_server_fn(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        let last = a.path().segments.last().map(|s| s.ident.to_string());
        matches!(
            last.as_deref(),
            Some("server" | "get" | "post" | "put" | "delete" | "patch")
        )
    })
}

#[derive(Default)]
struct CallCollector {
    calls: Vec<RecordedCall>,
}

struct RecordedCall {
    callee: String,
    line: usize,
}

impl<'ast> Visit<'ast> for CallCollector {
    fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
        if let syn::Expr::Path(p) = &*c.func
            && let Some(last) = p.path.segments.last()
        {
            self.calls.push(RecordedCall {
                callee: last.ident.to_string(),
                line: last.ident.span().start().line,
            });
        }
        syn::visit::visit_expr_call(self, c);
    }
    fn visit_expr_method_call(&mut self, mc: &'ast syn::ExprMethodCall) {
        // Only bucket method calls when the receiver is a plain path —
        // `cookies.get(…)`, `session.read()`. Chains and complex
        // receivers create noisy duplicate buckets and we'd rather
        // miss the long-tail than double-count the headline case.
        if let Some(receiver) = path_receiver_ident(&mc.receiver) {
            let key = format!("{}.{}", receiver, mc.method);
            self.calls.push(RecordedCall {
                callee: key,
                line: mc.method.span().start().line,
            });
        }
        syn::visit::visit_expr_method_call(self, mc);
    }
}

fn path_receiver_ident(e: &syn::Expr) -> Option<String> {
    let p = match e {
        syn::Expr::Path(p) => p,
        syn::Expr::Reference(r) => return path_receiver_ident(&r.expr),
        _ => return None,
    };
    // A single ident — not a multi-segment path. `cookies` qualifies;
    // `crate::server::state::SESSIONS` doesn't (we'd want a different
    // bucket strategy for that).
    if p.path.segments.len() == 1 {
        Some(p.path.segments[0].ident.to_string())
    } else {
        None
    }
}

fn is_auth_shaped(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("user")
        || lower.contains("auth")
        || lower.contains("session")
        || lower.contains("cookie")
}

fn is_auth_endpoint_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "who_am_i"
            | "whoami"
            | "login"
            | "login_user"
            | "logout"
            | "logout_user"
            | "sign_in"
            | "sign_out"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn run(files: &[(&str, &str)]) -> RepeatedAuthExtractorReport {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src");
        for (rel, content) in files {
            let p = src.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        let scanned = walk_rs_files(&src);
        let mut buckets: HashMap<String, Vec<CallSite>> = HashMap::new();
        for sf in &scanned {
            let Ok(ast) = &sf.ast else { continue };
            for item in &ast.items {
                let syn::Item::Fn(f) = item else { continue };
                if !is_server_fn(&f.attrs) {
                    continue;
                }
                let mut c = CallCollector::default();
                c.visit_block(&f.block);
                let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                for call in &c.calls {
                    if !is_auth_shaped(&call.callee) {
                        continue;
                    }
                    if seen.insert(call.callee.clone()) {
                        buckets
                            .entry(call.callee.clone())
                            .or_default()
                            .push(CallSite {
                                file: sf.path.clone(),
                                line: call.line,
                                server_fn: f.sig.ident.to_string(),
                            });
                    }
                }
            }
        }
        let mut findings = Vec::new();
        for (callee, sites) in buckets {
            if sites.len() < 3 || is_auth_endpoint_name(&callee) {
                continue;
            }
            findings.push(RepeatedAuthFinding {
                code: "repeated_auth_extractor",
                severity: "info",
                callee,
                sites,
                message: String::new(),
                fix: String::new(),
            });
        }
        RepeatedAuthExtractorReport {
            findings,
            parse_errors: collect_parse_errors(&scanned),
        }
    }

    /// iter03 shape: `user_from_cookies(&cookies)` is called from 3+
    /// server fns. Must fire on that bucket.
    #[test]
    fn flags_user_from_cookies_repetition() {
        let f1 = r#"
#[get("/api/board")]
async fn fetch_board() -> Result<(), ()> {
    if user_from_cookies(&cookies).is_none() { return Err(()); }
    Ok(())
}
"#;
        let f2 = r#"
#[post("/api/move")]
async fn move_card() -> Result<(), ()> {
    if user_from_cookies(&cookies).is_none() { return Err(()); }
    Ok(())
}
"#;
        let f3 = r#"
#[delete("/api/delete")]
async fn delete_card() -> Result<(), ()> {
    if user_from_cookies(&cookies).is_none() { return Err(()); }
    Ok(())
}
"#;
        let r = run(&[("a.rs", f1), ("b.rs", f2), ("c.rs", f3)]);
        assert_eq!(r.findings.len(), 1, "{r:?}");
        assert_eq!(r.findings[0].callee, "user_from_cookies");
        assert_eq!(r.findings[0].sites.len(), 3);
    }

    /// 2 server fns sharing the extractor is below the default
    /// threshold — pair-level duplication isn't the signal yet.
    #[test]
    fn silent_under_threshold() {
        let f1 = r#"
#[get("/a")]
async fn a() -> Result<(), ()> {
    if user_from_cookies(&cookies).is_none() { return Err(()); }
    Ok(())
}
"#;
        let f2 = r#"
#[get("/b")]
async fn b() -> Result<(), ()> {
    if user_from_cookies(&cookies).is_none() { return Err(()); }
    Ok(())
}
"#;
        let r = run(&[("a.rs", f1), ("b.rs", f2)]);
        assert!(r.findings.is_empty(), "{r:?}");
    }

    /// `who_am_i` and `login` use the same call but are exempt — those
    /// ARE the auth endpoints.
    #[test]
    fn silent_for_auth_endpoint_names() {
        let f1 = r#"
#[get("/login")]
async fn login() -> Result<(), ()> {
    let _ = login(); Ok(())
}
"#;
        let f2 = r#"
#[get("/who")]
async fn who_am_i() -> Result<(), ()> {
    let _ = login(); Ok(())
}
"#;
        let f3 = r#"
#[get("/x")]
async fn x() -> Result<(), ()> {
    let _ = login(); Ok(())
}
"#;
        // 3 sites, but the bucket name `login` is exempt.
        let r = run(&[("a.rs", f1), ("b.rs", f2), ("c.rs", f3)]);
        assert!(r.findings.is_empty(), "{r:?}");
    }
}
