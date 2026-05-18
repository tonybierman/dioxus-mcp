//! `optimistic_lock_gate`: flag the hand-rolled "snapshot a u32 signal,
//! bump it, gate the async reconciliation by comparing the signal back to
//! the snapshot" pattern. The shape is correct but it has shown up in three
//! generated apps verbatim — each app reinvents the same staleness gate.
//!
//! The detected shape, in one closure/hook body, on the same signal `S`:
//!
//! ```ignore
//! let snap = S();              // 1. snapshot
//! S += 1;                      // 2. bump (optimistic write side)
//! spawn(async move {
//!     let _ = save().await;
//!     if S() == snap { ... }   // 3. staleness gate in the async tail
//! });
//! ```
//!
//! Suggestion: move the gate into a `Store` generation method
//! (e.g. `store.bump_revision() -> RevToken; if store.matches(rev) {...}`)
//! so the pattern lives in one place and every callsite reads as a
//! domain operation, not as four unrelated lines that happen to add up
//! to optimistic concurrency.
//!
//! Confidence: `medium`. We require all three shapes co-occurring on the
//! same signal in the same write source, so false positives are limited
//! to bodies that genuinely use a counter that way for non-staleness
//! reasons (rare in practice).

use std::path::PathBuf;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::visit::Visit;

use crate::state::State;
use crate::tools::ast::{ParseError, collect_parse_errors, walk_rs_files};
use crate::tools::scaffold::crate_root;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct OptimisticLockGateParams {
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OptimisticLockGateFinding {
    pub code: &'static str,
    pub file: PathBuf,
    /// Line of the snapshot `let snap = S();` — anchoring on the snapshot
    /// makes the finding stable across reorderings of bump / async block.
    pub line: usize,
    pub component: String,
    /// Signal binding name (e.g. `local_lock`).
    pub signal: String,
    /// Snapshot let-binding name (e.g. `lock`, `gen`).
    pub snapshot: String,
    /// `"high"` when snapshot + bump + compare all live in one closure /
    /// hook body. `"medium"` when the pattern is split across bodies in the
    /// same component (snapshot + compare in one hook, bump in a separate
    /// event handler) — still the same staleness gate, just less obviously.
    pub confidence: &'static str,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct OptimisticLockGateReport {
    pub findings: Vec<OptimisticLockGateFinding>,
    pub parse_errors: Vec<ParseError>,
}

pub async fn optimistic_lock_gate(
    state: &Arc<State>,
    p: OptimisticLockGateParams,
) -> Result<OptimisticLockGateReport, String> {
    let root = crate_root(state, p.project_root.as_deref()).await?;
    let src_root = root.join("src");
    let files = walk_rs_files(&src_root);

    let mut findings: Vec<OptimisticLockGateFinding> = Vec::new();
    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
        for item in &ast.items {
            let syn::Item::Fn(f) = item else { continue };
            if !is_component_fn(f) {
                continue;
            }
            let signals = collect_int_use_signals(&f.block);
            if signals.is_empty() {
                continue;
            }
            scan_component(f, &sf.path, &signals, &mut findings);
        }
    }

    Ok(OptimisticLockGateReport {
        findings,
        parse_errors: collect_parse_errors(&files),
    })
}

fn is_component_fn(f: &syn::ItemFn) -> bool {
    f.attrs.iter().any(|a| {
        a.path()
            .segments
            .last()
            .map(|s| s.ident == "component")
            .unwrap_or(false)
    })
}

