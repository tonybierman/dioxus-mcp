//! `polling_future_no_backoff`: flag a `use_future` / `spawn` polling
//! loop that calls a server fn on every tick and then sleeps for a
//! constant interval (e.g. `TimeoutFuture::new(2000).await`) without
//! extending the delay on error or adding jitter. When the server
//! hiccups, every client retries on the same cadence — a classic
//! thundering-herd shape.
//!
//! Detected shape:
//!
//! ```ignore
//! use_future(move || async move {
//!     loop {
//!         let _ = crate::server::poll().await;          // (1) awaited call
//!         gloo_timers::future::TimeoutFuture::new(2000) // (2) constant delay
//!             .await;
//!     }
//! });
//! ```
//!
//! Three requirements for a finding:
//!   1. We're inside an async block (`async fn`, inline `async { … }`,
//!      or async closure body — covers both `use_future` and `spawn`).
//!   2. The async body contains a `loop { … }`.
//!   3. Inside that loop, at least one awaited expression AND a
//!      "sleep-like" call (`TimeoutFuture::new(<int>).await`,
//!      `tokio::time::sleep(Duration::from_…)`, `gloo_timers::sleep(…)`)
//!      whose duration argument is a literal integer — i.e. constant,
//!      no error-path variation.
//!
//! Fix: switch to exponential backoff on error (a `delay` local that
//! doubles on each Err and resets on Ok), and add jitter (e.g. randomise
//! by ±25%). Simpler still: extract a `retry_with_backoff(…)` helper
//! and call it from one place.

use std::path::PathBuf;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::spanned::Spanned;
use syn::visit::Visit;

use crate::state::State;
use crate::tools::ast::{ParseError, collect_parse_errors, walk_rs_files};
use crate::tools::scaffold::crate_root;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PollingFutureNoBackoffParams {
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PollingFutureFinding {
    pub code: &'static str,
    pub severity: &'static str,
    pub file: PathBuf,
    pub line: usize,
    /// The constant delay in milliseconds (best-effort parse from the
    /// literal). `None` if the argument was a path/expression we
    /// couldn't fold to a literal.
    pub delay_ms: Option<u64>,
    /// Last-segment path of the sleep call (e.g. `TimeoutFuture::new`,
    /// `sleep`, `delay_for`). Surfaced so the caller can grep for the
    /// site without re-deriving it from `file:line`.
    pub sleep_call: String,
    pub message: String,
    pub fix: String,
}

#[derive(Debug, Serialize)]
pub struct PollingFutureReport {
    pub findings: Vec<PollingFutureFinding>,
    pub parse_errors: Vec<ParseError>,
}

pub async fn polling_future_no_backoff(
    state: &Arc<State>,
    p: PollingFutureNoBackoffParams,
) -> Result<PollingFutureReport, String> {
    let root = crate_root(state, p.project_root.as_deref()).await?;
    let src_root = root.join("src");
    let files = walk_rs_files(&src_root);

    let mut findings: Vec<PollingFutureFinding> = Vec::new();
    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
        let mut scanner = AsyncLoopScanner {
            file: sf.path.clone(),
            depth: 0,
            findings: &mut findings,
        };
        scanner.visit_file(ast);
    }
    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));

    Ok(PollingFutureReport {
        findings,
        parse_errors: collect_parse_errors(&files),
    })
}

/// Track async-context depth (same shape as `empty_async_error_arm`) so
/// we only inspect `loop { … }` bodies that are running on the runtime,
/// not sync loops the caller deliberately wrote.
struct AsyncLoopScanner<'a> {
    file: PathBuf,
    depth: usize,
    findings: &'a mut Vec<PollingFutureFinding>,
}

