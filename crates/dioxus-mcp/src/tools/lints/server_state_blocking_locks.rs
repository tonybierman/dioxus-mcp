//! `server_state_blocking_locks`: flag `std::sync::Mutex` / `RwLock` /
//! `parking_lot::Mutex` etc. used from inside `async` server fn bodies.
//!
//! The pattern itself isn't a bug — short, fully-synchronous critical
//! sections under `std::sync` are fine — but the moment someone adds an
//! `.await` while holding the guard, the runtime thread is blocked for the
//! duration of the await. The Tokio docs explicitly call this out, but the
//! foot-gun is invisible until someone actually wires in the await.
//!
//! The lint is precautionary: it surfaces the call site so reviewers can
//! decide whether to migrate to `tokio::sync::Mutex` (lock-across-await
//! safe) or assert the critical section is sync. Findings stay at
//! `confidence: low` because the pattern is correct in many real apps.

use std::path::PathBuf;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::visit::Visit;

use crate::state::State;
use crate::tools::ast::{ParseError, collect_parse_errors, walk_rs_files};
use crate::tools::scaffold::crate_root;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ServerStateBlockingLocksParams {
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BlockingLockFinding {
    pub code: &'static str,
    pub file: PathBuf,
    pub line: usize,
    /// The server-fn name that contains the lock call site.
    pub server_fn: String,
    /// The actual method name observed — `lock`, `read`, `write`, `try_lock`.
    pub method: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct ServerStateBlockingLocksReport {
    pub findings: Vec<BlockingLockFinding>,
    pub parse_errors: Vec<ParseError>,
}

pub async fn server_state_blocking_locks(
    state: &Arc<State>,
    p: ServerStateBlockingLocksParams,
) -> Result<ServerStateBlockingLocksReport, String> {
    let root = crate_root(state, p.project_root.as_deref()).await?;
    let src_root = root.join("src");
    let files = walk_rs_files(&src_root);

    let mut findings: Vec<BlockingLockFinding> = Vec::new();
    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
        for item in &ast.items {
            let syn::Item::Fn(f) = item else { continue };
            if !is_server_fn(f) || f.sig.asyncness.is_none() {
                continue;
            }
            let mut v = BlockingLockVisitor {
                hits: Vec::new(),
                in_tokio_spawn_blocking: 0,
            };
            v.visit_block(&f.block);
            for (method, line) in v.hits {
                findings.push(BlockingLockFinding {
                    code: "server_state_blocking_locks",
                    file: sf.path.clone(),
                    line,
                    server_fn: f.sig.ident.to_string(),
                    method: method.clone(),
                    message: format!(
                        "`.{method}()` on a `std::sync` lock from inside an `async` server fn \
                         body. Currently safe if the critical section stays fully sync — but \
                         the first `.await` added while holding the guard will block the Tokio \
                         worker. Either switch to `tokio::sync::Mutex` / `RwLock` (lock-across-\
                         await safe) or assert the section stays sync (a comment + a manual \
                         `drop(guard)` before any await keeps the invariant visible)."
                    ),
                });
            }
        }
    }

    Ok(ServerStateBlockingLocksReport {
        findings,
        parse_errors: collect_parse_errors(&files),
    })
}

/// True when `f` has one of the server-fn attribute shapes: `#[server]`
/// (legacy form) OR one of the HTTP-method attributes `#[get|post|put|
/// delete|patch(...)]` (0.7 attribute-style fullstack form).
fn is_server_fn(f: &syn::ItemFn) -> bool {
    f.attrs.iter().any(|a| {
        let Some(last) = a.path().segments.last() else {
            return false;
        };
        let s = last.ident.to_string();
        matches!(
            s.as_str(),
            "server" | "get" | "post" | "put" | "delete" | "patch"
        )
    })
}

struct BlockingLockVisitor {
    /// (method-name, source line) for every blocking-lock call site we found.
    hits: Vec<(String, usize)>,
    /// Depth into a `tokio::task::spawn_blocking { … }` / `task::spawn_blocking(…)`
    /// call. Anything inside such a closure is allowed to use sync locks
    /// without warning — that's exactly the escape hatch the lint is meant
    /// to point users at.
    in_tokio_spawn_blocking: u32,
}

impl<'ast> Visit<'ast> for BlockingLockVisitor {
    fn visit_expr_method_call(&mut self, mc: &'ast syn::ExprMethodCall) {
        // Only `lock` / `read` / `write` / `try_lock` count. `try_read` /
        // `try_write` for `RwLock` are also in the set — they don't block
        // currently but they DO return a guard, and the same await-while-
        // holding problem applies.
        let method = mc.method.to_string();
        if matches!(
            method.as_str(),
            "lock" | "read" | "write" | "try_lock" | "try_read" | "try_write"
        ) && self.in_tokio_spawn_blocking == 0
        // Heuristic: the receiver is a path or method that's plausibly a
        // sync lock. We don't have a type resolver, so we accept any call
        // shape and let the message tell the reviewer to confirm.
        // `parking_lot` and `std::sync` are the two stdlib-adjacent
        // offenders; `tokio::sync::Mutex::lock` is also a method call but
        // it returns a future, so anyone with `.lock().await` is fine —
        // they trigger this lint but the suggestion ("switch to
        // tokio::sync") will be a no-op, which the reviewer can dismiss.
        {
            self.hits.push((method, mc.method.span().start().line));
        }
        syn::visit::visit_expr_method_call(self, mc);
    }
    fn visit_expr_call(&mut self, ec: &'ast syn::ExprCall) {
        // Detect `tokio::task::spawn_blocking(...)` so its body doesn't
        // trigger the lint. We bump the depth around the call's argument walk.
        let is_spawn_blocking = if let syn::Expr::Path(p) = &*ec.func {
            p.path
                .segments
                .last()
                .map(|s| s.ident == "spawn_blocking")
                .unwrap_or(false)
        } else {
            false
        };
        if is_spawn_blocking {
            self.in_tokio_spawn_blocking += 1;
            syn::visit::visit_expr_call(self, ec);
            self.in_tokio_spawn_blocking -= 1;
        } else {
            syn::visit::visit_expr_call(self, ec);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn scan(src: &str) -> Vec<BlockingLockFinding> {
        let dir = TempDir::new().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("server.rs"), src).unwrap();

        let files = walk_rs_files(&src_dir);
        let mut findings = Vec::new();
        for sf in &files {
            let Ok(ast) = &sf.ast else { continue };
            for item in &ast.items {
                let syn::Item::Fn(f) = item else { continue };
                if !is_server_fn(f) || f.sig.asyncness.is_none() {
                    continue;
                }
                let mut v = BlockingLockVisitor {
                    hits: Vec::new(),
                    in_tokio_spawn_blocking: 0,
                };
                v.visit_block(&f.block);
                for (method, line) in v.hits {
                    findings.push(BlockingLockFinding {
                        code: "server_state_blocking_locks",
                        file: sf.path.clone(),
                        line,
                        server_fn: f.sig.ident.to_string(),
                        method: method.clone(),
                        message: String::new(),
                    });
                }
            }
        }
        findings
    }

    /// Canonical foot-gun: `STATE.lock()` from inside `#[server] async fn`.
    /// Currently sync and safe, but flag so reviewers see it before someone
    /// adds the first `.await`.
    #[test]
    fn flags_std_sync_mutex_lock_in_async_server_fn() {
        let findings = scan(
            r#"use std::sync::Mutex;
static STATE: Mutex<Vec<u32>> = Mutex::new(Vec::new());

#[server]
pub async fn count() -> Result<u32, ServerFnError> {
    let g = STATE.lock().unwrap();
    Ok(g.len() as u32)
}
"#,
        );
        assert_eq!(
            findings.len(),
            1,
            "should flag the .lock() call: {findings:?}"
        );
        assert_eq!(findings[0].server_fn, "count");
        assert_eq!(findings[0].method, "lock");
    }

    /// `RwLock::read` / `RwLock::write` both count.
    #[test]
    fn flags_rwlock_read_and_write() {
        let findings = scan(
            r#"use std::sync::RwLock;
static STATE: RwLock<Vec<u32>> = RwLock::new(Vec::new());

#[get("/api/state")]
pub async fn handler() -> Result<u32, ServerFnError> {
    let r = STATE.read().unwrap();
    let mut w = STATE.write().unwrap();
    let _ = (r, w);
    Ok(0)
}
"#,
        );
        let methods: Vec<&str> = findings.iter().map(|f| f.method.as_str()).collect();
        assert!(
            methods.contains(&"read"),
            "RwLock::read must flag: {methods:?}"
        );
        assert!(
            methods.contains(&"write"),
            "RwLock::write must flag: {methods:?}"
        );
    }

    /// Calls inside `tokio::task::spawn_blocking` are the recommended escape
    /// hatch — don't flag those. The blocking section runs on the blocking
    /// pool, not a worker thread.
    #[test]
    fn does_not_flag_inside_spawn_blocking() {
        let findings = scan(
            r#"use std::sync::Mutex;
static STATE: Mutex<Vec<u32>> = Mutex::new(Vec::new());

#[server]
pub async fn handler() -> Result<u32, ServerFnError> {
    let n = tokio::task::spawn_blocking(|| {
        let g = STATE.lock().unwrap();
        g.len() as u32
    }).await.unwrap();
    Ok(n)
}
"#,
        );
        assert!(
            findings.is_empty(),
            "spawn_blocking {{ ... }} body is the safe escape hatch: {findings:?}"
        );
    }

    /// Sync (non-async) functions don't count — the lint is specifically
    /// about Tokio-worker blocking, which only happens in async contexts.
    #[test]
    fn does_not_flag_sync_function() {
        let findings = scan(
            r#"use std::sync::Mutex;
static STATE: Mutex<Vec<u32>> = Mutex::new(Vec::new());

pub fn sync_helper() -> u32 {
    let g = STATE.lock().unwrap();
    g.len() as u32
}
"#,
        );
        assert!(
            findings.is_empty(),
            "sync helpers without #[server]/#[get/etc.] should not trigger: {findings:?}"
        );
    }

    /// Functions without a server-fn attribute don't count either —
    /// arbitrary helpers can use sync locks however they like.
    #[test]
    fn does_not_flag_arbitrary_async_function() {
        let findings = scan(
            r#"use std::sync::Mutex;
static STATE: Mutex<Vec<u32>> = Mutex::new(Vec::new());

pub async fn helper() -> u32 {
    let g = STATE.lock().unwrap();
    g.len() as u32
}
"#,
        );
        assert!(
            findings.is_empty(),
            "no server-fn attribute → out of scope: {findings:?}"
        );
    }
}