/// Walk the top-level let statements of a component body and collect every
/// `let X = use_signal(|| <int_literal>);` binding name. We restrict to
/// integer-literal initializers — without a type resolver this is the only
/// shape where we can be confident the signal carries a counter, and the
/// staleness-gate pattern only makes sense for counters.
fn collect_int_use_signals(block: &syn::Block) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for stmt in &block.stmts {
        let syn::Stmt::Local(local) = stmt else {
            continue;
        };
        let Some(init) = &local.init else { continue };
        if !is_use_signal_int_init(&init.expr) {
            continue;
        }
        let name = match &local.pat {
            syn::Pat::Ident(p) => p.ident.to_string(),
            syn::Pat::Type(t) => match &*t.pat {
                syn::Pat::Ident(p) => p.ident.to_string(),
                _ => continue,
            },
            _ => continue,
        };
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

fn is_use_signal_int_init(expr: &syn::Expr) -> bool {
    let syn::Expr::Call(c) = expr else {
        return false;
    };
    let syn::Expr::Path(p) = &*c.func else {
        return false;
    };
    if p.path
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .as_deref()
        != Some("use_signal")
    {
        return false;
    }
    let Some(arg) = c.args.first() else {
        return false;
    };
    let syn::Expr::Closure(cl) = arg else {
        return false;
    };
    is_int_literal(&cl.body)
}

fn is_int_literal(expr: &syn::Expr) -> bool {
    match expr {
        syn::Expr::Lit(l) => matches!(l.lit, syn::Lit::Int(_)),
        syn::Expr::Unary(u) if matches!(u.op, syn::UnOp::Neg(_)) => is_int_literal(&u.expr),
        syn::Expr::Paren(p) => is_int_literal(&p.expr),
        _ => false,
    }
}

/// Locate every write-source scope (named closure binding, hook body) in
/// the component, run the three-shape detector against each, and push one
/// finding per (signal, scope) hit. After the single-body sweep, fall back
/// to a cross-body match: if any one body contains a snapshot + async-gate
/// pair AND any other body in the component bumps the same signal, emit a
/// `confidence: "medium"` finding. iter03's `local_lock` is the canonical
/// cross-body case — snapshot/compare in `use_future`, bumps scattered
/// across three distinct event handlers.
fn scan_component(
    f: &syn::ItemFn,
    file: &std::path::Path,
    signals: &[String],
    findings: &mut Vec<OptimisticLockGateFinding>,
) {
    let component = f.sig.ident.to_string();
    let sources = collect_write_sources(&f.block);
    let mut high_hits: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Pass 1 — single-body high-confidence findings, preserving the original
    // shape exactly. A signal that lands here is removed from the cross-body
    // sweep below so we never double-flag the same (component, signal).
    for src in &sources {
        for signal in signals {
            let Some(hit) = detect_in_source(src.body, signal) else {
                continue;
            };
            high_hits.insert(signal.clone());
            let snapshot_name = hit.snapshot_name.clone();
            findings.push(OptimisticLockGateFinding {
                code: "optimistic_lock_gate",
                file: file.to_path_buf(),
                line: hit.snapshot_line,
                component: component.clone(),
                signal: signal.clone(),
                snapshot: hit.snapshot_name,
                confidence: "high",
                message: format!(
                    "`{signal}` is hand-rolling the optimistic-lock staleness gate: \
                     snapshot (`let {snap} = {signal}();`), bump (`{signal} += …;`), \
                     and a gated reconciliation (`if {signal}() == {snap} {{ … }}`) \
                     inside an async tail. This is the third generated app with the \
                     same shape — extract the pattern into a `Store` generation \
                     method (e.g. `let rev = store.bump_revision(); … if \
                     store.matches(rev) {{ … }}`) so the invariant lives in one \
                     place. See the `Store` primitive in `get_dsl_spec`.",
                    snap = snapshot_name,
                ),
            });
        }
    }

    // Pass 2 — cross-body medium-confidence findings. For each remaining
    // signal, see if any single body contains the snapshot+gate pair AND any
    // body in the component bumps the same signal. We walk:
    //   - the tracked write-sources (the canonical case), AND
    //   - every `use_future`/`use_effect`/`use_resource`/`use_callback` call
    //     in the component, regardless of let-binding name (iter03's polling
    //     loop is bound to `_polling` and otherwise gets filtered out by
    //     `collect_write_sources`).
    // For bumps we walk the whole fn body, which catches event handlers
    // bound to inline rsx! attributes too.
    let mut extra_hook_bodies: Vec<&syn::Expr> = Vec::new();
    let mut hook_v = HookBodyVisitor {
        bodies: &mut extra_hook_bodies,
    };
    hook_v.visit_block(&f.block);
    for signal in signals {
        if high_hits.contains(signal) {
            continue;
        }
        let mut pair: Option<DetectionHit> = None;
        let candidates = sources
            .iter()
            .map(|s| s.body)
            .chain(extra_hook_bodies.iter().copied());
        for body in candidates {
            if let Some(hit) = detect_snapshot_gate_in_source(body, signal) {
                pair = Some(hit);
                break;
            }
        }
        let Some(hit) = pair else { continue };
        // Bump anywhere in the entire component body (any source, any depth).
        let mut bump_v = BumpVisitor { signal, saw: false };
        bump_v.visit_block(&f.block);
        if !bump_v.saw {
            continue;
        }
        let snapshot_name = hit.snapshot_name.clone();
        findings.push(OptimisticLockGateFinding {
            code: "optimistic_lock_gate",
            file: file.to_path_buf(),
            line: hit.snapshot_line,
            component: component.clone(),
            signal: signal.clone(),
            snapshot: hit.snapshot_name,
            confidence: "medium",
            message: format!(
                "`{signal}` looks like the optimistic-lock staleness gate split \
                 across bodies: snapshot + async compare against `{snap}` live in \
                 one hook/closure, the bump (`{signal} += …;`) lives in another. \
                 The semantics are still the same — extract into a `Store` \
                 generation method (e.g. `let rev = store.bump_revision(); … if \
                 store.matches(rev) {{ … }}`) so the invariant lives in one \
                 place. See the `Store` primitive in `get_dsl_spec`.",
                snap = snapshot_name,
            ),
        });
    }
}

/// Like `detect_in_source` but skips the bump requirement — the bump is
/// allowed to live in another body. Used only by the cross-body fallback.
fn detect_snapshot_gate_in_source(body: &syn::Expr, signal: &str) -> Option<DetectionHit> {
    let mut snap_v = SnapshotVisitor {
        signal,
        hits: Vec::new(),
    };
    snap_v.visit_expr(body);
    if snap_v.hits.is_empty() {
        return None;
    }
    let mut gate_v = AsyncGateVisitor {
        signal,
        async_depth: 0,
        saw_names: std::collections::HashSet::new(),
    };
    gate_v.visit_expr(body);
    if gate_v.saw_names.is_empty() {
        return None;
    }
    for (name, line) in &snap_v.hits {
        if gate_v.saw_names.contains(name) {
            return Some(DetectionHit {
                snapshot_name: name.clone(),
                snapshot_line: *line,
            });
        }
    }
    None
}

struct WriteSource<'a> {
    body: &'a syn::Expr,
}