impl<'a, 'ast> Visit<'ast> for AsyncLoopScanner<'a> {
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
    fn visit_expr_loop(&mut self, l: &'ast syn::ExprLoop) {
        if self.depth > 0 {
            let mut probe = LoopBodyProbe::default();
            probe.visit_block(&l.body);
            if probe.has_await
                && let Some(sleep) = probe.first_constant_sleep
                && sleep.delay_ms.is_some()
            {
                self.findings.push(PollingFutureFinding {
                    code: "polling_future_no_backoff",
                    severity: "warning",
                    file: self.file.clone(),
                    line: sleep.line,
                    delay_ms: sleep.delay_ms,
                    sleep_call: sleep.path_tail,
                    message: "This async loop sleeps for a constant interval after every \
                         awaited call. When the server hiccups, every client retries on \
                         the same cadence — a classic thundering-herd shape, and \
                         user-visible latency spikes (briefly stale UI on a healthy \
                         server become long stalls on a bad one)."
                        .into(),
                    fix: "Add error-aware backoff: `let mut delay = 1000u64;` outside \
                          the loop; on Ok reset to the base interval, on Err multiply \
                          by 2 up to a cap (e.g. 30s) and add jitter (`delay = delay + \
                          js_random_u64() % (delay / 2);`). For shared shapes extract a \
                          `retry_with_backoff(…)` helper into `src/model/` so every \
                          polling loop in the app uses the same policy."
                        .into(),
                });
            }
        }
        syn::visit::visit_expr_loop(self, l);
    }
}

#[derive(Default)]
struct LoopBodyProbe {
    has_await: bool,
    first_constant_sleep: Option<SleepCall>,
}

struct SleepCall {
    line: usize,
    /// Trailing path segment(s): `TimeoutFuture::new`, `sleep`, etc.
    path_tail: String,
    delay_ms: Option<u64>,
}

impl<'ast> Visit<'ast> for LoopBodyProbe {
    fn visit_expr_loop(&mut self, _l: &'ast syn::ExprLoop) {
        // Don't descend into a nested loop — its own pass will scan it.
    }
    fn visit_expr_async(&mut self, _e: &'ast syn::ExprAsync) {
        // Don't descend into a nested async block; it's a separate
        // future and any constant-sleep there is a separate finding.
    }
    fn visit_expr_await(&mut self, ea: &'ast syn::ExprAwait) {
        self.has_await = true;
        if self.first_constant_sleep.is_none()
            && let syn::Expr::Call(call) = &*ea.base
            && let Some(sleep) = classify_sleep_call(call)
        {
            self.first_constant_sleep = Some(sleep);
        }
        // also support `<expr>.method().await` chains where the sleep
        // call is the inner method (`sleep(Duration).await`).
        if self.first_constant_sleep.is_none()
            && let syn::Expr::MethodCall(mc) = &*ea.base
            && let Some(sleep) = classify_method_sleep(mc)
        {
            self.first_constant_sleep = Some(sleep);
        }
        syn::visit::visit_expr_await(self, ea);
    }
}

fn classify_sleep_call(call: &syn::ExprCall) -> Option<SleepCall> {
    let syn::Expr::Path(p) = &*call.func else {
        return None;
    };
    let last = p.path.segments.last()?.ident.to_string();
    // The set of names we recognise as a sleep / timer constructor.
    // Constructor calls (`TimeoutFuture::new(…)`) ride on the same
    // .await chain shape, so we accept both.
    if !matches!(
        last.as_str(),
        "new" | "sleep" | "delay_for" | "delay_until" | "sleep_until"
    ) {
        return None;
    }
    // Heuristic: if the path is `<Type>::new`, require `<Type>` to look
    // timer-shaped (`TimeoutFuture`, `Timer`, `Delay`, `Interval`,
    // `Sleep`) — otherwise `Vec::new` and friends would match.
    if last == "new" {
        let prev = p
            .path
            .segments
            .iter()
            .rev()
            .nth(1)
            .map(|s| s.ident.to_string());
        let is_timer_type = matches!(
            prev.as_deref(),
            Some("TimeoutFuture")
                | Some("Timer")
                | Some("Delay")
                | Some("Interval")
                | Some("Sleep")
        );
        if !is_timer_type {
            return None;
        }
    }
    let arg = call.args.iter().next()?;
    let delay_ms = literal_u64(arg);
    Some(SleepCall {
        line: call.func.span().start().line,
        path_tail: tail_path(&p.path),
        delay_ms,
    })
}

