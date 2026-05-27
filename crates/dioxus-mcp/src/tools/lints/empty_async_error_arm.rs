//! `empty_async_error_arm`: flag an `Err(_) => {}` arm (or
//! `if let Err(_) = … { }` with an empty body) inside an `async` context
//! — `use_future(|| async move { … })`, `spawn(async move { … })`,
//! `async fn`, or any inline `async { … }` block.
//!
//! Why it's a problem: in a polling loop the user-visible UI never knows
//! the network broke. The loop keeps spinning, the status stays "live",
//! the user sees stale data. iter03's `ping_presence` heartbeat at
//! `board_screen.rs:55-57` hits this verbatim — `Err(_) => {}` silently
//! swallows a server-fn failure.
//!
//! Fix is mechanical: either propagate (`Err(e) => return Err(e)`),
//! surface to the user (`status.set(Some(format!("…")))` is exactly what
//! `fetch_board`'s Ok/Err arms already do in the same file), or log.
//!
//! Detection: walk every fn body, descend into every async block / async
//! closure / async-fn body, and within that scope look for:
//!   * `match <expr> { … Err(<pat>) => { /* empty */ } … }`
//!   * `if let Err(<pat>) = <expr> { /* empty */ }`
//!
//! where the empty body is `{}`, `{ ; }`, or an explicit unit `()`.
//! The Err pattern's variable binding (`_`, `_e`, `_err`, …) is allowed
//! — the lint cares about *body* emptiness, not the binding.

use std::path::PathBuf;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::visit::Visit;

use crate::state::State;
use crate::tools::ast::{ParseError, collect_parse_errors, walk_rs_files};
use crate::tools::scaffold::crate_root;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct EmptyAsyncErrorArmParams {
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct EmptyAsyncErrorArmFinding {
    pub code: &'static str,
    pub severity: &'static str,
    pub file: PathBuf,
    pub line: usize,
    /// `match_arm` or `if_let_block` — surfacing the shape lets the fix
    /// snippet diverge per case.
    pub shape: &'static str,
    pub message: String,
    pub fix: String,
}

#[derive(Debug, Serialize)]
pub struct EmptyAsyncErrorArmReport {
    pub findings: Vec<EmptyAsyncErrorArmFinding>,
    pub parse_errors: Vec<ParseError>,
}

pub async fn empty_async_error_arm(
    state: &Arc<State>,
    p: EmptyAsyncErrorArmParams,
) -> Result<EmptyAsyncErrorArmReport, String> {
    let root = crate_root(state, p.project_root.as_deref()).await?;
    let src_root = root.join("src");
    let files = walk_rs_files(&src_root);

    let mut findings: Vec<EmptyAsyncErrorArmFinding> = Vec::new();
    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
        let mut v = AsyncContextScanner {
            file: sf.path.clone(),
            depth: 0,
            findings: &mut findings,
        };
        v.visit_file(ast);
    }
    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
    Ok(EmptyAsyncErrorArmReport {
        findings,
        parse_errors: collect_parse_errors(&files),
    })
}

/// Tracks whether the visitor is currently inside an async context.
/// Every `async fn` body, `async { … }` block, and `async |…| { … }`
/// closure increments `depth`. While `depth > 0`, the matchers below
/// fire; outside of that, errors might be deliberately swallowed.
struct AsyncContextScanner<'a> {
    file: PathBuf,
    depth: usize,
    findings: &'a mut Vec<EmptyAsyncErrorArmFinding>,
}

impl<'a, 'ast> Visit<'ast> for AsyncContextScanner<'a> {
    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        let async_fn = f.sig.asyncness.is_some();
        if async_fn {
            self.depth += 1;
        }
        syn::visit::visit_item_fn(self, f);
        if async_fn {
            self.depth -= 1;
        }
    }
    fn visit_expr_async(&mut self, ea: &'ast syn::ExprAsync) {
        self.depth += 1;
        syn::visit::visit_expr_async(self, ea);
        self.depth -= 1;
    }
    fn visit_expr_closure(&mut self, c: &'ast syn::ExprClosure) {
        let async_closure = c.asyncness.is_some();
        if async_closure {
            self.depth += 1;
        }
        syn::visit::visit_expr_closure(self, c);
        if async_closure {
            self.depth -= 1;
        }
    }

    fn visit_expr_match(&mut self, m: &'ast syn::ExprMatch) {
        if self.depth > 0 {
            for arm in &m.arms {
                if is_err_pat(&arm.pat) && is_empty_body(&arm.body) {
                    self.findings.push(EmptyAsyncErrorArmFinding {
                        code: "empty_async_error_arm",
                        severity: "warning",
                        file: self.file.clone(),
                        line: arm.fat_arrow_token.spans[0].start().line,
                        shape: "match_arm",
                        message: "An `Err(_) => {}` arm inside an async context silently \
                                  drops the failure. Polling loops keep spinning, the UI \
                                  never sees the broken server, and stale data looks \
                                  current."
                            .into(),
                        fix: "Surface the error to a status signal \
                              (`status.set(Some(format!(\"…: {e}\")))`), log it \
                              (`tracing::warn!(?e, \"…\")`), or propagate it via the \
                              caller's `Result`. If a silent drop is genuinely correct, \
                              bind the value and write the rationale in a comment so the \
                              next reviewer doesn't read it as a TODO."
                            .into(),
                    });
                }
            }
        }
        syn::visit::visit_expr_match(self, m);
    }

    fn visit_expr_if(&mut self, e: &'ast syn::ExprIf) {
        if self.depth > 0
            && let syn::Expr::Let(let_expr) = &*e.cond
            && is_err_pat(&let_expr.pat)
            && is_empty_block(&e.then_branch)
        {
            self.findings.push(EmptyAsyncErrorArmFinding {
                code: "empty_async_error_arm",
                severity: "warning",
                file: self.file.clone(),
                line: let_expr.let_token.span.start().line,
                shape: "if_let_block",
                message: "An `if let Err(_) = … {}` with an empty body inside an async \
                          context silently drops the failure. The branch reads as 'noop \
                          on error' which is almost never what the call site wants."
                    .into(),
                fix: "Either use `if let Err(e) = … { status.set(Some(format!(\"…: \
                      {e}\"))); }` to surface the error, or switch to `let _ = …;` if \
                      the drop is genuinely deliberate (the explicit `_` form makes the \
                      intent reviewable)."
                    .into(),
            });
        }
        syn::visit::visit_expr_if(self, e);
    }
}