/// Same convention as `signal_lint::collect_write_sources` but stripped to
/// only the borrowed body — we don't need names because each finding ties
/// itself to the snapshot let-line we detect inside the scope.
fn collect_write_sources(block: &syn::Block) -> Vec<WriteSource<'_>> {
    let mut out: Vec<WriteSource> = Vec::new();
    for stmt in &block.stmts {
        match stmt {
            syn::Stmt::Local(local) => {
                let Some(init) = &local.init else { continue };
                let name = match &local.pat {
                    syn::Pat::Ident(p) => p.ident.to_string(),
                    syn::Pat::Type(t) => match &*t.pat {
                        syn::Pat::Ident(p) => p.ident.to_string(),
                        _ => continue,
                    },
                    _ => continue,
                };
                if name.starts_with('_') {
                    continue;
                }
                if matches!(&*init.expr, syn::Expr::Closure(_)) {
                    out.push(WriteSource { body: &init.expr });
                    continue;
                }
                if is_named_hook_init(&init.expr) {
                    out.push(WriteSource { body: &init.expr });
                }
            }
            syn::Stmt::Expr(expr, semi) if semi.is_some() => {
                if is_named_hook_init(expr) {
                    out.push(WriteSource { body: expr });
                }
            }
            _ => {}
        }
    }
    out
}

/// Visitor that collects every `use_future`/`use_effect`/`use_resource`/
/// `use_callback` call expression in the component body, regardless of
/// how the result is bound. Cross-body detection feeds these as extra
/// candidate bodies on top of `collect_write_sources` (which skips
/// underscore-prefixed bindings); without this iter03's `let _polling =
/// use_future(…)` polling stub would be invisible to the lint.
struct HookBodyVisitor<'a, 'ast> {
    bodies: &'a mut Vec<&'ast syn::Expr>,
}