fn classify_method_sleep(mc: &syn::ExprMethodCall) -> Option<SleepCall> {
    let name = mc.method.to_string();
    if !matches!(name.as_str(), "sleep" | "delay_for" | "delay_until") {
        return None;
    }
    let arg = mc.args.iter().next()?;
    let delay_ms = literal_u64(arg);
    Some(SleepCall {
        line: mc.method.span().start().line,
        path_tail: name,
        delay_ms,
    })
}

fn tail_path(p: &syn::Path) -> String {
    let n = p.segments.len();
    let take = n.min(2);
    let segs: Vec<String> = p
        .segments
        .iter()
        .skip(n - take)
        .map(|s| s.ident.to_string())
        .collect();
    segs.join("::")
}

/// Recognise a literal integer (raw `2000`, `Duration::from_millis(2000)`,
/// `Duration::from_secs(2)`). Returns the value in milliseconds where
/// possible; `None` means "not a constant" — and a non-constant delay
/// is presumed to already encode backoff, so we don't flag it.
fn literal_u64(e: &syn::Expr) -> Option<u64> {
    if let syn::Expr::Lit(lit) = e
        && let syn::Lit::Int(i) = &lit.lit
        && let Ok(v) = i.base10_parse::<u64>()
    {
        return Some(v);
    }
    if let syn::Expr::Call(call) = e
        && let syn::Expr::Path(p) = &*call.func
        && let Some(last) = p.path.segments.last()
        && let Some(arg) = call.args.iter().next()
        && let Some(inner) = literal_u64(arg)
    {
        return match last.ident.to_string().as_str() {
            "from_millis" | "millis" => Some(inner),
            "from_secs" | "secs" => Some(inner * 1000),
            "from_nanos" | "nanos" => Some(inner / 1_000_000),
            "from_micros" | "micros" => Some(inner / 1000),
            _ => None,
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn run(content: &str) -> PollingFutureReport {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("lib.rs"), content).unwrap();
        let files = walk_rs_files(&src);
        let mut findings = Vec::new();
        for sf in &files {
            let Ok(ast) = &sf.ast else { continue };
            let mut scanner = AsyncLoopScanner {
                file: sf.path.clone(),
                depth: 0,
                findings: &mut findings,
            };
            scanner.visit_file(ast);
        }
        PollingFutureReport {
            findings,
            parse_errors: collect_parse_errors(&files),
        }
    }

    /// iter03 board-poll shape: 2s constant `TimeoutFuture::new` after
    /// awaiting a server fn.
    #[test]
    fn flags_iter03_board_poll() {
        let r = run(r#"
async fn run() {
    loop {
        let _ = fetch().await;
        gloo_timers::future::TimeoutFuture::new(2000).await;
    }
}
async fn fetch() -> Result<(), ()> { Ok(()) }
"#);
        assert_eq!(r.findings.len(), 1, "{r:?}");
        assert_eq!(r.findings[0].delay_ms, Some(2000));
        assert_eq!(r.findings[0].sleep_call, "TimeoutFuture::new");
    }

    /// `tokio::time::sleep(Duration::from_millis(500)).await` is the
    /// same shape via the method-call form.
    #[test]
    fn flags_tokio_sleep_constant() {
        let r = run(r#"
async fn run() {
    loop {
        let _ = fetch().await;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}
async fn fetch() -> Result<(), ()> { Ok(()) }
"#);
        assert_eq!(r.findings.len(), 1, "{r:?}");
        assert_eq!(r.findings[0].delay_ms, Some(500));
    }

    /// A loop where the delay is a *variable* is presumed to encode
    /// backoff — must stay silent.
    #[test]
    fn silent_when_delay_is_a_variable() {
        let r = run(r#"
async fn run() {
    let mut delay = 1000u64;
    loop {
        let _ = fetch().await;
        gloo_timers::future::TimeoutFuture::new(delay).await;
        delay = (delay * 2).min(30_000);
    }
}
async fn fetch() -> Result<(), ()> { Ok(()) }
"#);
        assert!(r.findings.is_empty(), "{r:?}");
    }

    /// A sync loop with a constant sleep is fine (not a polling future).
    #[test]
    fn silent_in_sync_context() {
        let r = run(r#"
fn run() {
    loop {
        std::thread::sleep(std::time::Duration::from_millis(2000));
    }
}
"#);
        assert!(r.findings.is_empty(), "{r:?}");
    }
}