fn is_err_pat(p: &syn::Pat) -> bool {
    // `Err(<binding>)` — TupleStruct with the last path segment being
    // `Err`. We don't follow the binding (`_` vs `_e` vs `e`); the body
    // emptiness is what matters.
    if let syn::Pat::TupleStruct(ts) = p {
        return ts
            .path
            .segments
            .last()
            .map(|s| s.ident == "Err")
            .unwrap_or(false);
    }
    false
}

fn is_empty_body(expr: &syn::Expr) -> bool {
    match expr {
        syn::Expr::Block(b) => is_empty_block(&b.block),
        syn::Expr::Tuple(t) if t.elems.is_empty() => true, // `()` shorthand
        _ => false,
    }
}

fn is_empty_block(b: &syn::Block) -> bool {
    // Strictly empty (`{}`) or contains only no-op semi statements
    // (`{ ; }`). A single explicit unit (`{ () }`) is also a no-op.
    if b.stmts.is_empty() {
        return true;
    }
    b.stmts.iter().all(|s| match s {
        syn::Stmt::Expr(syn::Expr::Tuple(t), _) => t.elems.is_empty(),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn run(content: &str) -> EmptyAsyncErrorArmReport {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("lib.rs"), content).unwrap();
        let files = walk_rs_files(&src);
        let mut findings = Vec::new();
        for sf in &files {
            let Ok(ast) = &sf.ast else { continue };
            let mut v = AsyncContextScanner {
                file: sf.path.clone(),
                depth: 0,
                findings: &mut findings,
            };
            v.visit_file(ast);
        }
        EmptyAsyncErrorArmReport {
            findings,
            parse_errors: collect_parse_errors(&files),
        }
    }

    /// iter03 `ping_presence` shape — `Err(_) => {}` inside
    /// `use_future(|| async move { loop { match …await { … } } })`.
    #[test]
    fn flags_iter03_ping_presence_shape() {
        let r = run(r#"
fn use_future<F, Fut>(_: F) where F: FnOnce() -> Fut {}
fn outer() {
    use_future(|| async move {
        loop {
            match call().await {
                Ok(_) => {}
                Err(_) => {}
            }
        }
    });
}
async fn call() -> Result<(), ()> { Ok(()) }
"#);
        // Only the Err arm fires; the Ok-empty arm is a no-op result we don't lint.
        assert_eq!(r.findings.len(), 1, "{r:?}");
        assert_eq!(r.findings[0].shape, "match_arm");
    }

    /// Async fn body with `if let Err(_) = …await {}` — the if-let
    /// alternate shape.
    #[test]
    fn flags_if_let_empty_body() {
        let r = run(r#"
async fn run() {
    if let Err(_) = doit().await {}
}
async fn doit() -> Result<(), ()> { Ok(()) }
"#);
        assert_eq!(r.findings.len(), 1, "{r:?}");
        assert_eq!(r.findings[0].shape, "if_let_block");
    }

    /// `Err(_) => {}` in a SYNC fn body should not fire — the lint is
    /// scoped to async contexts where the failure mode (silent polling
    /// loop) is the concrete harm.
    #[test]
    fn silent_in_sync_context() {
        let r = run(r#"
fn run() {
    match doit() {
        Ok(_) => {}
        Err(_) => {}
    }
}
fn doit() -> Result<(), ()> { Ok(()) }
"#);
        assert!(r.findings.is_empty(), "{r:?}");
    }

    /// A non-empty Err body is exactly what we want — must stay silent.
    #[test]
    fn silent_when_err_arm_has_real_body() {
        let r = run(r#"
async fn run() {
    match doit().await {
        Ok(_) => {}
        Err(e) => { eprintln!("oops: {e:?}"); }
    }
}
async fn doit() -> Result<(), ()> { Ok(()) }
"#);
        assert!(r.findings.is_empty(), "{r:?}");
    }
}