impl<'a, 'ast> Visit<'ast> for HookBodyVisitor<'a, 'ast> {
    fn visit_expr(&mut self, expr: &'ast syn::Expr) {
        if matches!(expr, syn::Expr::Call(_)) && is_named_hook_init(expr) {
            self.bodies.push(expr);
        }
        syn::visit::visit_expr(self, expr);
    }
}

fn is_named_hook_init(expr: &syn::Expr) -> bool {
    let syn::Expr::Call(c) = expr else {
        return false;
    };
    let syn::Expr::Path(p) = &*c.func else {
        return false;
    };
    let Some(last) = p.path.segments.last() else {
        return false;
    };
    matches!(
        last.ident.to_string().as_str(),
        "use_future" | "use_effect" | "use_resource" | "use_callback"
    )
}

struct DetectionHit {
    snapshot_name: String,
    snapshot_line: usize,
}

/// Look for all three shapes targeting `signal` inside one write-source
/// body. We snapshot on the FIRST `let snap = S()` we encounter, then
/// require that the same `snap` name appears in an `==`/`!=` against `S()`
/// inside an async/spawn block somewhere in the same body, AND a compound
/// `S += literal` (or `-=`) write also appears in the body.
///
/// Returns `Some` on the first match; we want at most one finding per
/// (source, signal) so this short-circuits.
fn detect_in_source(body: &syn::Expr, signal: &str) -> Option<DetectionHit> {
    let mut snap_v = SnapshotVisitor {
        signal,
        hits: Vec::new(),
    };
    snap_v.visit_expr(body);
    if snap_v.hits.is_empty() {
        return None;
    }
    let mut bump_v = BumpVisitor { signal, saw: false };
    bump_v.visit_expr(body);
    if !bump_v.saw {
        return None;
    }
    let mut gate_v = AsyncGateVisitor {
        signal,
        async_depth: 0,
        saw_names: std::collections::HashSet::new(),
    };
    gate_v.visit_expr(body);
    if gate_v.saw_names.is_empty() {
        return None;
    }
    // Match the FIRST snapshot whose binding name also appears in the gate set.
    for (name, line) in &snap_v.hits {
        if gate_v.saw_names.contains(name) {
            return Some(DetectionHit {
                snapshot_name: name.clone(),
                snapshot_line: *line,
            });
        }
    }
    None
}

struct SnapshotVisitor<'a> {
    signal: &'a str,
    /// (binding name, line) for each `let snap = S();` we find.
    hits: Vec<(String, usize)>,
}

impl<'a, 'ast> Visit<'ast> for SnapshotVisitor<'a> {
    fn visit_local(&mut self, local: &'ast syn::Local) {
        if let Some(init) = &local.init
            && is_signal_call(&init.expr, self.signal)
            && let Some(name) = pat_single_ident(&local.pat)
        {
            self.hits.push((name, local.let_token.span.start().line));
        }
        syn::visit::visit_local(self, local);
    }
}

struct BumpVisitor<'a> {
    signal: &'a str,
    saw: bool,
}

impl<'a, 'ast> Visit<'ast> for BumpVisitor<'a> {
    fn visit_expr_binary(&mut self, eb: &'ast syn::ExprBinary) {
        let is_compound = matches!(eb.op, syn::BinOp::AddAssign(_) | syn::BinOp::SubAssign(_));
        if is_compound
            && let Some(name) = expr_single_ident(&eb.left)
            && name == self.signal
            && is_int_literal(&eb.right)
        {
            self.saw = true;
        }
        syn::visit::visit_expr_binary(self, eb);
    }
}

struct AsyncGateVisitor<'a> {
    signal: &'a str,
    async_depth: u32,
    /// Snapshot binding names that appear in an `S() == snap` / `snap == S()`
    /// comparison inside an async block. We don't try to scope-check the
    /// binding — if the name was introduced earlier in the same write source
    /// AND it lines up with a snapshot we found, that's a strong enough match.
    saw_names: std::collections::HashSet<String>,
}

impl<'a, 'ast> Visit<'ast> for AsyncGateVisitor<'a> {
    fn visit_expr_async(&mut self, ea: &'ast syn::ExprAsync) {
        self.async_depth += 1;
        syn::visit::visit_expr_async(self, ea);
        self.async_depth -= 1;
    }
    fn visit_expr_closure(&mut self, ec: &'ast syn::ExprClosure) {
        // `async || { … }` blocks (Rust 1.85+) carry asyncness on the
        // closure itself rather than via an inner async expr; treat them
        // the same as `async { … }`.
        let is_async = ec.asyncness.is_some();
        if is_async {
            self.async_depth += 1;
        }
        syn::visit::visit_expr_closure(self, ec);
        if is_async {
            self.async_depth -= 1;
        }
    }
    fn visit_expr_binary(&mut self, eb: &'ast syn::ExprBinary) {
        if self.async_depth > 0 && matches!(eb.op, syn::BinOp::Eq(_) | syn::BinOp::Ne(_)) {
            // S() == snap  or  snap == S()
            if is_signal_call(&eb.left, self.signal)
                && let Some(name) = expr_single_ident(&eb.right)
            {
                self.saw_names.insert(name);
            } else if is_signal_call(&eb.right, self.signal)
                && let Some(name) = expr_single_ident(&eb.left)
            {
                self.saw_names.insert(name);
            }
        }
        syn::visit::visit_expr_binary(self, eb);
    }
}

/// True when `expr` is `S()` — i.e. a call whose callee is the bare ident
/// `signal`. Dioxus's `Signal<T>: Fn()` makes this the canonical read
/// shape, and the one the standup app uses.
fn is_signal_call(expr: &syn::Expr, signal: &str) -> bool {
    let syn::Expr::Call(c) = expr else {
        return false;
    };
    if !c.args.is_empty() {
        return false;
    }
    match expr_single_ident(&c.func) {
        Some(name) => name == signal,
        None => false,
    }
}

fn pat_single_ident(pat: &syn::Pat) -> Option<String> {
    match pat {
        syn::Pat::Ident(p) => Some(p.ident.to_string()),
        syn::Pat::Type(t) => pat_single_ident(&t.pat),
        _ => None,
    }
}

fn expr_single_ident(expr: &syn::Expr) -> Option<String> {
    if let syn::Expr::Path(p) = expr
        && p.path.segments.len() == 1
        && p.path.leading_colon.is_none()
        && p.qself.is_none()
    {
        return Some(p.path.segments[0].ident.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn scan(src: &str) -> Vec<OptimisticLockGateFinding> {
        let dir = TempDir::new().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("comp.rs"), src).unwrap();
        let files = walk_rs_files(&src_dir);
        let mut findings = Vec::new();
        for sf in &files {
            let Ok(ast) = &sf.ast else { continue };
            for item in &ast.items {
                let syn::Item::Fn(f) = item else { continue };
                if !is_component_fn(f) {
                    continue;
                }
                let signals = collect_int_use_signals(&f.block);
                if signals.is_empty() {
                    continue;
                }
                scan_component(f, &sf.path, &signals, &mut findings);
            }
        }
        findings
    }

    /// Canonical standup-app shape: u32 signal, snapshot, bump, async-gate
    /// triple in one event handler — must fire exactly once.
    #[test]
    fn flags_canonical_three_shape_match() {
        let findings = scan(
            r#"use dioxus::prelude::*;

#[component]
fn Board() -> Element {
    let mut local_lock = use_signal(|| 0u32);
    let on_save = move |_| {
        let lock = local_lock();
        local_lock += 1;
        spawn(async move {
            if local_lock() == lock {
                let _ = ();
            }
        });
    };
    rsx! {}
}
"#,
        );
        assert_eq!(
            findings.len(),
            1,
            "exactly one finding expected: {findings:?}"
        );
        assert_eq!(findings[0].signal, "local_lock");
        assert_eq!(findings[0].snapshot, "lock");
        assert_eq!(findings[0].code, "optimistic_lock_gate");
    }

    /// Reversed comparison side (`lock == local_lock()`) must also match —
    /// either order is the same pattern.
    #[test]
    fn flags_reversed_comparison() {
        let findings = scan(
            r#"use dioxus::prelude::*;

#[component]
fn Board() -> Element {
    let mut local_lock = use_signal(|| 0u32);
    let on_save = move |_| {
        let lock = local_lock();
        local_lock += 1;
        spawn(async move {
            if lock == local_lock() {
                let _ = ();
            }
        });
    };
    rsx! {}
}
"#,
        );
        assert_eq!(
            findings.len(),
            1,
            "reversed compare must match: {findings:?}"
        );
    }

    /// No bump → not the staleness pattern (just a one-shot read for
    /// display); must NOT fire.
    #[test]
    fn does_not_flag_without_bump() {
        let findings = scan(
            r#"use dioxus::prelude::*;

#[component]
fn Board() -> Element {
    let local_lock = use_signal(|| 0u32);
    let on_save = move |_| {
        let lock = local_lock();
        spawn(async move {
            if local_lock() == lock {
                let _ = ();
            }
        });
    };
    rsx! {}
}
"#,
        );
        assert!(
            findings.is_empty(),
            "no bump → not the staleness pattern: {findings:?}"
        );
    }

    /// Compare outside an async block → not the staleness pattern (the
    /// gate exists precisely because the result arrives later); must NOT fire.
    #[test]
    fn does_not_flag_sync_compare() {
        let findings = scan(
            r#"use dioxus::prelude::*;

#[component]
fn Board() -> Element {
    let mut local_lock = use_signal(|| 0u32);
    let on_save = move |_| {
        let lock = local_lock();
        local_lock += 1;
        if local_lock() == lock {
            // this is the same tick — no race to gate
            let _ = ();
        }
    };
    rsx! {}
}
"#,
        );
        assert!(
            findings.is_empty(),
            "sync compare is not the optimistic-lock pattern: {findings:?}"
        );
    }

    /// Non-integer initializer (`Vec::new`) means we can't be sure the
    /// signal is a counter — skip silently.
    #[test]
    fn does_not_flag_non_integer_signal() {
        let findings = scan(
            r#"use dioxus::prelude::*;

#[component]
fn Board() -> Element {
    let mut items = use_signal(|| Vec::<u32>::new());
    let on_save = move |_| {
        let snap = items();
        spawn(async move {
            if items() == snap {
                let _ = ();
            }
        });
    };
    rsx! {}
}
"#,
        );
        assert!(
            findings.is_empty(),
            "non-integer use_signal must not fire: {findings:?}"
        );
    }

    /// Snapshot is reassigned to a *different* name than the one the gate
    /// uses → not the same staleness pair; must NOT fire.
    #[test]
    fn does_not_flag_when_snapshot_and_gate_names_differ() {
        let findings = scan(
            r#"use dioxus::prelude::*;

#[component]
fn Board() -> Element {
    let mut local_lock = use_signal(|| 0u32);
    let on_save = move |_| {
        let snap = local_lock();
        local_lock += 1;
        spawn(async move {
            // Compares against a different binding (`other`) — not our snap.
            let other = 7u32;
            if local_lock() == other {
                let _ = ();
            }
            let _ = snap;
        });
    };
    rsx! {}
}
"#,
        );
        assert!(
            findings.is_empty(),
            "gate must be against our snapshot binding: {findings:?}"
        );
    }

    /// Two distinct signals, each with the pattern, in two distinct
    /// handlers → two findings.
    #[test]
    fn flags_two_distinct_signals_in_two_handlers() {
        let findings = scan(
            r#"use dioxus::prelude::*;

#[component]
fn Board() -> Element {
    let mut a = use_signal(|| 0u32);
    let mut b = use_signal(|| 0u32);
    let on_a = move |_| {
        let snap = a();
        a += 1;
        spawn(async move { if a() == snap { let _ = (); } });
    };
    let on_b = move |_| {
        let snap = b();
        b += 1;
        spawn(async move { if b() == snap { let _ = (); } });
    };
    rsx! {}
}
"#,
        );
        let names: Vec<&str> = findings.iter().map(|f| f.signal.as_str()).collect();
        assert!(names.contains(&"a"), "missing a: {findings:?}");
        assert!(names.contains(&"b"), "missing b: {findings:?}");
    }

    /// iter03's canonical shape: snapshot + async-gate live in `use_future`,
    /// the bumps live in unrelated event-handler closures. Before the fix
    /// this returned zero findings because `detect_in_source` required all
    /// three shapes in one body. Now it emits a `confidence: "medium"`
    /// finding anchored on the snapshot line.
    #[test]
    fn flags_cross_body_snapshot_and_bump() {
        let findings = scan(
            r#"use dioxus::prelude::*;

#[component]
fn Board() -> Element {
    let mut local_lock = use_signal(|| 0u32);
    let polling = use_future(move || async move {
        let lock = local_lock();
        let _ = fetch_board().await;
        if local_lock() == lock {
            // reconcile
        }
    });
    let submit_card = move |_| {
        local_lock += 1;
        // … POST /api/cards/create …
    };
    let delete_card_action = move |_| {
        local_lock += 1;
        // … DELETE /api/cards …
    };
    rsx! {}
}
"#,
        );
        assert_eq!(
            findings.len(),
            1,
            "exactly one cross-body finding expected: {findings:?}",
        );
        assert_eq!(findings[0].signal, "local_lock");
        assert_eq!(
            findings[0].confidence, "medium",
            "cross-body pattern should be medium-confidence: {findings:?}",
        );
        // Anchor line is the snapshot inside `use_future`.
        assert!(
            findings[0].snapshot == "lock",
            "snapshot binding should be `lock`: {findings:?}",
        );
    }

    /// iter03's polling stub is bound to `_polling = use_future(…)` —
    /// `collect_write_sources` skips underscore-prefixed bindings, so the
    /// snapshot+gate pair would be invisible without the `HookBodyVisitor`
    /// fallback. This test locks in that path.
    #[test]
    fn flags_cross_body_with_underscore_bound_use_future() {
        let findings = scan(
            r#"use dioxus::prelude::*;

#[component]
fn Board() -> Element {
    let mut local_lock = use_signal(|| 0u32);
    let _polling = use_future(move || async move {
        let lock = local_lock();
        let _ = fetch_board().await;
        if local_lock() == lock {
            // reconcile
        }
    });
    let submit_card = move |_| {
        local_lock += 1;
    };
    rsx! {}
}
"#,
        );
        assert_eq!(
            findings.len(),
            1,
            "cross-body fallback must see hooks behind `_`-bindings: {findings:?}",
        );
        assert_eq!(findings[0].confidence, "medium");
        assert_eq!(findings[0].signal, "local_lock");
    }

    /// Single-body matches stay `confidence: "high"` — the cross-body
    /// fallback must NOT double-flag the same (component, signal). Without
    /// the `high_hits` short-circuit the canonical test app would emit two
    /// overlapping findings.
    #[test]
    fn single_body_match_is_high_confidence() {
        let findings = scan(
            r#"use dioxus::prelude::*;

#[component]
fn Board() -> Element {
    let mut local_lock = use_signal(|| 0u32);
    let on_save = move |_| {
        let lock = local_lock();
        local_lock += 1;
        spawn(async move {
            if local_lock() == lock { let _ = (); }
        });
    };
    rsx! {}
}
"#,
        );
        assert_eq!(findings.len(), 1, "exactly one finding: {findings:?}");
        assert_eq!(
            findings[0].confidence, "high",
            "single-body match must stay high: {findings:?}",
        );
    }

    /// Non-component fns (no `#[component]` attr) must be ignored — the
    /// pattern is only meaningful in the reactive render context.
    #[test]
    fn ignores_non_component_fn() {
        let findings = scan(
            r#"use dioxus::prelude::*;

fn helper() {
    let mut local_lock = use_signal(|| 0u32);
    let on_save = move |_| {
        let lock = local_lock();
        local_lock += 1;
        spawn(async move {
            if local_lock() == lock { let _ = (); }
        });
    };
    let _ = on_save;
}
"#,
        );
        assert!(
            findings.is_empty(),
            "no #[component] → out of scope: {findings:?}"
        );
    }
}
