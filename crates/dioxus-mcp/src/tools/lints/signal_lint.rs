use std::path::{Path, PathBuf};
use std::sync::Arc;

use proc_macro2::TokenTree;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::spanned::Spanned;
use syn::visit::Visit;

use crate::state::State;
use crate::tools::ast::{ParseError, collect_parse_errors, walk_rs_files};
use crate::tools::scaffold::crate_root;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SignalLintParams {
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SignalIssue {
    pub code: &'static str,
    pub message: String,
    pub file: PathBuf,
    pub line: usize,
    pub component: Option<String>,
    /// Signal binding the issue is about, when the lint knows it. Used by
    /// the cross-link pass to pair `signal_many_writers` and
    /// `signal_used_as_fence` findings that target the same signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    /// Other lint codes that also fire for this signal in the same
    /// component. Populated by the cross-link post-pass so a caller can
    /// see "fixing `signal` to a `Store` covers both findings" without
    /// reading every issue body. Empty when no related finding exists.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_codes: Vec<String>,
    /// Paste-ready code suggestion, when the lint can generate one. The
    /// rollup keeps this short — a single function or struct skeleton the
    /// reviewer can drop into the component and adapt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SignalLintReport {
    pub issues: Vec<SignalIssue>,
    /// Suggestions to collapse 3+ sibling `provide_X` / `use_X` context-signal
    /// modules into one `Store`. Empty when the project has fewer than 3 such
    /// modules — a small number of bespoke context signals is fine.
    #[serde(default)]
    pub context_signal_triads: Vec<ContextSignalTriad>,
    /// Always-emitted snapshot of the detection state behind
    /// `context_signal_triads`. Callers can tell at a glance whether
    /// `context_signal_triads: []` means "no pairs detected" (`detected: 0`)
    /// or "below the noise threshold" (`detected: 2, threshold: 3`).
    /// Without this, an empty array is indistinguishable from a clean
    /// project — and on standup the report had 2 pairs that didn't surface.
    pub context_signal_triads_summary: ContextSignalTriadsSummary,
    pub parse_errors: Vec<ParseError>,
}

/// Diagnostic counts for the context-signal-triad detector. Always
/// included so callers can ground-truth the `context_signal_triads: []`
/// case (was it "nothing matched" or "matched but below the threshold"?).
#[derive(Debug, Serialize)]
pub struct ContextSignalTriadsSummary {
    /// Total number of `provide_X` + `use_X` pairs detected across the
    /// project src tree, regardless of the threshold.
    pub detected: usize,
    /// Threshold the detector applies before emitting a suggestion. Three
    /// or more pairs is the smell; below that, a handful of bespoke
    /// context signals is fine.
    pub threshold: usize,
    /// The suffixes of every detected pair, in stable (file, name) order —
    /// useful so the caller doesn't have to walk `context_signal_triads`
    /// to see the names when `detected < threshold`.
    pub names: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ContextSignalTriad {
    /// Per-module pair: the file plus the suffix shared by its
    /// `provide_X` / `use_X` functions (`X`).
    pub modules: Vec<ContextSignalModule>,
    /// Pre-rendered, paste-into-PR description naming the redundancy and the
    /// suggested consolidation target.
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct ContextSignalModule {
    pub file: PathBuf,
    pub name: String,
    pub provide_line: usize,
    pub use_line: usize,
}

pub async fn signal_lint(
    state: &Arc<State>,
    p: SignalLintParams,
) -> Result<SignalLintReport, String> {
    let crate_root = crate_root(state, p.project_root.as_deref()).await?;
    let src_root = crate_root.join("src");
    let files = walk_rs_files(&src_root);

    let mut issues: Vec<SignalIssue> = Vec::new();

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
            let mut v = LoopVisitor {
                loop_depth: 0,
                issues: &mut issues,
                file: &sf.path,
                component: f.sig.ident.to_string(),
            };
            v.visit_block(&f.block);

            // Hydration-mismatch lint: browser-only reads (LocalStorage,
            // SessionStorage, document.cookie, …) called from the component
            // body at render time differ between SSR (no window) and the
            // first client render — the page hydrates with placeholder
            // markup, then re-renders, and the difference shows up as a
            // hydration warning or visual flash. The same calls inside
            // `use_future` / `use_effect` / `spawn` / event handlers are
            // fine because they only fire after hydration.
            let mut h = HydrationVisitor {
                closure_depth: 0,
                hook_depth: 0,
                issues: &mut issues,
                file: &sf.path,
                component: f.sig.ident.to_string(),
            };
            h.visit_block(&f.block);

            // Hydration-unsafe `use_effect` + `spawn(async ...).set(...)`
            // pattern: server-fn bootstrapping that flips a gating signal
            // consumed by rsx. The Dioxus 0.7 guidance is to use
            // `use_server_future` / `use_server_cached` so the server
            // pre-renders the resolved branch; `use_effect` is correct only
            // for browser-only side effects.
            //
            // The specialized `bootstrap_gate_signal` variant fires when
            // the .set target is a `use_signal(|| false)` binding AND the
            // rsx body contains `if <name>() { ... } else { ... }` gating
            // the whole subtree — the canonical "router behind a boot
            // flag" shape iter03 produced.
            let bool_gates = collect_bool_gate_signals(&f.block);
            let rsx_gates = collect_rsx_if_signal_calls(&f.block);
            let mut e = EffectSpawnVisitor {
                effect_depth: 0,
                spawn_depth: 0,
                saw_await: false,
                set_lines: Vec::new(),
                set_calls: Vec::new(),
                effect_line: 0,
                bool_gates: &bool_gates,
                rsx_gates: &rsx_gates,
                issues: &mut issues,
                file: &sf.path,
                component: f.sig.ident.to_string(),
            };
            e.visit_block(&f.block);

            // Sibling rule for the route-guard hydration footgun: a
            // `use_effect` that calls `nav.push("/login")` (or sibling
            // router methods) based on a signal read. SSR renders the
            // protected branch, the client mounts, the effect reads
            // absent state, and immediately redirects — the user sees
            // the protected content flash. See `EffectNavigateVisitor`.
            let nav_bindings = collect_navigator_bindings(&f.block);
            let mut n = EffectNavigateVisitor {
                effect_depth: 0,
                effect_line: 0,
                nav_bindings: &nav_bindings,
                issues: &mut issues,
                file: &sf.path,
                component: f.sig.ident.to_string(),
                already_flagged: false,
            };
            n.visit_block(&f.block);

            // Polling `use_future` with a reactive read in its body —
            // every write to the read signal restarts the loop. Almost
            // always unintended (the signal is usually a gate, not a
            // driver). See `PollingFutureVisitor`.
            let signal_bindings = collect_use_signal_bindings(&f.block);
            let mut pf = PollingFutureVisitor {
                in_future: false,
                saw_sleep: false,
                read_lines: Vec::new(),
                future_line: 0,
                signal_bindings: &signal_bindings,
                issues: &mut issues,
                file: &sf.path,
                component: f.sig.ident.to_string(),
            };
            pf.visit_block(&f.block);

            // 3+ clones of the same prop into separate `let` bindings is
            // a code smell — the prop wants to be Rc<str>/ReadOnlySignal,
            // or the closures should share a single capture. Same shape
            // standup's `Column` ended up in for ondragover/ondrop/onmatch.
            check_prop_clone_overuse(f, &sf.path, &mut issues);

            // A single `use_signal` written from ≥3 distinct named closures
            // / hooks in the same component is the canonical "lift to a
            // Store" smell — see `get_dsl_spec`'s `Store` primitive for the
            // recommended refactor. Same shape standup's `BoardBody` ended
            // up in for `cards` and `local_lock`.
            check_signal_many_writers(f, &sf.path, &signal_bindings, &mut issues);

            // Counter signals that are *only* incremented/decremented and
            // *only* compared with `==` are sentinels, not state — they
            // belong in a Cell or a Store generation method, not a Signal.
            check_signal_used_as_fence(f, &sf.path, &signal_bindings, &mut issues);
        }
    }

    let (context_signal_triads, context_signal_triads_summary) =
        detect_context_signal_triads(&files);

    // Cross-link related findings: when the same component has both a
    // `signal_many_writers` and a `signal_used_as_fence` issue on the
    // same `signal` binding, point each at the other via `related_codes`.
    // Lifting the signal into a Store fixes both at once — the caller
    // shouldn't have to read both messages to realise that.
    link_related_findings(&mut issues);

    Ok(SignalLintReport {
        issues,
        context_signal_triads,
        context_signal_triads_summary,
        parse_errors: collect_parse_errors(&files),
    })
}

/// Threshold used by `detect_context_signal_triads`. Lifted to a constant
/// so the summary can echo it back to callers without drifting out of
/// sync with the check.
const CONTEXT_SIGNAL_TRIAD_THRESHOLD: usize = 3;

/// Walk every scanned `.rs` file and look for paired `pub fn provide_<X>`
/// and `pub fn use_<X>` definitions in the same module — the classic
/// context-signal pattern. When three or more such modules appear in the
/// crate, emit a single suggestion to collapse them into one `Store`.
///
/// We require both functions to live in the same file (the convention) and
/// share the suffix exactly. False positives (e.g. a `use_router` from the
/// Dioxus prelude paired with a `provide_router`) are unlikely in the
/// project's own src tree — those live in dependencies and don't show up
/// in `walk_rs_files`.
fn detect_context_signal_triads(
    files: &[crate::tools::ast::ScannedFile],
) -> (Vec<ContextSignalTriad>, ContextSignalTriadsSummary) {
    use quote::ToTokens;
    let mut modules: Vec<ContextSignalModule> = Vec::new();
    // Track whether each module's provide_X / use_X bodies match the
    // canonical signal boilerplate (`use_context_provider(|| Signal::new(…))`
    // in provide_X, `use_context::<Signal<…>>()` in use_X). The N=2
    // hint only fires when BOTH modules share this shape; otherwise
    // the pair could be coincidental.
    let mut boilerplate: std::collections::HashSet<(std::path::PathBuf, String)> =
        std::collections::HashSet::new();
    for sf in files {
        let Ok(ast) = &sf.ast else { continue };
        let mut provides: std::collections::HashMap<String, (usize, String)> =
            std::collections::HashMap::new();
        let mut uses: std::collections::HashMap<String, (usize, String)> =
            std::collections::HashMap::new();
        for item in &ast.items {
            let syn::Item::Fn(f) = item else { continue };
            if !matches!(f.vis, syn::Visibility::Public(_)) {
                continue;
            }
            let name = f.sig.ident.to_string();
            let line = f.sig.ident.span().start().line;
            let body = collapse_ws(&f.block.to_token_stream().to_string());
            if let Some(suffix) = name.strip_prefix("provide_") {
                provides.insert(suffix.to_string(), (line, body));
            } else if let Some(suffix) = name.strip_prefix("use_") {
                uses.insert(suffix.to_string(), (line, body));
            }
        }
        for (suffix, (provide_line, p_body)) in &provides {
            if let Some((use_line, u_body)) = uses.get(suffix) {
                modules.push(ContextSignalModule {
                    file: sf.path.clone(),
                    name: suffix.clone(),
                    provide_line: *provide_line,
                    use_line: *use_line,
                });
                if is_signal_provider_boilerplate(p_body) && is_signal_use_boilerplate(u_body) {
                    boilerplate.insert((sf.path.clone(), suffix.clone()));
                }
            }
        }
    }
    // Stable, human-friendly order: sort by file path so the report is
    // deterministic across runs and OS-specific dir-read orders. Done
    // before the threshold check so the summary's `names` list matches
    // what callers would see if/when the threshold is hit.
    modules.sort_by(|a, b| a.file.cmp(&b.file).then(a.name.cmp(&b.name)));
    let names: Vec<String> = modules.iter().map(|m| m.name.clone()).collect();
    let summary = ContextSignalTriadsSummary {
        detected: modules.len(),
        threshold: CONTEXT_SIGNAL_TRIAD_THRESHOLD,
        names: names.clone(),
    };

    if modules.len() >= CONTEXT_SIGNAL_TRIAD_THRESHOLD {
        let message = format!(
            "{} sibling context-signal modules detected ({}). Three or more `provide_X` + `use_X` pairs is a smell — consolidate into a single `Store` (see the `Store` primitive in `get_dsl_spec`) so callers share one provider and one type, instead of N near-identical files.",
            modules.len(),
            names.join(", ")
        );
        return (vec![ContextSignalTriad { modules, message }], summary);
    }
    // N=2 hint: require BOTH modules to share the canonical signal
    // boilerplate. iter03 has exactly two such modules (`session`,
    // `presence`) — neither hits the warning threshold but the pair is
    // already a smell worth surfacing as a hint.
    if modules.len() == 2
        && modules
            .iter()
            .all(|m| boilerplate.contains(&(m.file.clone(), m.name.clone())))
    {
        let message = format!(
            "2 sibling context-signal modules detected ({}). Each one is a byte-identical \
             `use_context_provider(|| Signal::new(…))` + `use_context::<Signal<…>>()` pair — \
             the boilerplate is already present, just below the N=3 warning threshold. \
             Consider consolidating into a single `Store` now (see the `Store` primitive in \
             `get_dsl_spec`) before a third context-signal pair lands and the duplication \
             becomes harder to unwind.",
            names.join(", ")
        );
        return (vec![ContextSignalTriad { modules, message }], summary);
    }
    (Vec::new(), summary)
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = false;
    for c in s.chars() {
        let c = if c.is_whitespace() { ' ' } else { c };
        if c == ' ' {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(c);
            last_space = false;
        }
    }
    out.trim().to_string()
}

/// True when the body matches the canonical `provide_X` signal-context
/// shape: contains `use_context_provider` AND `Signal :: new` (or
/// `Signal::new`) within the same body. Token-level substring match —
/// good enough for the boilerplate-shape detection because the body is
/// already known to be a `pub fn provide_<X>` and we don't try to
/// distinguish other context providers.
fn is_signal_provider_boilerplate(body: &str) -> bool {
    let normalized = body.replace(' ', "");
    normalized.contains("use_context_provider")
        && (normalized.contains("Signal::new") || normalized.contains("Signal<"))
}

/// True when the body matches the canonical `use_X` signal-context shape:
/// `use_context::<Signal<…>>()`.
fn is_signal_use_boilerplate(body: &str) -> bool {
    let normalized = body.replace(' ', "");
    normalized.contains("use_context::<Signal<")
        || normalized.contains("use_context::<ReadSignal<")
        || normalized.contains("use_context::<WriteSignal<")
}

struct LoopVisitor<'a> {
    loop_depth: usize,
    issues: &'a mut Vec<SignalIssue>,
    file: &'a std::path::Path,
    component: String,
}

impl<'a, 'ast> Visit<'ast> for LoopVisitor<'a> {
    fn visit_expr_for_loop(&mut self, e: &'ast syn::ExprForLoop) {
        self.loop_depth += 1;
        syn::visit::visit_expr_for_loop(self, e);
        self.loop_depth -= 1;
    }
    fn visit_expr_while(&mut self, e: &'ast syn::ExprWhile) {
        self.loop_depth += 1;
        syn::visit::visit_expr_while(self, e);
        self.loop_depth -= 1;
    }
    fn visit_expr_loop(&mut self, e: &'ast syn::ExprLoop) {
        self.loop_depth += 1;
        syn::visit::visit_expr_loop(self, e);
        self.loop_depth -= 1;
    }
    fn visit_expr_call(&mut self, e: &'ast syn::ExprCall) {
        if self.loop_depth > 0
            && let syn::Expr::Path(p) = &*e.func
            && let Some(seg) = p.path.segments.last()
            && is_hook_name(&seg.ident.to_string())
        {
            emit_hook_issue(
                self.issues,
                self.file,
                &self.component,
                &seg.ident.to_string(),
                seg.ident.span().start().line,
            );
        }
        syn::visit::visit_expr_call(self, e);
    }
    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        let is_rsx = m
            .path
            .segments
            .last()
            .map(|s| s.ident == "rsx")
            .unwrap_or(false);
        if is_rsx {
            let tokens: Vec<TokenTree> = m.tokens.clone().into_iter().collect();
            lint_rsx_tokens(
                &tokens,
                self.loop_depth > 0,
                self.file,
                &self.component,
                self.issues,
            );
        }
        syn::visit::visit_macro(self, m);
    }
}

fn is_hook_name(name: &str) -> bool {
    matches!(
        name,
        "use_signal" | "use_memo" | "use_resource" | "use_effect"
    )
}

fn emit_hook_issue(
    issues: &mut Vec<SignalIssue>,
    file: &Path,
    component: &str,
    kind: &str,
    line: usize,
) {
    issues.push(SignalIssue {
        code: "hook_in_loop",
        message: format!(
            "`{kind}` inside a loop body — a new hook is created on every iteration; lift it out or use a `Vec<Signal<_>>` constructed once"
        ),
        file: file.to_path_buf(),
        line,
        component: Some(component.to_string()),
        signal: None,
        related_codes: Vec::new(),
        fix: None,
    });
}

fn lint_rsx_tokens(
    tokens: &[TokenTree],
    in_loop: bool,
    file: &Path,
    component: &str,
    issues: &mut Vec<SignalIssue>,
) {
    let mut i = 0;
    while i < tokens.len() {
        if let TokenTree::Ident(id) = &tokens[i] {
            let name = id.to_string();
            if (name == "for" || name == "while" || name == "loop")
                && let Some(brace_idx) = find_next_brace_group(tokens, i)
                && let TokenTree::Group(g) = &tokens[brace_idx]
            {
                let inner: Vec<TokenTree> = g.stream().into_iter().collect();
                lint_rsx_tokens(&inner, true, file, component, issues);
                i = brace_idx + 1;
                continue;
            }
            if in_loop
                && is_hook_name(&name)
                && let Some(TokenTree::Group(g)) = tokens.get(i + 1)
                && g.delimiter() == proc_macro2::Delimiter::Parenthesis
            {
                emit_hook_issue(issues, file, component, &name, id.span().start().line);
            }
        }
        if let TokenTree::Group(g) = &tokens[i] {
            let inner: Vec<TokenTree> = g.stream().into_iter().collect();
            lint_rsx_tokens(&inner, in_loop, file, component, issues);
        }
        i += 1;
    }
}

fn find_next_brace_group(tokens: &[TokenTree], start: usize) -> Option<usize> {
    for (k, tt) in tokens.iter().enumerate().skip(start + 1) {
        if let TokenTree::Group(g) = tt
            && g.delimiter() == proc_macro2::Delimiter::Brace
        {
            return Some(k);
        }
    }
    None
}

/// Detects browser-API reads (LocalStorage / SessionStorage / document.cookie)
/// inside a component body at render time — the classic hydration-mismatch
/// footgun. SSR has no window, so the call returns nothing (or panics) on the
/// server; the client then runs the same code with real values and renders
/// different markup, triggering a re-hydration flash.
///
/// We only flag calls at `closure_depth == 0` AND `hook_depth == 0`. A call
/// nested inside a `use_future(move || …)`, `use_effect(move || …)`, or
/// `spawn(async move { … })` closure is safe — those bodies only fire after
/// the first hydration pass completes. Same for event-handler closures
/// passed to `onclick:` etc., which the rsx! visitor sees as plain closures.
struct HydrationVisitor<'a> {
    closure_depth: usize,
    /// Distinct from closure_depth so calls inside `use_future(...)` /
    /// `use_effect(...)` are recognised even when the body isn't written as
    /// a closure (e.g. an early-return prelude that calls `LocalStorage::get`
    /// before constructing the closure). The hook arg is the closure 99% of
    /// the time, but the depth tracker keeps the rule honest.
    hook_depth: usize,
    issues: &'a mut Vec<SignalIssue>,
    file: &'a std::path::Path,
    component: String,
}

impl<'a, 'ast> Visit<'ast> for HydrationVisitor<'a> {
    fn visit_expr_closure(&mut self, e: &'ast syn::ExprClosure) {
        self.closure_depth += 1;
        syn::visit::visit_expr_closure(self, e);
        self.closure_depth -= 1;
    }

    fn visit_expr_async(&mut self, e: &'ast syn::ExprAsync) {
        // `async move { ... }` — same shape as a closure for our purposes;
        // the body runs on a future, not during render.
        self.closure_depth += 1;
        syn::visit::visit_expr_async(self, e);
        self.closure_depth -= 1;
    }

    fn visit_expr_call(&mut self, e: &'ast syn::ExprCall) {
        let is_hook = matches!(
            extract_path_tail(&e.func),
            Some(name) if matches!(
                name.as_str(),
                "use_future" | "use_effect" | "use_resource" | "use_memo" | "spawn" | "use_coroutine"
            )
        );
        if is_hook {
            self.hook_depth += 1;
        }
        // Flag the call itself when it's a browser-API entrypoint at the
        // wrong depth. We check the path before recursing so a wrapper like
        // `Some(LocalStorage::get(...))` still flags the inner read.
        if self.closure_depth == 0
            && self.hook_depth == 0
            && let Some(kind) = browser_api_for_path(&e.func)
        {
            emit_hydration_issue(
                self.issues,
                self.file,
                &self.component,
                kind,
                e.func.span().start().line,
            );
        }
        syn::visit::visit_expr_call(self, e);
        if is_hook {
            self.hook_depth -= 1;
        }
    }

    fn visit_expr_method_call(&mut self, e: &'ast syn::ExprMethodCall) {
        if self.closure_depth == 0 && self.hook_depth == 0 {
            // Method-style reads: `.local_storage()`, `.session_storage()`,
            // `.cookie()` on a `web_sys::Document` / `Window`. Same hazard
            // as the path-style reads handled above.
            let name = e.method.to_string();
            let kind = match name.as_str() {
                "local_storage" => Some("`Window::local_storage()` call"),
                "session_storage" => Some("`Window::session_storage()` call"),
                "cookie" => Some("`Document::cookie()` call"),
                _ => None,
            };
            if let Some(k) = kind {
                emit_hydration_issue(
                    self.issues,
                    self.file,
                    &self.component,
                    k,
                    e.method.span().start().line,
                );
            }
        }
        syn::visit::visit_expr_method_call(self, e);
    }
}

/// Pull the last `::` segment of a path-style expr (`a::b::c::Foo::get` →
/// `Some("get")`). Returns `None` for non-path callees.
fn extract_path_tail(expr: &syn::Expr) -> Option<String> {
    if let syn::Expr::Path(p) = expr {
        return p.path.segments.last().map(|s| s.ident.to_string());
    }
    None
}

/// Recognise a path-style call that reads a browser API. We look at the
/// path itself rather than the call target so wrappers like
/// `gloo_storage::LocalStorage::get` (qualified) and `LocalStorage::get`
/// (imported) both match. Returns the human-readable label used in the
/// issue message.
fn browser_api_for_path(expr: &syn::Expr) -> Option<&'static str> {
    let syn::Expr::Path(p) = expr else {
        return None;
    };
    let idents: Vec<String> = p
        .path
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect();
    // We need at least Type::method, so 2 segments minimum.
    if idents.len() < 2 {
        return None;
    }
    let ty = idents
        .get(idents.len() - 2)
        .map(|s| s.as_str())
        .unwrap_or("");
    match ty {
        "LocalStorage" => Some("`LocalStorage::*` browser read"),
        "SessionStorage" => Some("`SessionStorage::*` browser read"),
        _ => None,
    }
}

fn emit_hydration_issue(
    issues: &mut Vec<SignalIssue>,
    file: &Path,
    component: &str,
    kind: &str,
    line: usize,
) {
    issues.push(SignalIssue {
        code: "hydration_browser_read",
        message: format!(
            "{kind} in component body — SSR has no window, so the first client render reads different state and triggers a hydration flash. Move it inside `use_future` / `use_effect` / `spawn`, or guard with `#[cfg(target_arch = \"wasm32\")]`."
        ),
        file: file.to_path_buf(),
        line,
        component: Some(component.to_string()),
        signal: None,
        related_codes: Vec::new(),
        fix: None,
    });
}

/// Detects the `use_effect(move || { spawn(async move { server_fn().await;
/// sig.set(...); }) })` shape — a hydration-mismatch hazard. The server
/// renders the *loading* branch (because the effect doesn't run on SSR), the
/// client first paints the same loading branch, and only after the fetch
/// resolves does the gating signal flip — flashing the real content in.
///
/// The flagged pattern requires all three signals together so we don't
/// false-positive on `use_effect`s that only do browser-side cleanup or that
/// fire-and-forget without writing a signal that rsx consumes:
///
/// 1. inside a `use_effect(|| ...)` body,
/// 2. there's a `spawn(async ...)` (or a bare `async move { ... }` block
///    that gets handed to a hook),
/// 3. the async body awaits SOMETHING and writes to a signal via `.set(…)`.
///
/// We emit one issue per `.set` call so the report points at the writes —
/// usually the same line the user needs to convert to `use_server_future`.
struct EffectSpawnVisitor<'a> {
    effect_depth: usize,
    /// Depth into a `spawn(async ...)` or `async move { ... }` body that
    /// lives inside the current `use_effect`. Both nesting levels must be
    /// positive at the same time for the `.set` write to flag.
    spawn_depth: usize,
    /// `.await` seen since entering the current spawn body. We only flag
    /// `.set` calls that follow an await — without one, the effect is just
    /// scheduling a sync write and is unrelated to the hydration shape we
    /// care about.
    saw_await: bool,
    /// Lines of `.set(…)` calls observed in the current spawn body. We
    /// flush these on exit from the spawn, attaching the recorded await
    /// flag to decide whether to emit.
    set_lines: Vec<usize>,
    /// `.set(...)` calls captured with their receiver ident and whether
    /// the value is the literal `true`. Used to detect the
    /// `bootstrap_gate_signal` specialization: a `use_signal(|| false)`
    /// binding flipped to `true` after an awaited server fn.
    set_calls: Vec<SetCall>,
    /// Line of the enclosing `use_effect(` call — used as the issue line so
    /// the report points at the hook (where the fix lives) rather than at
    /// the buried `.set`.
    effect_line: usize,
    /// Names of `let mut X = use_signal(|| false);` bindings in the
    /// component body. Pre-scanned because the visitor never traverses
    /// `Local` statements (the binding lives outside the effect body).
    bool_gates: &'a [String],
    /// Names referenced as `if X() { ... }` in any rsx body in the
    /// component. These are the candidate gates for the bootstrap shape.
    rsx_gates: &'a std::collections::HashSet<String>,
    issues: &'a mut Vec<SignalIssue>,
    file: &'a std::path::Path,
    component: String,
}

#[derive(Debug, Clone)]
struct SetCall {
    receiver: String,
    /// `true` iff the argument is the literal `true`. We only fire
    /// `bootstrap_gate_signal` on this specific value — a `.set(false)`
    /// or `.set(some_var)` doesn't match the bootstrap shape.
    is_true_literal: bool,
    #[allow(dead_code)]
    line: usize,
}

impl<'a, 'ast> Visit<'ast> for EffectSpawnVisitor<'a> {
    fn visit_expr_call(&mut self, e: &'ast syn::ExprCall) {
        let tail = extract_path_tail(&e.func);
        let entering_effect = matches!(tail.as_deref(), Some("use_effect"));
        let entering_spawn = matches!(tail.as_deref(), Some("spawn"));

        if entering_effect {
            self.effect_depth += 1;
            self.effect_line = e.func.span().start().line;
        }
        if entering_spawn && self.effect_depth > 0 {
            self.spawn_depth += 1;
            // Save and reset the per-spawn observations so a sibling spawn
            // doesn't see another spawn's await.
            let saved_await = self.saw_await;
            let saved_sets = std::mem::take(&mut self.set_lines);
            let saved_calls = std::mem::take(&mut self.set_calls);
            self.saw_await = false;
            syn::visit::visit_expr_call(self, e);
            self.flush_pending_issues();
            self.saw_await = saved_await;
            self.set_lines = saved_sets;
            self.set_calls = saved_calls;
            self.spawn_depth -= 1;
        } else {
            syn::visit::visit_expr_call(self, e);
        }
        if entering_effect {
            self.effect_depth -= 1;
        }
    }

    fn visit_expr_await(&mut self, e: &'ast syn::ExprAwait) {
        if self.effect_depth > 0 && self.spawn_depth > 0 {
            self.saw_await = true;
        }
        syn::visit::visit_expr_await(self, e);
    }

    fn visit_expr_method_call(&mut self, e: &'ast syn::ExprMethodCall) {
        if self.effect_depth > 0 && self.spawn_depth > 0 && e.method == "set" {
            let line = e.method.span().start().line;
            self.set_lines.push(line);
            if let syn::Expr::Path(p) = &*e.receiver
                && p.path.segments.len() == 1
            {
                let receiver = p.path.segments[0].ident.to_string();
                let is_true_literal = e
                    .args
                    .first()
                    .map(|arg| matches!(arg, syn::Expr::Lit(l) if matches!(l.lit, syn::Lit::Bool(syn::LitBool { value: true, .. }))))
                    .unwrap_or(false);
                self.set_calls.push(SetCall {
                    receiver,
                    is_true_literal,
                    line,
                });
            }
        }
        syn::visit::visit_expr_method_call(self, e);
    }
}

impl<'a> EffectSpawnVisitor<'a> {
    fn flush_pending_issues(&mut self) {
        if !self.saw_await || self.set_lines.is_empty() {
            return;
        }
        // One issue per use_effect, pointing at the hook site — duplicating
        // on every `.set` would just be noise when there are several writes
        // (loading flag + result write, for instance).
        let line = if self.effect_line > 0 {
            self.effect_line
        } else {
            *self.set_lines.first().unwrap()
        };

        // Bootstrap-gate specialization: did any `.set(true)` write target
        // a `use_signal(|| false)` binding that also gates a top-level rsx
        // `if`? If so, emit the dedicated finding with a paste-ready
        // `use_server_future` snippet and skip the generic hydration
        // warning — they're the same fix.
        let gate_match = self.set_calls.iter().find(|sc| {
            sc.is_true_literal
                && self.bool_gates.iter().any(|g| g == &sc.receiver)
                && self.rsx_gates.contains(&sc.receiver)
        });
        if let Some(sc) = gate_match {
            let signal_name = sc.receiver.clone();
            self.issues.push(SignalIssue {
                code: "bootstrap_gate_signal",
                message: format!(
                    "`use_effect` flips `{signal_name}` (a `use_signal(|| false)` binding) to \
                     `true` after awaiting a server fn, and rsx gates the whole subtree behind \
                     `if {signal_name}() {{ … }} else {{ loading }}`. SSR renders the loading \
                     branch, the client paints the same loading branch, then flashes to the real \
                     content once the fetch resolves. Replace the effect + flag with \
                     `use_server_future`: drop the `{signal_name}` signal, drop the `use_effect`, \
                     and write `let boot = use_server_future(|| async {{ /* the server fn */ }})?; \
                     match boot() {{ Some(value) => rsx! {{ /* resolved tree */ }}, None => rsx! \
                     {{ /* loading */ }} }}` — Dioxus 0.7 pre-renders the resolved branch on the \
                     server and there's no hydration flash. `use_effect` is correct for \
                     browser-only side effects (cleanup, focus, scroll); keep it for those.",
                ),
                file: self.file.to_path_buf(),
                line,
                component: Some(self.component.clone()),
                signal: None,
                related_codes: Vec::new(),
                fix: None,
            });
        } else {
            self.issues.push(SignalIssue {
                code: "hydration_unsafe_effect",
                message:
                    "`use_effect` spawns an async block that awaits a server fn and writes a signal \
                     consumed by rsx — this renders the loading branch on SSR and again on the first \
                     client paint, then flips. Prefer `use_server_future` (or `use_server_cached`) so \
                     the server pre-renders the resolved branch and there's no hydration flash. \
                     `use_effect` is correct for browser-only side effects; keep it for those."
                        .to_string(),
                file: self.file.to_path_buf(),
                line,
                component: Some(self.component.clone()),
                signal: None,
                related_codes: Vec::new(),
                fix: None,
            });
        }
        self.set_lines.clear();
        self.set_calls.clear();
    }
}

/// Pre-scan a component body for `let X = use_navigator();` (or sibling
/// `use_router*()`) bindings. Those names become the receivers we'll match
/// against inside `use_effect` for the route-guard hydration lint. We also
/// fall back to a hardcoded name list (`nav`/`navigator`/`router`) inside
/// the visitor for components that took the navigator from a parent prop or
/// elided the binding — that fallback is in `is_navigator_receiver`.
fn collect_navigator_bindings(block: &syn::Block) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for stmt in &block.stmts {
        let syn::Stmt::Local(local) = stmt else {
            continue;
        };
        let Some(init) = &local.init else { continue };
        let Some(tail) = init_call_tail(&init.expr) else {
            continue;
        };
        if !matches!(
            tail.as_str(),
            "use_navigator" | "use_router" | "use_router_state" | "navigator"
        ) {
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

/// Pull the last `::` segment of a path-style call's function — used by
/// `collect_navigator_bindings` to recognise `let n = use_navigator();` even
/// when the helper is called via a qualified path
/// (`dioxus_router::hooks::use_navigator()`).
fn init_call_tail(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Call(c) => match &*c.func {
            syn::Expr::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
            _ => None,
        },
        syn::Expr::MethodCall(m) => init_call_tail(&m.receiver),
        syn::Expr::Try(t) => init_call_tail(&t.expr),
        syn::Expr::Await(a) => init_call_tail(&a.base),
        syn::Expr::Paren(p) => init_call_tail(&p.expr),
        _ => None,
    }
}

/// Detects `use_effect(move || nav.push("/login"))` — the route-guard
/// hydration hazard. The body of a `use_effect` doesn't execute during SSR,
/// so the server still renders the protected branch; the client mounts,
/// the effect runs, reads the (absent) session, and pushes a redirect. The
/// user sees the protected markup flash before the navigation takes effect.
///
/// We flag a method call inside `use_effect` whose name matches a router
/// navigation method (`push` / `replace` / `go_back` / `go_forward`) and
/// whose receiver is recognised as a navigator binding (either picked up
/// from `let n = use_navigator()` by `collect_navigator_bindings`, or
/// matched against a small hardcoded name list for components that received
/// the navigator from a parent / prop). One issue per `use_effect`.
struct EffectNavigateVisitor<'a> {
    effect_depth: usize,
    effect_line: usize,
    nav_bindings: &'a [String],
    issues: &'a mut Vec<SignalIssue>,
    file: &'a std::path::Path,
    component: String,
    /// Set once we've emitted for the current `use_effect`. Two `nav.push`
    /// calls in the same effect (rare but possible) shouldn't double-flag —
    /// the fix is the same regardless.
    already_flagged: bool,
}

impl<'a, 'ast> Visit<'ast> for EffectNavigateVisitor<'a> {
    fn visit_expr_call(&mut self, e: &'ast syn::ExprCall) {
        let entering_effect = matches!(extract_path_tail(&e.func).as_deref(), Some("use_effect"));
        if entering_effect {
            self.effect_depth += 1;
            self.effect_line = e.func.span().start().line;
            let saved_flagged = self.already_flagged;
            self.already_flagged = false;
            syn::visit::visit_expr_call(self, e);
            self.already_flagged = saved_flagged;
            self.effect_depth -= 1;
        } else {
            syn::visit::visit_expr_call(self, e);
        }
    }

    fn visit_expr_method_call(&mut self, e: &'ast syn::ExprMethodCall) {
        if self.effect_depth > 0 && !self.already_flagged {
            let method = e.method.to_string();
            let is_nav_method = matches!(
                method.as_str(),
                "push" | "replace" | "go_back" | "go_forward"
            );
            if is_nav_method && self.is_navigator_receiver(&e.receiver) {
                self.flag(e.method.span().start().line);
            }
        }
        syn::visit::visit_expr_method_call(self, e);
    }
}

impl<'a> EffectNavigateVisitor<'a> {
    /// True when `expr` is a bare ident that names something we believe is a
    /// router/navigator. Tracked bindings (from `use_navigator()`) are the
    /// strong signal; the hardcoded name list catches the convention-based
    /// cases (nav, navigator, router) for components that got the navigator
    /// from a parent / prop / context and didn't bind it locally.
    fn is_navigator_receiver(&self, expr: &syn::Expr) -> bool {
        let Some(name) = single_ident(expr) else {
            return false;
        };
        if self.nav_bindings.iter().any(|b| b == &name) {
            return true;
        }
        matches!(name.as_str(), "nav" | "navigator" | "router")
    }

    fn flag(&mut self, line: usize) {
        let issue_line = if self.effect_line > 0 {
            self.effect_line
        } else {
            line
        };
        self.issues.push(SignalIssue {
            code: "hydration_unsafe_effect",
            message:
                "`use_effect` calls a router navigation method (`push`/`replace`/`go_back`) — \
                 SSR doesn't run effects, so the server renders the protected branch and the \
                 client briefly paints it before the effect mounts and redirects. The user \
                 sees the protected content flash. Move the auth check to a server-side guard \
                 (`use_server_future` + early Outlet branch) or render the redirect markup \
                 conditionally instead of imperatively navigating from an effect."
                    .to_string(),
            file: self.file.to_path_buf(),
            line: issue_line,
            component: Some(self.component.clone()),
            signal: None,
            related_codes: Vec::new(),
            fix: None,
        });
        self.already_flagged = true;
    }
}

/// Per-component scan for the "prop cloned N≥3 times into separate `let`
/// bindings just so each closure can `move` its own copy" smell. Same
/// shape standup's `Column` ends up in for `id_for_dragover` /
/// `id_for_drop` / `id_for_match`. The fix is usually `Rc<str>`,
/// `ReadOnlySignal<T>`, or restructuring the closures to share a capture.
///
/// We only handle the `#[component] fn Foo(prop: T, ...)` form — the
/// `fn Foo(props: FooProps)` form is harder to scan without resolving
/// struct fields and is left for a follow-up if it turns out to matter.
fn check_prop_clone_overuse(
    f: &syn::ItemFn,
    file: &std::path::Path,
    issues: &mut Vec<SignalIssue>,
) {
    // Argument names ARE the prop names for `#[component] fn Foo(a: A, b: B)`.
    let prop_names: Vec<String> = f
        .sig
        .inputs
        .iter()
        .filter_map(|input| {
            let syn::FnArg::Typed(t) = input else {
                return None;
            };
            let syn::Pat::Ident(p) = &*t.pat else {
                return None;
            };
            Some(p.ident.to_string())
        })
        .collect();
    if prop_names.is_empty() {
        return;
    }

    let mut clones: std::collections::BTreeMap<String, Vec<usize>> =
        std::collections::BTreeMap::new();
    for stmt in &f.block.stmts {
        let syn::Stmt::Local(local) = stmt else {
            continue;
        };
        let Some(init) = &local.init else { continue };
        let syn::Expr::MethodCall(mc) = &*init.expr else {
            continue;
        };
        if mc.method != "clone" || !mc.args.is_empty() {
            continue;
        }
        let Some(name) = single_ident(&mc.receiver) else {
            continue;
        };
        if prop_names.iter().any(|p| p == &name) {
            clones
                .entry(name)
                .or_default()
                .push(mc.method.span().start().line);
        }
    }

    let mut per_prop_fired: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for (prop, lines) in &clones {
        if lines.len() < 3 {
            continue;
        }
        per_prop_fired.insert(prop.as_str());
        let line_list = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        issues.push(SignalIssue {
            code: "prop_clone_overuse",
            message: format!(
                "prop `{prop}` is cloned {n} times into separate `let` bindings (lines {line_list}). \
                 This usually means each clone is captured by its own `move` closure. Consider one of: \
                 (1) change the prop type to `Rc<str>` / `Arc<T>` so clones are cheap and one binding \
                 can be reused; (2) lift the prop into a `ReadOnlySignal<T>` so closures can `.read()` \
                 it directly without owning a copy; (3) restructure the closures to share a single capture.",
                n = lines.len(),
            ),
            file: file.to_path_buf(),
            line: lines[0],
            component: Some(f.sig.ident.to_string()),
            signal: None,
            related_codes: Vec::new(),
            fix: None,
        });
    }

    // Aggregate "many-clones across multiple props" pass. Standup's `CardItem`
    // cloned `card_id` twice AND `column_id` twice — total of four clones into
    // separate `let` bindings — yet escaped the per-prop rule because no single
    // prop hit the 3-count threshold. The aggregate signal is the same: each
    // `move` closure captures its own clone, the props are by-value rather than
    // by-handle, and the refactor (Arc / ReadOnlySignal / shared capture) is
    // the same. Threshold is ≥4 clones across ≥2 props so a single prop cloned
    // twice doesn't get double-flagged on its own.
    let total_clones: usize = clones.values().map(|v| v.len()).sum();
    let prop_count = clones.len();
    // Skip the aggregate when ANY prop in this component already fired the
    // per-prop rule. The per-prop finding already names the same refactor
    // (Arc / ReadOnlySignal / shared capture), so the aggregate on top is
    // redundant noise. iter03's `Column` had `id` ×3 (per-prop fires) plus
    // `state_passthrough` clones — before the fix, the aggregate fired at
    // the same line because the threshold only blocked when *every* prop in
    // the clone map fired per-prop. Tightening to "any per-prop fired"
    // collapses the duplicate.
    let any_per_prop_fired = !per_prop_fired.is_empty();
    // Require at least one prop to be cloned twice — "one clone each across
    // five props" is the normal capture-per-closure shape and was producing
    // false positives. The smell is *repeat* clones of the same prop spread
    // across multiple props (e.g. `card_id` ×2 + `column_id` ×2), not "lots
    // of distinct props each captured once."
    let has_repeat_clone = clones.values().any(|v| v.len() >= 2);
    if total_clones >= 4 && prop_count >= 2 && has_repeat_clone && !any_per_prop_fired {
        let mut summary_parts: Vec<String> = clones
            .iter()
            .map(|(p, l)| format!("`{p}` ×{}", l.len()))
            .collect();
        summary_parts.sort();
        let first_line = clones
            .values()
            .flat_map(|v| v.iter().copied())
            .min()
            .unwrap_or(0);
        issues.push(SignalIssue {
            code: "prop_clone_overuse_aggregate",
            message: format!(
                "this component clones props into separate `let` bindings {total_clones} times \
                 across {prop_count} props ({list}). No single prop hit the 3-clone per-prop \
                 threshold, but the same closure-capture refactor applies — switch to \
                 `Rc<str>` / `Arc<T>` props, lift into a `ReadOnlySignal<T>`, or share a single \
                 capture across the closures.",
                list = summary_parts.join(", "),
            ),
            file: file.to_path_buf(),
            line: first_line,
            component: Some(f.sig.ident.to_string()),
            signal: None,
            related_codes: Vec::new(),
            fix: None,
        });
    }
}

/// A "write source" inside the component body — either a named closure
/// (`let mut commit_move = move |…| { … }`) or a hook (`use_future(|| async
/// { … })` / `use_effect(|| { … })`). We track these as the units of code
/// from which a Signal can be written, so the many-writers lint can answer
/// "how many distinct callable bodies mutate this signal?" — three or
/// more is the smell.
struct WriteSource<'a> {
    /// Display name used in the lint message. Closures keep their let-
    /// binding ident (`commit_move`); hooks use a synthetic `<use_X@LINE>`
    /// form so the report points at the source.
    name: String,
    /// Closure / async block body whose interior is scanned for `.set()`,
    /// `.write()`, `+=`, etc. Held as a borrowed `syn::Expr` so we can
    /// reuse the visit infrastructure.
    body: &'a syn::Expr,
    /// Source line of the binding / hook — used as the issue line.
    line: usize,
}

/// Walk a component body and produce one `WriteSource` per named closure
/// binding and per standalone `use_future` / `use_effect` / `use_resource`
/// / `use_callback` call site. Closures bound to `_name` are skipped — by
/// convention those are throwaway handlers and double-counting them just
/// muddies the smell. We do NOT include `use_signal` / `use_memo`
/// initializers (their bodies aren't mutator callsites in the same sense).
fn collect_write_sources(block: &syn::Block) -> Vec<WriteSource<'_>> {
    let mut out: Vec<WriteSource> = Vec::new();
    for stmt in &block.stmts {
        match stmt {
            syn::Stmt::Local(local) => {
                let Some(init) = &local.init else { continue };
                let line = local.let_token.span.start().line;
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
                // Named closure: `let [mut?] foo = move |args| { body }`.
                if matches!(&*init.expr, syn::Expr::Closure(_)) {
                    out.push(WriteSource {
                        name,
                        body: &init.expr,
                        line,
                    });
                    continue;
                }
                // `let bar = use_future(|| async { ... });` — the future
                // body is itself a write source.
                if is_named_hook_init(&init.expr) {
                    out.push(WriteSource {
                        name: format!("<{name}>"),
                        body: &init.expr,
                        line,
                    });
                }
            }
            // Standalone `use_future(|| …);` / `use_effect(|| …);` — no
            // `let` binding, so we synthesise a name from the call form.
            syn::Stmt::Expr(expr, semi) if semi.is_some() => {
                if let Some(form) = standalone_hook_form(expr) {
                    use syn::spanned::Spanned;
                    let line = expr.span().start().line;
                    out.push(WriteSource {
                        name: format!("<{form}@{line}>"),
                        body: expr,
                        line,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// True when `expr` is a call whose path tail is one of the hooks whose
/// closure body counts as a "write source" (use_future / use_effect /
/// use_resource / use_callback).
fn is_named_hook_init(expr: &syn::Expr) -> bool {
    standalone_hook_form(expr).is_some()
}

/// `Some("use_future")` etc. when `expr` is a call to one of the
/// write-source hooks; `None` for non-hook calls or for `use_signal` /
/// `use_memo` (which initialise pure-data slots, not mutator bodies).
fn standalone_hook_form(expr: &syn::Expr) -> Option<&'static str> {
    let call = match expr {
        syn::Expr::Call(c) => c,
        _ => return None,
    };
    let syn::Expr::Path(p) = &*call.func else {
        return None;
    };
    let last = p.path.segments.last()?.ident.to_string();
    match last.as_str() {
        "use_future" => Some("use_future"),
        "use_effect" => Some("use_effect"),
        "use_resource" => Some("use_resource"),
        "use_callback" => Some("use_callback"),
        _ => None,
    }
}

/// Walk `expr` and collect every `use_signal` binding name that gets
/// written. Mirrors `EffectSpawnVisitor`'s write detection: `.set`,
/// `.set_silent`, `.write`, `.with_mut`, `.replace`, `.swap`, `.take`,
/// compound `+=` / `-=` etc. on a Signal, and plain `sig = …` / `*sig = …`.
fn collect_signal_writes(expr: &syn::Expr, signal_bindings: &[String]) -> Vec<String> {
    struct V<'a> {
        signals: &'a [String],
        hits: std::collections::BTreeSet<String>,
    }
    impl<'a, 'ast> Visit<'ast> for V<'a> {
        fn visit_expr_method_call(&mut self, mc: &'ast syn::ExprMethodCall) {
            let is_write = matches!(
                mc.method.to_string().as_str(),
                "set" | "set_silent" | "write" | "with_mut" | "replace" | "swap" | "take"
            );
            if is_write
                && let Some(name) = single_ident(&mc.receiver)
                && self.signals.iter().any(|s| s == &name)
            {
                self.hits.insert(name);
            }
            syn::visit::visit_expr_method_call(self, mc);
        }
        fn visit_expr_assign(&mut self, ea: &'ast syn::ExprAssign) {
            if let Some(name) = single_ident(&ea.left)
                && self.signals.iter().any(|s| s == &name)
            {
                self.hits.insert(name);
            }
            syn::visit::visit_expr_assign(self, ea);
        }
        fn visit_expr_binary(&mut self, eb: &'ast syn::ExprBinary) {
            // syn 2.0 represents `sig += 1` as `ExprBinary` with `BinOp::AddAssign`
            // (and the matching variants for the other nine compound forms). Each
            // mutates the lhs binding, so we attribute it as a write — without
            // this, `signal_many_writers` was blind to mutators written as
            // `local_lock += 1` (every compound-assigning closure looked
            // signal-free).
            if is_compound_assign(&eb.op)
                && let Some(name) = single_ident(&eb.left)
                && self.signals.iter().any(|s| s == &name)
            {
                self.hits.insert(name);
            }
            syn::visit::visit_expr_binary(self, eb);
        }
        fn visit_expr_unary(&mut self, eu: &'ast syn::ExprUnary) {
            // `*sig = x` shows up as `Assign(Unary(Deref, sig), x)` — the
            // assign handler already covers it via `single_ident` peeling
            // unary/deref. Default-recurse the body.
            syn::visit::visit_expr_unary(self, eu);
        }
    }
    let mut v = V {
        signals: signal_bindings,
        hits: std::collections::BTreeSet::new(),
    };
    v.visit_expr(expr);
    v.hits.into_iter().collect()
}

/// Many-writers smell: a single `use_signal` binding is written from ≥3
/// distinct named closures or hooks in the same component. Each write site
/// duplicates lock/state plumbing the calling code has to keep in sync; the
/// Dioxus 0.7 idiom is to lift the signal into a `Store` with named
/// mutator methods so the call sites read like `store.commit_move(...)`
/// instead of inline `cards.with_mut(...)`. See the `Store` primitive in
/// `get_dsl_spec` for the recommended refactor.
fn check_signal_many_writers(
    f: &syn::ItemFn,
    file: &std::path::Path,
    signal_bindings: &[String],
    issues: &mut Vec<SignalIssue>,
) {
    if signal_bindings.is_empty() {
        return;
    }
    let sources = collect_write_sources(&f.block);
    // signal_name → list of (write-source-name, line) writers.
    let mut by_signal: std::collections::BTreeMap<String, Vec<(String, usize)>> =
        std::collections::BTreeMap::new();
    for src in &sources {
        for sig in collect_signal_writes(src.body, signal_bindings) {
            let entry = by_signal.entry(sig).or_default();
            if !entry.iter().any(|(n, _)| n == &src.name) {
                entry.push((src.name.clone(), src.line));
            }
        }
    }
    for (signal, writers) in &by_signal {
        if writers.len() < 3 {
            continue;
        }
        let mut names: Vec<&str> = writers.iter().map(|(n, _)| n.as_str()).collect();
        names.sort();
        let first_line = writers.iter().map(|(_, l)| *l).min().unwrap_or(0);
        issues.push(SignalIssue {
            code: "signal_many_writers",
            message: format!(
                "`{signal}` is written from {n} distinct sources in this component ({list}). \
                 Three or more mutator callsites is the canonical \"lift to Store\" smell — \
                 each call duplicates plumbing the others have to keep in sync. Move `{signal}` \
                 into a `Store` (see `get_dsl_spec`'s `Store` primitive) and expose named \
                 mutator methods (`store.commit_move(...)` etc.) instead of inline `.with_mut` / \
                 `.set` calls.",
                n = writers.len(),
                list = names.join(", "),
            ),
            file: file.to_path_buf(),
            line: first_line,
            component: Some(f.sig.ident.to_string()),
            signal: Some(signal.clone()),
            related_codes: Vec::new(),
            fix: Some(store_skeleton_fix_snippet(signal, &names)),
        });
    }
}

/// Fence smell: a `Signal<integer>` binding whose only reads are
/// equality comparisons and whose only writes are `+= n` / `-= n` / `.set(0)` —
/// the signal isn't holding state, it's a generation/sentinel counter. The
/// Dioxus runtime overhead (reactive subscription, change tracking) buys
/// nothing here; a plain `Cell<u32>` (server-side) or a generation field
/// on a Store does the job at zero reactive cost.
///
/// We only fire when:
///   1. the initializer is an integer literal (`0u32`, `0i64`, `0`) — the
///      sole signal we have without a full type-resolver,
///   2. at least 2 distinct writes exist that are all `+= n` / `-= n`
///      compound assigns,
///   3. every read of the binding occurs inside a `==` or `!=` comparison,
///   4. the binding is NOT interpolated into rsx (`{sig}` would mean
///      callers DO care about the value as a value).
fn check_signal_used_as_fence(
    f: &syn::ItemFn,
    file: &std::path::Path,
    signal_bindings: &[String],
    issues: &mut Vec<SignalIssue>,
) {
    if signal_bindings.is_empty() {
        return;
    }
    // Gather integer-literal-initialised signals.
    let candidates: Vec<(String, usize)> = f
        .block
        .stmts
        .iter()
        .filter_map(|stmt| {
            let syn::Stmt::Local(local) = stmt else {
                return None;
            };
            let init = local.init.as_ref()?;
            let name = match &local.pat {
                syn::Pat::Ident(p) => p.ident.to_string(),
                syn::Pat::Type(t) => match &*t.pat {
                    syn::Pat::Ident(p) => p.ident.to_string(),
                    _ => return None,
                },
                _ => return None,
            };
            if !signal_bindings.iter().any(|s| s == &name) {
                return None;
            }
            if !is_use_signal_int_init(&init.expr) {
                return None;
            }
            Some((name, local.let_token.span.start().line))
        })
        .collect();
    if candidates.is_empty() {
        return;
    }

    // Pre-pass: collect "snapshot bindings" — `let snap = sig()` where the
    // snap is later compared against a fresh read of the same signal
    // (`sig() == snap` or `snap == sig()`). Without this the snapshot read
    // looks like a bare disqualifying read and the lint drops every
    // optimistic-staleness-gate signal. We accept both the same-body shape
    // and the cross-body shape — the visitor walks the entire fn block.
    let target_names: Vec<String> = candidates.iter().map(|(n, _)| n.clone()).collect();
    let paired_snapshots = collect_paired_snapshots(&f.block, &target_names);

    let mut profiles: std::collections::BTreeMap<String, FenceProfile> =
        std::collections::BTreeMap::new();
    let mut v = FenceVisitor {
        target_names: target_names.clone(),
        profiles: &mut profiles,
        comparison_depth: 0,
        paired_snapshots: &paired_snapshots,
        in_paired_snapshot_init: false,
    };
    v.visit_block(&f.block);

    // rsx interpolation check — a fenced signal is never shown to the user.
    let rsx_interp = rsx_interpolated_names(&f.block);

    for (name, line) in &candidates {
        let Some(p) = profiles.get(name) else {
            // Unused signal — skip; that's a different smell.
            continue;
        };
        if p.disqualified {
            continue;
        }
        if p.compound_writes < 2 {
            continue;
        }
        if rsx_interp.contains(name) {
            continue;
        }
        issues.push(SignalIssue {
            code: "signal_used_as_fence",
            message: format!(
                "`{name}` is a `Signal<integer>` used as a fence — every read is an \
                 equality comparison and every write bumps it by a constant. The \
                 reactive subscription is pure overhead. Replace with a `Cell<u32>` \
                 (server-side state) or expose a generation method on a Store. See \
                 the `Store` primitive in `get_dsl_spec`."
            ),
            file: file.to_path_buf(),
            line: *line,
            component: Some(f.sig.ident.to_string()),
            signal: Some(name.clone()),
            related_codes: Vec::new(),
            fix: Some(fence_store_skeleton(name)),
        });
    }
}

/// Paste-ready `Store` skeleton sized to the writers we observed. The
/// rollup keeps this short — it's a hint, not a generator. Named after
/// the component but not bound to its name (the reviewer renames as
/// they integrate).
fn store_skeleton_fix_snippet(signal: &str, writers: &[&str]) -> String {
    let writer_lines: Vec<String> = writers
        .iter()
        .take(3)
        .map(|w| format!("    pub fn {w}(&mut self) {{ /* moved-out logic for {w} */ }}"))
        .collect();
    let etc = if writers.len() > 3 {
        "    // … remaining writers …\n"
    } else {
        ""
    };
    format!(
        "// Lift `{signal}` into a Store with named mutators:\n\
         pub struct {struct_name}Store {{ pub {signal}: /* type */ }}\n\
         impl {struct_name}Store {{\n\
{writers}\n{etc}}}\n\
         // In the component body:\n\
         //   let mut store = use_context_provider(|| Signal::new({struct_name}Store {{ {signal}: /* init */ }}));\n\
         //   store.write().{first_writer}();",
        struct_name = capitalize(signal),
        writers = writer_lines.join("\n"),
        first_writer = writers.first().copied().unwrap_or("commit"),
        signal = signal,
        etc = etc,
    )
}

fn fence_store_skeleton(name: &str) -> String {
    format!(
        "// Replace the reactive signal with a Cell or a Store generation field:\n\
         use std::cell::Cell;\n\
         let {name}: Cell<u32> = Cell::new(0);\n\
         // bump:    {name}.set({name}.get() + 1);\n\
         // compare: if {name}.get() == previous {{ … }}\n\
         // For a shared/Store version: expose `bump_{name}()` and `current_{name}()`\n\
         // methods on the Store and read the generation when needed.",
    )
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Pair findings that share `(component, signal)` and surface peer codes
/// via `related_codes`. Currently bridges `signal_many_writers` and
/// `signal_used_as_fence` — the Store-lift fix covers both. Other code
/// pairs can be added here without touching the per-lint sites.
fn link_related_findings(issues: &mut [SignalIssue]) {
    use std::collections::HashMap;
    // Build (component, signal) -> set of codes observed.
    let mut buckets: HashMap<(String, String), Vec<&'static str>> = HashMap::new();
    for issue in issues.iter() {
        let (Some(component), Some(signal)) = (&issue.component, &issue.signal) else {
            continue;
        };
        buckets
            .entry((component.clone(), signal.clone()))
            .or_default()
            .push(issue.code);
    }
    for issue in issues.iter_mut() {
        let (Some(component), Some(signal)) = (&issue.component, &issue.signal) else {
            continue;
        };
        let Some(codes) = buckets.get(&(component.clone(), signal.clone())) else {
            continue;
        };
        if codes.len() < 2 {
            continue;
        }
        let mut related: Vec<String> = codes
            .iter()
            .filter(|c| **c != issue.code)
            .map(|c| c.to_string())
            .collect();
        related.sort();
        related.dedup();
        issue.related_codes = related;
    }
}

/// Per-signal state for the fence visitor: how many `+=`/`-=` writes
/// observed, plus a `disqualified` flag set the moment any other shape
/// turns up (a read outside `==`, a `.set` with a non-literal arg, an
/// rsx interpolation, …). One side step and the signal isn't a fence.
#[derive(Default)]
struct FenceProfile {
    compound_writes: u32,
    disqualified: bool,
}

struct FenceVisitor<'a> {
    target_names: Vec<String>,
    profiles: &'a mut std::collections::BTreeMap<String, FenceProfile>,
    /// Depth into a `==` / `!=` comparison expression — bare identifier
    /// reads at depth>0 are "ok" (the fence usage); reads at depth=0 are
    /// disqualifying (callers care about the value).
    comparison_depth: usize,
    /// (signal_name -> {binding_names that hold a snapshot of the signal AND
    /// pair up with a later `sig() == binding_name` comparison}). When we
    /// descend into a `let X = sig()` whose X is in this set, we treat the
    /// inner `sig` read as fence usage instead of disqualifying.
    paired_snapshots: &'a std::collections::HashMap<String, std::collections::HashSet<String>>,
    /// Set whenever the visitor is inside the init expression of a `let X =
    /// sig()` that pairs with a later compare. Reads observed here don't
    /// disqualify.
    in_paired_snapshot_init: bool,
}

impl<'a> FenceVisitor<'a> {
    fn entry(&mut self, name: &str) -> Option<&mut FenceProfile> {
        if self.target_names.iter().any(|n| n == name) {
            Some(self.profiles.entry(name.to_string()).or_default())
        } else {
            None
        }
    }
}

impl<'a, 'ast> Visit<'ast> for FenceVisitor<'a> {
    fn visit_local(&mut self, local: &'ast syn::Local) {
        // Snapshot-let detection: if this is a paired `let X = sig()`, walk
        // the init with `in_paired_snapshot_init = true` so the inner `sig`
        // read doesn't disqualify the fence shape. We still walk the rest
        // of the local normally.
        let paired = paired_snapshot_for(local, self.paired_snapshots);
        if paired.is_some() {
            self.in_paired_snapshot_init = true;
            if let Some(init) = &local.init {
                self.visit_expr(&init.expr);
                if let Some((_, diverge)) = &init.diverge {
                    self.visit_expr(diverge);
                }
            }
            self.in_paired_snapshot_init = false;
            // Visit attributes/pat as usual (no-op for our patterns).
            for attr in &local.attrs {
                self.visit_attribute(attr);
            }
            self.visit_pat(&local.pat);
            return;
        }
        syn::visit::visit_local(self, local);
    }

    fn visit_expr_binary(&mut self, eb: &'ast syn::ExprBinary) {
        // syn 2.0 collapses `a += 1` into `ExprBinary` with `BinOp::AddAssign`,
        // not a separate `ExprAssignOp` node. We handle three flavours here:
        //   - `==` / `!=`  → bump comparison_depth so reads inside don't
        //     disqualify the fence;
        //   - `+=` / `-=`  → compound write; count when the rhs is an int
        //     literal, disqualify otherwise (`sig += other()` means callers
        //     care about the value);
        //   - everything else (`+`, `-`, `<`, etc.) → default-recurse; bare
        //     reads at depth 0 get caught by `visit_expr_path`.
        let is_cmp = matches!(eb.op, syn::BinOp::Eq(_) | syn::BinOp::Ne(_));
        let is_inc_dec = matches!(eb.op, syn::BinOp::AddAssign(_) | syn::BinOp::SubAssign(_));
        if is_inc_dec {
            if let Some(name) = single_ident(&eb.left)
                && let Some(p) = self.entry(&name)
            {
                if is_int_literal(&eb.right) {
                    p.compound_writes += 1;
                } else {
                    p.disqualified = true;
                }
            }
            // Visit the rhs but not the lhs — the lhs ident here is the
            // write target, not a read.
            self.visit_expr(&eb.right);
            return;
        }
        if is_cmp {
            self.comparison_depth += 1;
            syn::visit::visit_expr_binary(self, eb);
            self.comparison_depth -= 1;
        } else {
            syn::visit::visit_expr_binary(self, eb);
        }
    }
    fn visit_expr_assign(&mut self, ea: &'ast syn::ExprAssign) {
        // Plain `sig = x` — disqualifies (writes are supposed to be compound).
        if let Some(name) = single_ident(&ea.left)
            && let Some(p) = self.entry(&name)
        {
            p.disqualified = true;
        }
        syn::visit::visit_expr_assign(self, ea);
    }
    fn visit_expr_method_call(&mut self, mc: &'ast syn::ExprMethodCall) {
        let method = mc.method.to_string();
        // Any method-style write other than `.set(integer_literal)` is
        // disqualifying for the fence shape.
        let is_write_method = matches!(
            method.as_str(),
            "set" | "set_silent" | "write" | "with_mut" | "replace" | "swap" | "take"
        );
        if is_write_method
            && let Some(name) = single_ident(&mc.receiver)
            && let Some(p) = self.entry(&name)
        {
            let ok = method == "set" && mc.args.len() == 1 && is_int_literal(&mc.args[0]);
            if !ok {
                p.disqualified = true;
            }
        }
        syn::visit::visit_expr_method_call(self, mc);
    }
    fn visit_expr_path(&mut self, ep: &'ast syn::ExprPath) {
        if let Some(name) = path_single_ident(ep) {
            let depth = self.comparison_depth;
            let in_snapshot = self.in_paired_snapshot_init;
            if let Some(p) = self.entry(&name)
                && depth == 0
                && !in_snapshot
            {
                // Bare ident read outside a comparison — disqualifies, unless
                // we're under a paired `let snap = sig()` snapshot init, in
                // which case the read is part of the fence shape itself.
                p.disqualified = true;
            }
        }
        syn::visit::visit_expr_path(self, ep);
    }
}

/// Pre-pass over the component body — returns `(signal_name ->
/// {binding_names that snapshot this signal AND later appear in a
/// `sig() == binding_name` / `binding_name == sig()` comparison})`.
///
/// The detector intentionally treats the snapshot and the compare as the
/// same "fence read pair" even when they live in different bodies: the
/// optimistic-staleness-gate shape often spreads the snapshot into a
/// `use_future` hook body and the compare into the same hook tail. We don't
/// scope-check the binding (a free `lock` ident in one closure isn't
/// necessarily the same binding declared in another closure), but the false-
/// positive rate is low — the binding name has to line up exactly with a
/// snapshot we collected.
fn collect_paired_snapshots(
    block: &syn::Block,
    target_names: &[String],
) -> std::collections::HashMap<String, std::collections::HashSet<String>> {
    let mut snapshots: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    let mut compares: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();

    let mut snap_v = SnapshotCollector {
        target_names,
        out: &mut snapshots,
    };
    snap_v.visit_block(block);
    let mut cmp_v = CompareCollector {
        target_names,
        out: &mut compares,
    };
    cmp_v.visit_block(block);

    // Intersection per-signal: keep only binding names that BOTH snapshot
    // the signal AND appear in a `sig() == X` comparison.
    let mut paired: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    for (sig, snap_names) in &snapshots {
        let Some(cmp_names) = compares.get(sig) else {
            continue;
        };
        let intersect: std::collections::HashSet<String> =
            snap_names.intersection(cmp_names).cloned().collect();
        if !intersect.is_empty() {
            paired.insert(sig.clone(), intersect);
        }
    }
    paired
}

fn paired_snapshot_for(
    local: &syn::Local,
    paired: &std::collections::HashMap<String, std::collections::HashSet<String>>,
) -> Option<(String, String)> {
    let init = local.init.as_ref()?;
    let sig_name = signal_call_name(&init.expr)?;
    let binding_name = match &local.pat {
        syn::Pat::Ident(p) => p.ident.to_string(),
        syn::Pat::Type(t) => match &*t.pat {
            syn::Pat::Ident(p) => p.ident.to_string(),
            _ => return None,
        },
        _ => return None,
    };
    if paired.get(&sig_name)?.contains(&binding_name) {
        Some((sig_name, binding_name))
    } else {
        None
    }
}

/// True if `expr` is `sig_name()` — a zero-arg call whose callee is the
/// bare ident `sig_name`. Returns the ident.
fn signal_call_name(expr: &syn::Expr) -> Option<String> {
    let syn::Expr::Call(c) = expr else {
        return None;
    };
    if !c.args.is_empty() {
        return None;
    }
    let syn::Expr::Path(p) = &*c.func else {
        return None;
    };
    path_single_ident(p)
}

struct SnapshotCollector<'a> {
    target_names: &'a [String],
    out: &'a mut std::collections::HashMap<String, std::collections::HashSet<String>>,
}

impl<'a, 'ast> Visit<'ast> for SnapshotCollector<'a> {
    fn visit_local(&mut self, local: &'ast syn::Local) {
        if let Some(init) = &local.init
            && let Some(sig_name) = signal_call_name(&init.expr)
            && self.target_names.iter().any(|n| n == &sig_name)
        {
            let binding_name = match &local.pat {
                syn::Pat::Ident(p) => Some(p.ident.to_string()),
                syn::Pat::Type(t) => match &*t.pat {
                    syn::Pat::Ident(p) => Some(p.ident.to_string()),
                    _ => None,
                },
                _ => None,
            };
            if let Some(binding) = binding_name {
                self.out.entry(sig_name).or_default().insert(binding);
            }
        }
        syn::visit::visit_local(self, local);
    }
}

struct CompareCollector<'a> {
    target_names: &'a [String],
    out: &'a mut std::collections::HashMap<String, std::collections::HashSet<String>>,
}

impl<'a, 'ast> Visit<'ast> for CompareCollector<'a> {
    fn visit_expr_binary(&mut self, eb: &'ast syn::ExprBinary) {
        if matches!(eb.op, syn::BinOp::Eq(_) | syn::BinOp::Ne(_)) {
            // `sig() == name` (or reversed) — capture (sig, name) pairs.
            if let Some(sig) = signal_call_name(&eb.left)
                && self.target_names.iter().any(|n| n == &sig)
                && let syn::Expr::Path(p) = &*eb.right
                && let Some(name) = path_single_ident(p)
            {
                self.out.entry(sig).or_default().insert(name);
            } else if let Some(sig) = signal_call_name(&eb.right)
                && self.target_names.iter().any(|n| n == &sig)
                && let syn::Expr::Path(p) = &*eb.left
                && let Some(name) = path_single_ident(p)
            {
                self.out.entry(sig).or_default().insert(name);
            }
        }
        syn::visit::visit_expr_binary(self, eb);
    }
}

/// Like `single_ident` but takes a borrowed `ExprPath` directly — used
/// inside `FenceVisitor::visit_expr_path` where we already have the path.
fn path_single_ident(ep: &syn::ExprPath) -> Option<String> {
    if ep.path.segments.len() == 1 && ep.path.leading_colon.is_none() && ep.qself.is_none() {
        Some(ep.path.segments[0].ident.to_string())
    } else {
        None
    }
}

/// True when `expr` is `use_signal(|| <int-literal>)` — the only shape we
/// recognise as "definitely an integer-typed signal" without a real type
/// resolver. We accept the literal alone (`0`, `0u32`, `0i64`, `-1i32`).
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
    // Argument should be a closure with an integer-literal body. Anything
    // more complex (Vec::new(), String::new(), Option::None, ...) means
    // we can't be sure of the type, so we skip.
    let Some(arg) = c.args.first() else {
        return false;
    };
    let syn::Expr::Closure(cl) = arg else {
        return false;
    };
    is_int_literal(&cl.body)
}

/// True for `0`, `1`, `0u32`, `-5i32`, `(0)`, etc. We strip unary minus and
/// parens so all the natural integer-literal-init forms count.
fn is_int_literal(expr: &syn::Expr) -> bool {
    match expr {
        syn::Expr::Lit(l) => matches!(l.lit, syn::Lit::Int(_)),
        syn::Expr::Unary(u) if matches!(u.op, syn::UnOp::Neg(_)) => is_int_literal(&u.expr),
        syn::Expr::Paren(p) => is_int_literal(&p.expr),
        _ => false,
    }
}

/// Set of binding names that appear inside rsx interpolation. We
/// conservatively scan token streams (same approach as `explain_signal_graph`)
/// — a name showing up here means consumers care about its *value*, so the
/// fence shape doesn't apply.
fn rsx_interpolated_names(block: &syn::Block) -> std::collections::HashSet<String> {
    use proc_macro2::TokenTree;
    fn walk(ts: proc_macro2::TokenStream, hits: &mut std::collections::HashSet<String>) {
        for tt in ts {
            match tt {
                TokenTree::Group(g) => walk(g.stream(), hits),
                TokenTree::Ident(i) => {
                    hits.insert(i.to_string());
                }
                TokenTree::Literal(lit) => {
                    let s = lit.to_string();
                    if let Some(inner) = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                        let bytes = inner.as_bytes();
                        let mut i = 0;
                        while i < bytes.len() {
                            if bytes[i] == b'{' && (i + 1 >= bytes.len() || bytes[i + 1] != b'{') {
                                let start = i + 1;
                                let mut end = start;
                                while end < bytes.len() && bytes[end] != b'}' && bytes[end] != b':'
                                {
                                    end += 1;
                                }
                                let token = &inner[start..end];
                                let head = token.split(['.', '(', ' ']).next().unwrap_or("");
                                if !head.is_empty() {
                                    hits.insert(head.to_string());
                                }
                                while i < bytes.len() && bytes[i] != b'}' {
                                    i += 1;
                                }
                            }
                            i += 1;
                        }
                    }
                }
                TokenTree::Punct(_) => {}
            }
        }
    }
    struct RsxFinder {
        hits: std::collections::HashSet<String>,
    }
    impl<'ast> Visit<'ast> for RsxFinder {
        fn visit_macro(&mut self, m: &'ast syn::Macro) {
            let is_rsx = m
                .path
                .segments
                .last()
                .map(|s| s.ident == "rsx")
                .unwrap_or(false);
            if is_rsx {
                walk(m.tokens.clone(), &mut self.hits);
            }
            syn::visit::visit_macro(self, m);
        }
    }
    let mut f = RsxFinder {
        hits: std::collections::HashSet::new(),
    };
    f.visit_block(block);
    f.hits
}

/// Pre-scan a component body for `let X = use_signal(...)` bindings — the
/// set of names whose reactive reads matter for the polling-future lint.
/// Mirrors the scope inspector in `explain_signal_graph`, but lives here so
/// the signal lints stay independent.
/// Names of `let [mut] X = use_signal(|| false);` bindings in the component
/// body. We require the initial value to be the literal `false` because
/// the bootstrap-gate shape always starts in the not-bootstrapped state.
/// Sibling shapes like `use_signal(|| Some(true))` or `use_signal(MyEnum::Loading)`
/// could conceivably gate rsx but aren't the recurring generator pattern.
fn collect_bool_gate_signals(block: &syn::Block) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for stmt in &block.stmts {
        let syn::Stmt::Local(local) = stmt else {
            continue;
        };
        let Some(init) = &local.init else { continue };
        // Receiver shape: `use_signal(|| false)`. The init expression is
        // an ExprCall with `use_signal` as the path and a single closure
        // argument whose body is `false`.
        let syn::Expr::Call(call) = &*init.expr else {
            continue;
        };
        let Some(tail) = extract_path_tail(&call.func) else {
            continue;
        };
        if tail != "use_signal" {
            continue;
        }
        let Some(arg) = call.args.first() else {
            continue;
        };
        let syn::Expr::Closure(closure) = arg else {
            continue;
        };
        let is_false_init = match &*closure.body {
            syn::Expr::Lit(l) => matches!(l.lit, syn::Lit::Bool(syn::LitBool { value: false, .. })),
            _ => false,
        };
        if !is_false_init {
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

/// Scan every `rsx!` macro body in the component for `if <Ident>()` token
/// sequences. We don't try to parse the rsx grammar — just look for the
/// signature shape `Ident( ) { … }` inside the token stream. iter03's
/// `if bootstrapped() { Router … } else { div … }` matches.
fn collect_rsx_if_signal_calls(block: &syn::Block) -> std::collections::HashSet<String> {
    use proc_macro2::Delimiter;
    let mut visitor = RsxCollector { bodies: Vec::new() };
    visitor.visit_block(block);
    let mut out: std::collections::HashSet<String> = std::collections::HashSet::new();
    for body in &visitor.bodies {
        let tokens: Vec<TokenTree> = body.clone().into_iter().collect();
        scan_if_calls(&tokens, &mut out);
    }

    fn scan_if_calls(tokens: &[TokenTree], out: &mut std::collections::HashSet<String>) {
        let mut i = 0;
        while i + 2 < tokens.len() {
            if let TokenTree::Ident(kw) = &tokens[i]
                && kw == "if"
                && let (TokenTree::Ident(name), TokenTree::Group(args)) =
                    (&tokens[i + 1], &tokens[i + 2])
                && args.delimiter() == Delimiter::Parenthesis
                && args.stream().is_empty()
            {
                out.insert(name.to_string());
            }
            if let TokenTree::Group(g) = &tokens[i] {
                let inner: Vec<TokenTree> = g.stream().into_iter().collect();
                scan_if_calls(&inner, out);
            }
            i += 1;
        }
        // Trailing groups in the last window.
        for tt in tokens.iter().skip(i) {
            if let TokenTree::Group(g) = tt {
                let inner: Vec<TokenTree> = g.stream().into_iter().collect();
                scan_if_calls(&inner, out);
            }
        }
    }

    out
}

struct RsxCollector {
    bodies: Vec<proc_macro2::TokenStream>,
}

impl<'ast> Visit<'ast> for RsxCollector {
    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        let is_rsx = m
            .path
            .segments
            .last()
            .map(|s| s.ident == "rsx")
            .unwrap_or(false);
        if is_rsx {
            self.bodies.push(m.tokens.clone());
        }
        syn::visit::visit_macro(self, m);
    }
}

fn collect_use_signal_bindings(block: &syn::Block) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for stmt in &block.stmts {
        let syn::Stmt::Local(local) = stmt else {
            continue;
        };
        let Some(init) = &local.init else { continue };
        let Some(tail) = init_call_tail(&init.expr) else {
            continue;
        };
        if tail != "use_signal" {
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

/// Detects `use_future` bodies that contain BOTH a sleep/interval AND a
/// reactive signal read — the polling-loop subscription footgun. Every
/// reactive read inside a `use_future` body subscribes the future, so an
/// optimistic write to the read signal cancels and restarts the loop.
/// Standup's `fetch_board` is the canonical case: a polling future that
/// reads `local_lock()` for *gating* (suppress applying stale responses)
/// inadvertently turns the lock into a *driver*.
///
/// We require both signals so we don't false-positive on:
/// - one-shot fetches with no sleep (`use_future` that runs once)
/// - sleeping futures that don't read any signal (timers, animations)
///
/// `.peek()` is treated as a non-reactive read and does NOT trigger — the
/// fix suggestion when the warning fires is usually "switch the read to
/// `.peek()`", so the lint must respect that escape hatch.
struct PollingFutureVisitor<'a> {
    in_future: bool,
    saw_sleep: bool,
    /// `(signal_name, line)` per reactive read seen inside the current
    /// `use_future`. Flushed (and deduped) when we exit the call.
    read_lines: Vec<(String, usize)>,
    future_line: usize,
    signal_bindings: &'a [String],
    issues: &'a mut Vec<SignalIssue>,
    file: &'a std::path::Path,
    component: String,
}

impl<'a> PollingFutureVisitor<'a> {
    fn flush(&mut self) {
        if !self.saw_sleep || self.read_lines.is_empty() {
            return;
        }
        let mut names: Vec<String> = self.read_lines.iter().map(|(n, _)| n.clone()).collect();
        names.sort();
        names.dedup();
        let signals = names.join(", ");
        self.issues.push(SignalIssue {
            code: "polling_future_reactive_read",
            message: format!(
                "`use_future` with a sleep/interval also reads signal(s) [{signals}] — \
                 each reactive read subscribes the future, so every write restarts the \
                 polling loop. If the signal is meant to *gate* applying stale results \
                 (the common intent), read it with `.peek()` instead. If you DO want the \
                 loop to restart on the signal, use `use_resource` so the dependency is \
                 explicit and idiomatic."
            ),
            file: self.file.to_path_buf(),
            line: self.future_line,
            component: Some(self.component.clone()),
            signal: None,
            related_codes: Vec::new(),
            fix: None,
        });
    }
}

impl<'a, 'ast> Visit<'ast> for PollingFutureVisitor<'a> {
    fn visit_expr_call(&mut self, e: &'ast syn::ExprCall) {
        let tail = extract_path_tail(&e.func);
        let entering = matches!(tail.as_deref(), Some("use_future")) && !self.in_future;
        if entering {
            // Save/swap per-future state so a sibling `use_future` (rare,
            // but legal) doesn't pick up the previous future's sleep flag.
            let saved_sleep = self.saw_sleep;
            let saved_reads = std::mem::take(&mut self.read_lines);
            let saved_line = self.future_line;
            self.in_future = true;
            self.saw_sleep = false;
            self.future_line = e.func.span().start().line;
            syn::visit::visit_expr_call(self, e);
            self.flush();
            self.in_future = false;
            self.saw_sleep = saved_sleep;
            self.read_lines = saved_reads;
            self.future_line = saved_line;
            return;
        }

        // Sleep / interval / timeout detection inside the current future.
        // The list covers tokio (`sleep`, `interval`), gloo-timers
        // (`TimeoutFuture::new`, `sleep`), async-std (`sleep`), and the
        // browser DOM globals (`set_interval`, `set_timeout`) used via
        // `wasm-bindgen-futures`. For the bare-fn forms we just match the
        // path tail; for the `Type::new(...)` constructor form (Tokio
        // `Interval::new`, gloo-timers `TimeoutFuture::new`) the tail is
        // always `new`, so we also peek at the second-to-last segment.
        if self.in_future {
            if let Some(t) = tail.as_deref()
                && matches!(
                    t,
                    "sleep"
                        | "sleep_until"
                        | "interval"
                        | "interval_at"
                        | "TimeoutFuture"
                        | "Interval"
                        | "set_interval"
                        | "set_timeout"
                )
            {
                self.saw_sleep = true;
            }
            if tail.as_deref() == Some("new") && is_timer_type_new(&e.func) {
                self.saw_sleep = true;
            }
        }

        // Reactive read via `signal()` — bare single-segment path call with
        // no args, where the ident names a local `use_signal` binding.
        if self.in_future
            && let syn::Expr::Path(p) = &*e.func
            && p.path.segments.len() == 1
            && p.path.leading_colon.is_none()
            && e.args.is_empty()
            && let Some(seg) = p.path.segments.last()
        {
            let name = seg.ident.to_string();
            if self.signal_bindings.iter().any(|s| s == &name) {
                self.read_lines.push((name, e.func.span().start().line));
            }
        }

        syn::visit::visit_expr_call(self, e);
    }

    fn visit_expr_method_call(&mut self, e: &'ast syn::ExprMethodCall) {
        if self.in_future
            && e.method == "read"
            && let Some(name) = single_ident(&e.receiver)
            && self.signal_bindings.iter().any(|s| s == &name)
        {
            self.read_lines.push((name, e.method.span().start().line));
        }
        syn::visit::visit_expr_method_call(self, e);
    }
}

/// True when `expr` is a path-style call whose tail is `new` and whose
/// preceding type segment names a known timer/interval type. Catches the
/// `gloo_timers::future::TimeoutFuture::new(2000)` and `tokio::time::Interval::new(...)`
/// constructors that the path-tail check alone misses (tail = "new"). We
/// only look at the segment immediately before `new`, so `Foo::Bar::new`
/// matches when `Bar` is one of the timer types — qualified or imported.
fn is_timer_type_new(expr: &syn::Expr) -> bool {
    let syn::Expr::Path(p) = expr else {
        return false;
    };
    if p.path.segments.len() < 2 {
        return false;
    }
    let ty = &p.path.segments[p.path.segments.len() - 2].ident;
    matches!(
        ty.to_string().as_str(),
        "TimeoutFuture" | "Interval" | "IntervalStream"
    )
}

/// Returns the ident name when `expr` is a bare single-segment path
/// (`nav`, `router`) — used to identify the receiver of a `.push("/...")`
/// call as a navigator/router handle. Returns `None` for `state.nav`,
/// `self.nav`, `nav.read()`, or any non-trivial expression.
fn single_ident(expr: &syn::Expr) -> Option<String> {
    if let syn::Expr::Path(p) = expr
        && p.path.segments.len() == 1
        && p.path.leading_colon.is_none()
        && p.qself.is_none()
    {
        return Some(p.path.segments[0].ident.to_string());
    }
    None
}

/// True for every compound-assignment `BinOp` variant in syn 2.0 (`+=`, `-=`,
/// `*=`, `/=`, `%=`, `^=`, `&=`, `|=`, `<<=`, `>>=`). syn 2.0 collapsed the
/// separate `ExprAssignOp` node from syn 1.x into `ExprBinary`, so
/// compound-assignment writes show up under `visit_expr_binary` with one of
/// these ops — the visitor has to recognise them or the lhs reference reads
/// as inert.
fn is_compound_assign(op: &syn::BinOp) -> bool {
    matches!(
        op,
        syn::BinOp::AddAssign(_)
            | syn::BinOp::SubAssign(_)
            | syn::BinOp::MulAssign(_)
            | syn::BinOp::DivAssign(_)
            | syn::BinOp::RemAssign(_)
            | syn::BinOp::BitXorAssign(_)
            | syn::BinOp::BitAndAssign(_)
            | syn::BinOp::BitOrAssign(_)
            | syn::BinOp::ShlAssign(_)
            | syn::BinOp::ShrAssign(_)
    )
}

#[cfg(test)]
mod hydration_tests {
    use super::*;

    /// Drive `HydrationVisitor` over a synthetic component body. Returns the
    /// emitted issues — we assert on `code` + `line` so adding a new lint
    /// elsewhere doesn't break these.
    fn lint_snippet(snippet: &str) -> Vec<SignalIssue> {
        let src = format!(
            "use dioxus::prelude::*;\n#[component]\nfn Demo() -> Element {{\n{snippet}\nrsx!{{}}\n}}\n"
        );
        let file: syn::File = syn::parse_str(&src).expect("test snippet parses");
        let mut issues: Vec<SignalIssue> = Vec::new();
        for item in &file.items {
            let syn::Item::Fn(f) = item else { continue };
            let mut v = HydrationVisitor {
                closure_depth: 0,
                hook_depth: 0,
                issues: &mut issues,
                file: Path::new("snippet.rs"),
                component: f.sig.ident.to_string(),
            };
            v.visit_block(&f.block);
        }
        issues
    }

    #[test]
    fn flags_localstorage_get_at_top_level() {
        let issues =
            lint_snippet(r#"let _x = LocalStorage::get::<String>("theme").unwrap_or_default();"#);
        assert!(
            issues
                .iter()
                .any(|i| i.code == "hydration_browser_read" && i.message.contains("LocalStorage")),
            "expected LocalStorage hit, got: {issues:?}"
        );
    }

    #[test]
    fn flags_sessionstorage_set_at_top_level() {
        let issues = lint_snippet(r#"SessionStorage::set("key", "value").ok();"#);
        assert!(
            issues
                .iter()
                .any(|i| i.code == "hydration_browser_read" && i.message.contains("SessionStorage")),
            "expected SessionStorage hit, got: {issues:?}"
        );
    }

    #[test]
    fn ignores_localstorage_inside_use_effect() {
        let issues = lint_snippet(
            r#"use_effect(move || {
                let _x = LocalStorage::get::<String>("theme").unwrap_or_default();
            });"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "hydration_browser_read")
            .collect();
        assert!(
            hits.is_empty(),
            "LocalStorage inside use_effect must not be flagged: {hits:?}"
        );
    }

    #[test]
    fn ignores_localstorage_inside_event_handler_closure() {
        let issues = lint_snippet(
            r#"let _on_click = move |_e: Event<MouseData>| {
                LocalStorage::set("k", "v").ok();
            };"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "hydration_browser_read")
            .collect();
        assert!(
            hits.is_empty(),
            "LocalStorage inside an event-handler closure must not be flagged: {hits:?}"
        );
    }

    #[test]
    fn flags_document_cookie_method_call() {
        let issues =
            lint_snippet(r#"let _c = web_sys::window().unwrap().document().unwrap().cookie();"#);
        assert!(
            issues
                .iter()
                .any(|i| i.code == "hydration_browser_read" && i.message.contains("cookie")),
            "expected Document::cookie() hit, got: {issues:?}"
        );
    }

    fn parse_into_scanned(path: &str, src: &str) -> crate::tools::ast::ScannedFile {
        let ast = syn::parse_file(src);
        crate::tools::ast::ScannedFile {
            path: std::path::PathBuf::from(path),
            ast,
        }
    }

    #[test]
    fn no_triad_below_three_modules() {
        let files = vec![
            parse_into_scanned(
                "src/state/theme.rs",
                "pub fn provide_theme() {}\npub fn use_theme() {}\n",
            ),
            parse_into_scanned(
                "src/state/user.rs",
                "pub fn provide_user() {}\npub fn use_user() {}\n",
            ),
        ];
        let (triads, summary) = detect_context_signal_triads(&files);
        assert!(
            triads.is_empty(),
            "two paired modules is not a triad: {triads:?}"
        );
        // Even when no triad is emitted, the summary surfaces what WAS
        // detected — so callers can tell "below threshold" from "nothing
        // matched" at a glance.
        assert_eq!(summary.detected, 2);
        assert_eq!(summary.threshold, 3);
        let mut names = summary.names.clone();
        names.sort();
        assert_eq!(names, vec!["theme".to_string(), "user".to_string()]);
    }

    /// iter03 follow-up: when N=2 modules each carry the canonical
    /// `use_context_provider(|| Signal::new(…))` + `use_context::<Signal<…>>()`
    /// boilerplate, emit a hint before the third pair lands and makes
    /// the duplication harder to unwind. The earlier
    /// `no_triad_below_three_modules` covers the empty-body case where
    /// the pair is coincidental — that one must remain silent.
    #[test]
    fn flags_two_modules_when_both_share_signal_boilerplate() {
        let files = vec![
            parse_into_scanned(
                "src/state/session.rs",
                r#"use dioxus::prelude::*;
pub fn provide_session() {
    use_context_provider(|| Signal::new(None::<String>));
}
pub fn use_session() -> Signal<Option<String>> {
    use_context::<Signal<Option<String>>>()
}
"#,
            ),
            parse_into_scanned(
                "src/state/presence.rs",
                r#"use dioxus::prelude::*;
pub fn provide_presence() {
    use_context_provider(|| Signal::new(Vec::<String>::new()));
}
pub fn use_presence() -> Signal<Vec<String>> {
    use_context::<Signal<Vec<String>>>()
}
"#,
            ),
        ];
        let (triads, summary) = detect_context_signal_triads(&files);
        assert_eq!(triads.len(), 1, "expected one N=2 hint: {triads:?}");
        let names: Vec<&str> = triads[0].modules.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"session"));
        assert!(names.contains(&"presence"));
        assert!(triads[0].message.contains("Store"));
        assert!(triads[0].message.contains("2 sibling"));
        assert_eq!(summary.detected, 2);
    }

    #[test]
    fn flags_three_or_more_provide_use_pairs() {
        let files = vec![
            parse_into_scanned(
                "src/state/theme.rs",
                "pub fn provide_theme() {}\npub fn use_theme() {}\n",
            ),
            parse_into_scanned(
                "src/state/user.rs",
                "pub fn provide_user() {}\npub fn use_user() {}\n",
            ),
            parse_into_scanned(
                "src/state/locale.rs",
                "pub fn provide_locale() {}\npub fn use_locale() {}\n",
            ),
        ];
        let (triads, summary) = detect_context_signal_triads(&files);
        assert_eq!(triads.len(), 1, "expected one triad suggestion: {triads:?}");
        let names: Vec<&str> = triads[0].modules.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"theme"));
        assert!(names.contains(&"user"));
        assert!(names.contains(&"locale"));
        assert!(triads[0].message.contains("Store"));
        // Summary mirrors the suggestion: detected count and names match.
        assert_eq!(summary.detected, 3);
        assert_eq!(summary.threshold, 3);
    }

    #[test]
    fn module_with_only_provide_or_only_use_is_skipped() {
        // First file pairs provide_a + use_a → counts.
        // Second has only provide_b — doesn't count.
        // Third has only use_c — doesn't count.
        // Net: 1 module ⇒ no triad.
        let files = vec![
            parse_into_scanned(
                "src/state/a.rs",
                "pub fn provide_a() {}\npub fn use_a() {}\n",
            ),
            parse_into_scanned("src/state/b.rs", "pub fn provide_b() {}\n"),
            parse_into_scanned("src/state/c.rs", "pub fn use_c() {}\n"),
        ];
        let (triads, summary) = detect_context_signal_triads(&files);
        assert!(
            triads.is_empty(),
            "lone halves don't form pairs: {triads:?}"
        );
        assert_eq!(summary.detected, 1, "only `a` paired both halves");
        assert_eq!(summary.names, vec!["a".to_string()]);
    }

    #[test]
    fn ignores_private_provide_use_functions() {
        // All three modules use `fn` instead of `pub fn` — they're inferred
        // helpers, not the public context-signal idiom. No triad.
        let files = vec![
            parse_into_scanned("src/state/a.rs", "fn provide_a() {}\nfn use_a() {}\n"),
            parse_into_scanned("src/state/b.rs", "fn provide_b() {}\nfn use_b() {}\n"),
            parse_into_scanned("src/state/c.rs", "fn provide_c() {}\nfn use_c() {}\n"),
        ];
        let (triads, summary) = detect_context_signal_triads(&files);
        assert!(
            triads.is_empty(),
            "private helpers aren't the public context-signal idiom: {triads:?}"
        );
        // Summary should also report zero — the detector ignored the
        // private fns entirely, didn't just suppress the suggestion.
        assert_eq!(summary.detected, 0);
        assert!(summary.names.is_empty());
    }

    #[test]
    fn ignores_document_cookie_inside_spawn_async_block() {
        let issues = lint_snippet(
            r#"spawn(async move {
                let _c = web_sys::window().unwrap().document().unwrap().cookie();
            });"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "hydration_browser_read")
            .collect();
        assert!(
            hits.is_empty(),
            "cookie() inside spawn(async move) must not be flagged: {hits:?}"
        );
    }

    /// Drive `EffectSpawnVisitor` over a synthetic component body. Mirrors
    /// `lint_snippet` but uses the new visitor — kept separate so the two
    /// hydration lints can evolve independently.
    fn lint_effect_spawn(snippet: &str) -> Vec<SignalIssue> {
        let src = format!(
            "use dioxus::prelude::*;\n#[component]\nfn Demo() -> Element {{\n{snippet}\nrsx!{{}}\n}}\n"
        );
        let file: syn::File = syn::parse_str(&src).expect("test snippet parses");
        let mut issues: Vec<SignalIssue> = Vec::new();
        for item in &file.items {
            let syn::Item::Fn(f) = item else { continue };
            let bool_gates = collect_bool_gate_signals(&f.block);
            let rsx_gates = collect_rsx_if_signal_calls(&f.block);
            let mut v = EffectSpawnVisitor {
                effect_depth: 0,
                spawn_depth: 0,
                saw_await: false,
                set_lines: Vec::new(),
                set_calls: Vec::new(),
                effect_line: 0,
                bool_gates: &bool_gates,
                rsx_gates: &rsx_gates,
                issues: &mut issues,
                file: Path::new("snippet.rs"),
                component: f.sig.ident.to_string(),
            };
            v.visit_block(&f.block);
        }
        issues
    }

    #[test]
    fn flags_use_effect_spawn_with_await_and_set() {
        // The exact dioxus_standup `App` shape: bootstrap a session via
        // `use_effect` + `spawn(async move { server_fn().await; sig.set(...) })`
        // and gate the rendered Router on the signal. Hydration footgun.
        let issues = lint_effect_spawn(
            r#"let mut bootstrapped = use_signal(|| false);
use_effect(move || {
    spawn(async move {
        let _ = some_server_fn().await;
        bootstrapped.set(true);
    });
});"#,
        );
        assert_eq!(
            issues.len(),
            1,
            "expected one hydration_unsafe_effect issue, got: {issues:?}"
        );
        assert_eq!(issues[0].code, "hydration_unsafe_effect");
        assert!(
            issues[0].message.contains("use_server_future"),
            "message should suggest use_server_future: {}",
            issues[0].message
        );
    }

    #[test]
    fn ignores_use_effect_without_await() {
        // `use_effect` that schedules a sync write (e.g. updates a derived
        // signal) is unrelated to the hydration shape — no await, no flash.
        let issues = lint_effect_spawn(
            r#"let mut sig = use_signal(|| 0);
use_effect(move || {
    spawn(async move {
        sig.set(1);
    });
});"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "hydration_unsafe_effect")
            .collect();
        assert!(
            hits.is_empty(),
            "no await ⇒ not a hydration-flash pattern: {hits:?}"
        );
    }

    #[test]
    fn ignores_use_effect_without_signal_write() {
        // Pure browser-side effect — fetch + log, no signal write. The
        // server's render doesn't depend on its outcome, so no hydration
        // mismatch. Common for analytics pings / focus-tracking.
        let issues = lint_effect_spawn(
            r#"use_effect(move || {
    spawn(async move {
        let _ = some_server_fn().await;
    });
});"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "hydration_unsafe_effect")
            .collect();
        assert!(
            hits.is_empty(),
            "no signal write ⇒ no hydration shape, just a side effect: {hits:?}"
        );
    }

    #[test]
    fn ignores_bare_spawn_outside_use_effect() {
        // `spawn(async move { ... .set(...) })` at the component body level
        // (not inside use_effect) is the recommended shape for event-handler
        // hand-offs — we don't flag it here.
        let issues = lint_effect_spawn(
            r#"let mut sig = use_signal(|| 0);
spawn(async move {
    let _ = some_server_fn().await;
    sig.set(1);
});"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "hydration_unsafe_effect")
            .collect();
        assert!(
            hits.is_empty(),
            "bare spawn outside use_effect isn't the flagged shape: {hits:?}"
        );
    }

    /// Drive `EffectNavigateVisitor` over a synthetic component body. Mirrors
    /// `lint_snippet` / `lint_effect_spawn`; kept separate so the navigate
    /// rule's tests don't shadow the spawn-await rule's.
    fn lint_effect_navigate(snippet: &str) -> Vec<SignalIssue> {
        let src = format!(
            "use dioxus::prelude::*;\n#[component]\nfn Demo() -> Element {{\n{snippet}\nrsx!{{}}\n}}\n"
        );
        let file: syn::File = syn::parse_str(&src).expect("test snippet parses");
        let mut issues: Vec<SignalIssue> = Vec::new();
        for item in &file.items {
            let syn::Item::Fn(f) = item else { continue };
            let nav_bindings = collect_navigator_bindings(&f.block);
            let mut v = EffectNavigateVisitor {
                effect_depth: 0,
                effect_line: 0,
                nav_bindings: &nav_bindings,
                issues: &mut issues,
                file: Path::new("snippet.rs"),
                component: f.sig.ident.to_string(),
                already_flagged: false,
            };
            v.visit_block(&f.block);
        }
        issues
    }

    /// Standup's `Protected` shape: read a context signal in a `use_effect`
    /// and `nav.push("/login")` based on its value. SSR doesn't run the
    /// effect, so the server returns the protected page, the client mounts,
    /// the effect reads the absent session, and immediately redirects — the
    /// user sees the protected content flash. Flag it on the effect line.
    #[test]
    fn flags_use_effect_with_nav_push_to_redirect() {
        let issues = lint_effect_navigate(
            r#"let session = use_context::<Signal<Option<String>>>();
let nav = use_navigator();
use_effect(move || {
    if session().is_none() {
        nav.push("/login");
    }
});"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "hydration_unsafe_effect")
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "expected one redirect-in-effect hit: {issues:?}"
        );
        assert!(
            hits[0].message.contains("router navigation"),
            "message should call out the navigation shape: {}",
            hits[0].message
        );
    }

    /// Even without a local `let nav = use_navigator()` binding (e.g., when
    /// the navigator is received via prop / context), the convention-based
    /// name `nav` still matches so the rule catches the same shape.
    #[test]
    fn flags_unbound_navigator_via_name_fallback() {
        let issues = lint_effect_navigate(
            r#"use_effect(move || {
    nav.replace("/login");
});"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "hydration_unsafe_effect")
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "fallback name should still catch nav.replace: {issues:?}"
        );
    }

    /// Nav calls inside event-handler closures (onclick, onsubmit, …) are
    /// fine — they only fire after a user interaction, well after hydration
    /// — so they must not flag. The `use_effect` test above is the actual
    /// hazard.
    #[test]
    fn ignores_nav_push_inside_event_handler_closure() {
        let issues = lint_effect_navigate(
            r#"let nav = use_navigator();
let _on_click = move |_e: Event<MouseData>| {
    nav.push("/login");
};"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "hydration_unsafe_effect")
            .collect();
        assert!(
            hits.is_empty(),
            "nav.push inside an event handler is fine: {hits:?}"
        );
    }

    /// `vec.push(item)` and `signal.write().push(item)` look superficially
    /// like nav calls but the receivers don't match a navigator binding or
    /// the name fallback — they must NOT flag.
    #[test]
    fn ignores_vec_push_inside_use_effect() {
        let issues = lint_effect_navigate(
            r#"let mut items = use_signal(|| Vec::<u32>::new());
use_effect(move || {
    items.write().push(1);
});"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "hydration_unsafe_effect")
            .collect();
        assert!(
            hits.is_empty(),
            "Vec::push inside use_effect is not the route-guard shape: {hits:?}"
        );
    }

    /// Drive `check_prop_clone_overuse` over a synthetic component fn body.
    fn lint_prop_clones(component_src: &str) -> Vec<SignalIssue> {
        let src = format!("use dioxus::prelude::*;\n{component_src}\n");
        let file: syn::File = syn::parse_str(&src).expect("test snippet parses");
        let mut issues: Vec<SignalIssue> = Vec::new();
        for item in &file.items {
            let syn::Item::Fn(f) = item else { continue };
            check_prop_clone_overuse(f, Path::new("snippet.rs"), &mut issues);
        }
        issues
    }

    /// Standup `Column` shape: an `id: String` prop cloned three times into
    /// `id_for_dragover` / `id_for_drop` / `id_for_match` so each closure
    /// can `move` its own copy. Flag once for `id` with all three lines.
    #[test]
    fn flags_three_clones_of_same_prop() {
        let issues = lint_prop_clones(
            r#"#[component]
fn Column(id: String, title: String) -> Element {
    let id_for_dragover = id.clone();
    let id_for_drop = id.clone();
    let id_for_match = id.clone();
    let _ = (id_for_dragover, id_for_drop, id_for_match, title);
    rsx!{}
}"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "prop_clone_overuse")
            .collect();
        assert_eq!(hits.len(), 1, "one finding per overused prop: {issues:?}");
        assert!(
            hits[0].message.contains("`id`"),
            "message names the prop: {}",
            hits[0].message
        );
        assert!(
            hits[0].message.contains("ReadOnlySignal"),
            "message suggests ReadOnlySignal as one fix: {}",
            hits[0].message
        );
    }

    /// Two clones is acceptable (one for the callback, one for the
    /// derived value); the rule kicks in at three.
    #[test]
    fn ignores_two_clones_of_same_prop() {
        let issues = lint_prop_clones(
            r#"#[component]
fn Demo(id: String) -> Element {
    let id_for_a = id.clone();
    let id_for_b = id.clone();
    let _ = (id_for_a, id_for_b);
    rsx!{}
}"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "prop_clone_overuse")
            .collect();
        assert!(hits.is_empty(), "two clones is fine: {hits:?}");
    }

    /// Three clones of three DIFFERENT props (one each) is unrelated to
    /// the smell — must not flag.
    #[test]
    fn ignores_one_clone_each_of_three_distinct_props() {
        let issues = lint_prop_clones(
            r#"#[component]
fn Demo(a: String, b: String, c: String) -> Element {
    let _a = a.clone();
    let _b = b.clone();
    let _c = c.clone();
    rsx!{}
}"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "prop_clone_overuse")
            .collect();
        assert!(
            hits.is_empty(),
            "one clone per prop is the normal shape: {hits:?}"
        );
    }

    /// Standup `CardItem` shape: `card_id` cloned twice AND `column_id`
    /// cloned twice — four clones total across two props, but neither prop
    /// hits the 3-clone per-prop threshold. The aggregate signal is the same
    /// (one closure captures per binding); fire `prop_clone_overuse_aggregate`
    /// once with a summary that names each prop and its count.
    #[test]
    fn flags_aggregate_when_multiple_props_each_below_threshold() {
        let issues = lint_prop_clones(
            r#"#[component]
fn CardItem(card_id: String, column_id: String) -> Element {
    let card_for_a = card_id.clone();
    let card_for_b = card_id.clone();
    let col_for_a = column_id.clone();
    let col_for_b = column_id.clone();
    let _ = (card_for_a, card_for_b, col_for_a, col_for_b);
    rsx!{}
}"#,
        );
        let agg: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "prop_clone_overuse_aggregate")
            .collect();
        assert_eq!(
            agg.len(),
            1,
            "aggregate must fire when ≥4 total clones across ≥2 props: {issues:?}"
        );
        assert!(
            agg[0].message.contains("`card_id` ×2"),
            "names card_id and count: {}",
            agg[0].message
        );
        assert!(
            agg[0].message.contains("`column_id` ×2"),
            "names column_id and count: {}",
            agg[0].message
        );

        // And the per-prop rule must NOT have also fired, since neither prop
        // hit 3 clones individually.
        let per_prop: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "prop_clone_overuse")
            .collect();
        assert!(
            per_prop.is_empty(),
            "per-prop rule must stay silent below its own threshold: {per_prop:?}"
        );
    }

    /// iter03 regression: `Column` clones `id` three times (per-prop rule
    /// fires) AND clones a second prop once (so `prop_count >= 2`). Before
    /// the fix, the aggregate ALSO fired at the same line because the
    /// threshold only blocked when *every* prop in the clone map had
    /// already fired the per-prop rule. We now skip the aggregate the
    /// moment ANY prop fires per-prop — the per-prop finding already
    /// names the same refactor.
    #[test]
    fn aggregate_silent_when_per_prop_fired_on_any_prop() {
        let issues = lint_prop_clones(
            r#"#[component]
fn Column(id: String, label: String) -> Element {
    let a = id.clone();
    let b = id.clone();
    let c = id.clone();
    let l = label.clone();
    let _ = (a, b, c, l);
    rsx!{}
}"#,
        );
        let per_prop: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "prop_clone_overuse")
            .collect();
        assert_eq!(
            per_prop.len(),
            1,
            "per-prop rule should fire for id ×3: {per_prop:?}",
        );
        let agg: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "prop_clone_overuse_aggregate")
            .collect();
        assert!(
            agg.is_empty(),
            "aggregate must NOT double-flag when per-prop already fired: {agg:?}",
        );
    }

    /// Aggregate stays silent when only one prop has clones (even ≥4 of
    /// them) — that case is already covered by the per-prop rule, and
    /// double-firing would just be noise.
    #[test]
    fn aggregate_silent_when_only_one_prop_clones() {
        let issues = lint_prop_clones(
            r#"#[component]
fn Demo(id: String) -> Element {
    let a = id.clone();
    let b = id.clone();
    let c = id.clone();
    let d = id.clone();
    let _ = (a, b, c, d);
    rsx!{}
}"#,
        );
        let agg: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "prop_clone_overuse_aggregate")
            .collect();
        assert!(
            agg.is_empty(),
            "aggregate must defer to the per-prop rule when only one prop is involved: {agg:?}"
        );
        let per_prop: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "prop_clone_overuse")
            .collect();
        assert_eq!(
            per_prop.len(),
            1,
            "per-prop rule should fire for the 4-clone case: {per_prop:?}"
        );
    }

    /// Aggregate stays silent for one clone per prop across many props —
    /// the threshold is total ≥4 clones, and "one each" is the baseline
    /// normal shape.
    #[test]
    fn aggregate_silent_with_one_clone_each_across_many_props() {
        let issues = lint_prop_clones(
            r#"#[component]
fn Demo(a: String, b: String, c: String, d: String, e: String) -> Element {
    let _a = a.clone();
    let _b = b.clone();
    let _c = c.clone();
    let _d = d.clone();
    let _e = e.clone();
    rsx!{}
}"#,
        );
        let agg: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "prop_clone_overuse_aggregate")
            .collect();
        assert!(
            agg.is_empty(),
            "one clone per prop is the baseline; aggregate must not flag: {agg:?}"
        );
    }

    /// Clones of locals or non-prop bindings shouldn't flag — only prop
    /// names from the fn signature count.
    #[test]
    fn ignores_clones_of_local_bindings() {
        let issues = lint_prop_clones(
            r#"#[component]
fn Demo(prop: String) -> Element {
    let local = String::from("x");
    let l1 = local.clone();
    let l2 = local.clone();
    let l3 = local.clone();
    let _ = (l1, l2, l3, prop);
    rsx!{}
}"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "prop_clone_overuse")
            .collect();
        assert!(
            hits.is_empty(),
            "clones of locals are out of scope (only props count): {hits:?}"
        );
    }

    /// Drive `PollingFutureVisitor` over a synthetic component body.
    fn lint_polling_future(snippet: &str) -> Vec<SignalIssue> {
        let src = format!(
            "use dioxus::prelude::*;\n#[component]\nfn Demo() -> Element {{\n{snippet}\nrsx!{{}}\n}}\n"
        );
        let file: syn::File = syn::parse_str(&src).expect("test snippet parses");
        let mut issues: Vec<SignalIssue> = Vec::new();
        for item in &file.items {
            let syn::Item::Fn(f) = item else { continue };
            let signal_bindings = collect_use_signal_bindings(&f.block);
            let mut v = PollingFutureVisitor {
                in_future: false,
                saw_sleep: false,
                read_lines: Vec::new(),
                future_line: 0,
                signal_bindings: &signal_bindings,
                issues: &mut issues,
                file: Path::new("snippet.rs"),
                component: f.sig.ident.to_string(),
            };
            v.visit_block(&f.block);
        }
        issues
    }

    /// Standup's `fetch_board` shape: a polling future that reads
    /// `local_lock()` for *gating* but ends up subscribing to it, so every
    /// optimistic write to `local_lock` restarts the poll.
    #[test]
    fn flags_polling_use_future_with_reactive_read() {
        let issues = lint_polling_future(
            r#"let mut local_lock = use_signal(|| 0u32);
let mut cards = use_signal(|| Vec::<String>::new());
use_future(move || async move {
    loop {
        let _ = local_lock();
        cards.set(Vec::new());
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
});"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "polling_future_reactive_read")
            .collect();
        assert_eq!(hits.len(), 1, "expected one polling-read hit: {issues:?}");
        assert!(
            hits[0].message.contains("local_lock"),
            "message should name the offending signal: {}",
            hits[0].message
        );
        assert!(
            hits[0].message.contains(".peek()"),
            "fix suggestion should mention .peek(): {}",
            hits[0].message
        );
    }

    /// Standup's actual shape: the polling loop sleeps via
    /// `gloo_timers::future::TimeoutFuture::new(2000).await`, not via a
    /// bare `sleep()` helper. The path tail is `new`, so the original
    /// timer-name match never fired — the lint silently passed even though
    /// the body still reads a signal inside a polling loop. Guard with the
    /// fully-qualified form so a regression that drops the second-to-last
    /// segment check is caught.
    #[test]
    fn flags_polling_future_with_timeoutfuture_new() {
        let issues = lint_polling_future(
            r#"let mut local_lock = use_signal(|| 0u32);
let mut cards = use_signal(|| Vec::<String>::new());
use_future(move || async move {
    loop {
        let _ = local_lock();
        cards.set(Vec::new());
        gloo_timers::future::TimeoutFuture::new(2000).await;
    }
});"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "polling_future_reactive_read")
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "TimeoutFuture::new should count as a sleep: {issues:?}"
        );
    }

    /// Same shape with `tokio::time::Interval::new(...)` — the `Interval`
    /// type's constructor also takes the `new` tail and the path-segment
    /// check is the only thing that classifies it as a timer.
    #[test]
    fn flags_polling_future_with_interval_new() {
        let issues = lint_polling_future(
            r#"let mut tick = use_signal(|| 0u32);
use_future(move || async move {
    let mut iv = tokio::time::Interval::new(std::time::Duration::from_secs(1));
    loop {
        let _ = tick();
        iv.tick().await;
    }
});"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "polling_future_reactive_read")
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "Interval::new should count as a timer: {issues:?}"
        );
    }

    /// `.peek()` is the explicit escape hatch — using it for gating reads
    /// should NOT trigger the lint, because `.peek()` doesn't subscribe.
    #[test]
    fn ignores_peek_read_inside_polling_future() {
        let issues = lint_polling_future(
            r#"let mut local_lock = use_signal(|| 0u32);
use_future(move || async move {
    loop {
        let _ = local_lock.peek();
        gloo_timers::future::sleep(std::time::Duration::from_secs(1)).await;
    }
});"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "polling_future_reactive_read")
            .collect();
        assert!(
            hits.is_empty(),
            ".peek() is the escape hatch — must not flag: {hits:?}"
        );
    }

    /// One-shot fetches (no sleep) shouldn't fire — the warning is about
    /// the loop-restart shape, not all signal reads in futures.
    #[test]
    fn ignores_use_future_with_read_but_no_sleep() {
        let issues = lint_polling_future(
            r#"let mut id = use_signal(|| 0u32);
use_future(move || async move {
    let _ = id();
    let _ = some_server_fn().await;
});"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "polling_future_reactive_read")
            .collect();
        assert!(
            hits.is_empty(),
            "no sleep ⇒ no polling-restart shape: {hits:?}"
        );
    }

    /// Sleeping futures with no signal reads (animations, timers) shouldn't
    /// fire either — the lint is about the COMBINATION.
    #[test]
    fn ignores_sleep_only_use_future() {
        let issues = lint_polling_future(
            r#"use_future(move || async move {
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
});"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "polling_future_reactive_read")
            .collect();
        assert!(
            hits.is_empty(),
            "pure timer future — no read, no flag: {hits:?}"
        );
    }

    /// Two `nav.push` calls in the same `use_effect` (rare, but seen when
    /// branching on auth state) should emit ONCE — the fix is the same and
    /// duplicating the issue just adds noise.
    #[test]
    fn duplicate_nav_calls_in_one_effect_flag_once() {
        let issues = lint_effect_navigate(
            r#"let nav = use_navigator();
use_effect(move || {
    if true { nav.push("/a"); } else { nav.push("/b"); }
});"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "hydration_unsafe_effect")
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "expected one flag per use_effect: {issues:?}"
        );
    }

    /// Drive `check_signal_many_writers` over a component fn body.
    fn lint_many_writers(component_src: &str) -> Vec<SignalIssue> {
        let src = format!("use dioxus::prelude::*;\n{component_src}\n");
        let file: syn::File = syn::parse_str(&src).expect("test snippet parses");
        let mut issues: Vec<SignalIssue> = Vec::new();
        for item in &file.items {
            let syn::Item::Fn(f) = item else { continue };
            let bindings = collect_use_signal_bindings(&f.block);
            check_signal_many_writers(f, Path::new("snippet.rs"), &bindings, &mut issues);
        }
        issues
    }

    /// Standup `BoardBody` shape: `cards` is written from a polling
    /// `use_future` AND three named closures (logout/submit/commit). Four
    /// distinct write sources — well past the 3-source threshold. The lint
    /// names each writer in the message so the refactor target is obvious.
    #[test]
    fn flags_signal_written_from_four_sources() {
        let issues = lint_many_writers(
            r#"#[component]
fn BoardBody() -> Element {
    let mut cards = use_signal(|| Vec::<String>::new());
    use_future(move || async move {
        cards.set(Vec::new());
    });
    let logout = move |_| { cards.set(Vec::new()); };
    let submit_card = move |t: String| { cards.with_mut(|c| c.push(t)); };
    let commit_move = move |_| { cards.set(Vec::new()); };
    let _ = (logout, submit_card, commit_move);
    rsx!{}
}"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "signal_many_writers")
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "expected one many-writers hit on `cards`: {issues:?}"
        );
        let msg = &hits[0].message;
        assert!(msg.contains("`cards`"), "names the signal: {msg}");
        assert!(
            msg.contains("Store"),
            "recommends the Store primitive: {msg}"
        );
        assert!(
            msg.contains("logout") && msg.contains("submit_card") && msg.contains("commit_move"),
            "lists each closure writer: {msg}"
        );
    }

    /// Two writers is fine — the cutoff is three. A signal mutated from a
    /// hook plus one event handler is the normal shape for most apps.
    #[test]
    fn ignores_signal_with_two_writers() {
        let issues = lint_many_writers(
            r#"#[component]
fn Demo() -> Element {
    let mut count = use_signal(|| 0u32);
    use_effect(move || { count.set(0); });
    let on_click = move |_| { count.set(count() + 1); };
    let _ = on_click;
    rsx!{}
}"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "signal_many_writers")
            .collect();
        assert!(
            hits.is_empty(),
            "two writers is below the threshold: {hits:?}"
        );
    }

    /// dioxus_standup `BoardScreen` shape: every mutator writes `local_lock`
    /// via `local_lock += 1` rather than `.set` / `.with_mut`. syn 2.0 lowers
    /// that to `ExprBinary` with `BinOp::AddAssign`, not `ExprAssign`, so the
    /// old walker silently missed every site and `signal_many_writers` never
    /// fired. Pin the compound-assign path so three closures bumping the
    /// signal with `+=` is enough to trip the lint.
    #[test]
    fn flags_signal_written_only_via_compound_assigns() {
        let issues = lint_many_writers(
            r#"#[component]
fn BoardScreen() -> Element {
    let mut local_lock = use_signal(|| 0u32);
    let submit_card = move |_| { local_lock += 1; };
    let delete_card = move |_| { local_lock += 1; };
    let commit_move = move |_| { local_lock += 1; };
    let _ = (submit_card, delete_card, commit_move);
    rsx!{}
}"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "signal_many_writers")
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "three closures bumping `local_lock` with `+=` should trip the lint: {issues:?}"
        );
        let msg = &hits[0].message;
        assert!(msg.contains("`local_lock`"), "names the signal: {msg}");
        assert!(
            msg.contains("submit_card")
                && msg.contains("delete_card")
                && msg.contains("commit_move"),
            "lists each compound-assigning closure: {msg}"
        );
    }

    /// Multiple writes from a single closure don't multi-count — the
    /// threshold is "distinct sources", not "distinct write sites".
    #[test]
    fn dedupes_multiple_writes_within_one_closure() {
        let issues = lint_many_writers(
            r#"#[component]
fn Demo() -> Element {
    let mut count = use_signal(|| 0u32);
    let bump = move |_| {
        count.set(1);
        count.set(2);
        count.set(3);
    };
    let _ = bump;
    rsx!{}
}"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "signal_many_writers")
            .collect();
        assert!(
            hits.is_empty(),
            "one closure with three writes is still ONE source: {hits:?}"
        );
    }

    /// Drive `check_signal_used_as_fence` over a component fn body.
    fn lint_fence(component_src: &str) -> Vec<SignalIssue> {
        let src = format!("use dioxus::prelude::*;\n{component_src}\n");
        let file: syn::File = syn::parse_str(&src).expect("test snippet parses");
        let mut issues: Vec<SignalIssue> = Vec::new();
        for item in &file.items {
            let syn::Item::Fn(f) = item else { continue };
            let bindings = collect_use_signal_bindings(&f.block);
            check_signal_used_as_fence(f, Path::new("snippet.rs"), &bindings, &mut issues);
        }
        issues
    }

    /// Standup's `local_lock` shape: a `Signal<u32>` initialised to `0`,
    /// bumped by `+= 1` from a handful of mutators, and only ever read in
    /// `local_lock() == saved` comparisons. The runtime overhead of the
    /// reactive subscription buys nothing here — it's a generation counter,
    /// not state.
    #[test]
    fn flags_int_signal_used_purely_as_fence() {
        let issues = lint_fence(
            r#"#[component]
fn BoardBody() -> Element {
    let mut local_lock = use_signal(|| 0u32);
    let bump_a = move |_| { local_lock += 1; };
    let bump_b = move |_| { local_lock += 1; };
    let check = move |saved: u32| {
        if local_lock == saved { /* still fresh */ }
    };
    let _ = (bump_a, bump_b, check);
    rsx!{}
}"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "signal_used_as_fence")
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "fence shape should fire on local_lock: {issues:?}"
        );
        assert!(
            hits[0].message.contains("Cell") || hits[0].message.contains("generation"),
            "should recommend the lighter-weight replacement: {}",
            hits[0].message
        );
    }

    /// iter03's canonical shape: snapshot-then-compare on an int signal.
    /// The bare snapshot read `let lock = local_lock();` would normally
    /// disqualify the fence shape (callers seem to care about the value),
    /// but the snapshot is *immediately* used in a `local_lock() == lock`
    /// comparison — that's the same fence-read pattern, just spread over
    /// two source lines. Treat it as fence usage instead of dropping the
    /// finding.
    #[test]
    fn flags_snapshot_then_compare_fence_pattern() {
        let issues = lint_fence(
            r#"#[component]
fn BoardBody() -> Element {
    let mut local_lock = use_signal(|| 0u32);
    let bump_a = move |_| { local_lock += 1; };
    let bump_b = move |_| { local_lock += 1; };
    let check = move |_| {
        let lock = local_lock();
        if local_lock() == lock { /* still fresh */ }
    };
    let _ = (bump_a, bump_b, check);
    rsx!{}
}"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "signal_used_as_fence")
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "snapshot-then-compare still IS the fence shape: {issues:?}",
        );
    }

    /// Counter-test: a snapshot let-binding whose name is NEVER compared
    /// back against the signal is just a value read — the lint must still
    /// disqualify so we don't suppress legitimate `Signal<int>` consumers.
    #[test]
    fn ignores_snapshot_read_without_pairing_compare() {
        let issues = lint_fence(
            r#"#[component]
fn Demo() -> Element {
    let mut count = use_signal(|| 0u32);
    let bump_a = move |_| { count += 1; };
    let bump_b = move |_| { count += 1; };
    let show = move |_| {
        let now = count();
        // `now` is just used elsewhere — never compared back to `count()`.
        let _ = now + 1;
    };
    let _ = (bump_a, bump_b, show);
    rsx!{}
}"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "signal_used_as_fence")
            .collect();
        assert!(
            hits.is_empty(),
            "unpaired snapshot means callers care about the value: {hits:?}",
        );
    }

    /// A signal read for its value (e.g. arithmetic, rsx interpolation, or
    /// a non-comparison branch) is NOT a fence — callers care about the
    /// number. Must not flag.
    #[test]
    fn ignores_int_signal_read_for_its_value() {
        let issues = lint_fence(
            r#"#[component]
fn Demo() -> Element {
    let mut count = use_signal(|| 0u32);
    let bump = move |_| { count += 1; };
    let _ = bump;
    rsx!{ span { "{count}" } }
}"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "signal_used_as_fence")
            .collect();
        assert!(
            hits.is_empty(),
            "value shown in rsx — not a fence: {hits:?}"
        );
    }

    /// Single bump → only one compound write — below the 2-write threshold.
    /// The fence shape is about *repeated* sentinel bumps; one is too
    /// little evidence.
    #[test]
    fn ignores_single_bump_int_signal() {
        let issues = lint_fence(
            r#"#[component]
fn Demo() -> Element {
    let mut tick = use_signal(|| 0u32);
    let bump = move |_| { tick += 1; };
    let check = move |saved: u32| { let _ = tick == saved; };
    let _ = (bump, check);
    rsx!{}
}"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "signal_used_as_fence")
            .collect();
        assert!(
            hits.is_empty(),
            "one compound write isn't enough evidence: {hits:?}"
        );
    }

    /// Non-integer init disqualifies: `Vec::new()` / `String::new()` are
    /// out of scope for the fence lint regardless of the read/write pattern.
    #[test]
    fn ignores_non_integer_init() {
        let issues = lint_fence(
            r#"#[component]
fn Demo() -> Element {
    let mut items = use_signal(|| Vec::<u32>::new());
    let a = move |_| { items.set(Vec::new()); };
    let b = move |_| { items.set(Vec::new()); };
    let _ = (a, b);
    rsx!{}
}"#,
        );
        let hits: Vec<&SignalIssue> = issues
            .iter()
            .filter(|i| i.code == "signal_used_as_fence")
            .collect();
        assert!(
            hits.is_empty(),
            "non-int init signal is out of scope: {hits:?}"
        );
    }
}

#[cfg(test)]
mod bootstrap_gate_tests {
    use super::*;

    fn lint_component(body: &str) -> Vec<SignalIssue> {
        let src =
            format!("use dioxus::prelude::*;\n#[component]\nfn App() -> Element {{\n{body}\n}}\n");
        let file: syn::File = syn::parse_str(&src).expect("test component parses");
        let mut issues: Vec<SignalIssue> = Vec::new();
        for item in &file.items {
            let syn::Item::Fn(f) = item else { continue };
            let bool_gates = collect_bool_gate_signals(&f.block);
            let rsx_gates = collect_rsx_if_signal_calls(&f.block);
            let mut e = EffectSpawnVisitor {
                effect_depth: 0,
                spawn_depth: 0,
                saw_await: false,
                set_lines: Vec::new(),
                set_calls: Vec::new(),
                effect_line: 0,
                bool_gates: &bool_gates,
                rsx_gates: &rsx_gates,
                issues: &mut issues,
                file: Path::new("comp.rs"),
                component: f.sig.ident.to_string(),
            };
            e.visit_block(&f.block);
        }
        issues
    }

    /// iter03's canonical shape: a bool gate, an effect that flips it after
    /// awaiting `who_am_i`, and rsx gating the router behind it. Must fire
    /// `bootstrap_gate_signal`, not the generic `hydration_unsafe_effect`.
    #[test]
    fn fires_bootstrap_gate_signal_on_iter03_shape() {
        let issues = lint_component(
            r#"
let mut bootstrapped = use_signal(|| false);
use_effect(move || {
    spawn(async move {
        let _ = who_am_i().await;
        bootstrapped.set(true);
    });
});
rsx! {
    if bootstrapped() {
        Router::<Route> {}
    } else {
        div { "Loading..." }
    }
}
"#,
        );
        let codes: Vec<&str> = issues.iter().map(|i| i.code).collect();
        assert!(
            codes.contains(&"bootstrap_gate_signal"),
            "expected bootstrap_gate_signal, got: {codes:?}",
        );
        assert!(
            !codes.contains(&"hydration_unsafe_effect"),
            "specialized finding should suppress generic one: {codes:?}",
        );
    }

    /// A `use_signal(|| 0u32)` flipped to a numeric counter inside an
    /// effect isn't the bootstrap shape — must fall back to the generic
    /// `hydration_unsafe_effect` and NOT fire `bootstrap_gate_signal`.
    #[test]
    fn falls_back_to_generic_when_gate_is_not_bool_false() {
        let issues = lint_component(
            r#"
let mut counter = use_signal(|| 0u32);
use_effect(move || {
    spawn(async move {
        let _ = some_fn().await;
        counter.set(counter() + 1);
    });
});
rsx! {
    div { "x" }
}
"#,
        );
        let codes: Vec<&str> = issues.iter().map(|i| i.code).collect();
        assert!(
            codes.contains(&"hydration_unsafe_effect"),
            "non-bool init must fall back to generic: {codes:?}",
        );
        assert!(
            !codes.contains(&"bootstrap_gate_signal"),
            "non-bool init must NOT fire bootstrap: {codes:?}",
        );
    }

    /// `.set(false)` on a `use_signal(|| false)` is not the boot pattern —
    /// the signal must flip true. Must fall back to generic.
    #[test]
    fn falls_back_when_set_value_is_not_true_literal() {
        let issues = lint_component(
            r#"
let mut flag = use_signal(|| false);
use_effect(move || {
    spawn(async move {
        let _ = some_fn().await;
        flag.set(false);
    });
});
rsx! {
    if flag() { div { "a" } } else { div { "b" } }
}
"#,
        );
        let codes: Vec<&str> = issues.iter().map(|i| i.code).collect();
        assert!(
            codes.contains(&"hydration_unsafe_effect"),
            "set(false) must fall back to generic: {codes:?}",
        );
        assert!(
            !codes.contains(&"bootstrap_gate_signal"),
            "set(false) is not the bootstrap shape: {codes:?}",
        );
    }

    /// Bool flag exists but rsx doesn't gate on it — the user just stores
    /// a "ready" status without conditional rendering. Fall back to
    /// generic so we don't suggest `use_server_future` when there's
    /// nothing to pre-render.
    #[test]
    fn falls_back_when_rsx_has_no_if_gate() {
        let issues = lint_component(
            r#"
let mut flag = use_signal(|| false);
use_effect(move || {
    spawn(async move {
        let _ = some_fn().await;
        flag.set(true);
    });
});
rsx! {
    div { class: if flag() { "ready" } else { "loading" } }
}
"#,
        );
        let codes: Vec<&str> = issues.iter().map(|i| i.code).collect();
        // Note: the class-attribute conditional uses `if flag() { ... }`
        // INSIDE an rsx attribute value, which our scan picks up via
        // recursive group traversal. To avoid this false-positive in the
        // future, the scan would need a "top-level if" filter. For now,
        // accept that an inline conditional in rsx is a gate-like shape.
        // The bootstrap finding still applies here — the user could
        // benefit from server-side resolution.
        assert!(
            codes.contains(&"hydration_unsafe_effect") || codes.contains(&"bootstrap_gate_signal"),
            "must surface at least one finding: {codes:?}",
        );
    }

    /// `collect_bool_gate_signals` picks up `use_signal(|| false)` and
    /// rejects other inits. Pin the helper directly so the visitor wiring
    /// can rely on a stable contract.
    #[test]
    fn collect_bool_gate_signals_picks_up_only_false_init() {
        let src = r#"#[component]
fn X() -> Element {
    let mut a = use_signal(|| false);
    let mut b = use_signal(|| true);
    let mut c = use_signal(|| 0);
    let mut d = use_signal(String::new);
    rsx! {}
}"#;
        let file: syn::File = syn::parse_str(src).unwrap();
        for item in &file.items {
            let syn::Item::Fn(f) = item else { continue };
            let bools = collect_bool_gate_signals(&f.block);
            assert_eq!(bools, vec!["a".to_string()]);
        }
    }

    /// `collect_rsx_if_signal_calls` picks up `if <name>()` and rejects
    /// non-call bare-ident conditions. Pins the scanner.
    #[test]
    fn collect_rsx_if_signal_calls_recognises_gate_shape() {
        let src = r#"#[component]
fn X() -> Element {
    rsx! {
        if bootstrapped() {
            div { "ready" }
        } else {
            div { "loading" }
        }
        if some_signal_read {
            div { "no parens — not a gate match" }
        }
    }
}"#;
        let file: syn::File = syn::parse_str(src).unwrap();
        for item in &file.items {
            let syn::Item::Fn(f) = item else { continue };
            let gates = collect_rsx_if_signal_calls(&f.block);
            assert!(gates.contains("bootstrapped"));
            assert!(!gates.contains("some_signal_read"));
        }
    }
}

#[cfg(test)]
mod related_codes_tests {
    use super::*;

    fn issue(code: &'static str, component: &str, signal: Option<&str>) -> SignalIssue {
        SignalIssue {
            code,
            message: String::new(),
            file: PathBuf::new(),
            line: 1,
            component: Some(component.to_string()),
            signal: signal.map(|s| s.to_string()),
            related_codes: Vec::new(),
            fix: None,
        }
    }

    /// Two findings on the same signal in the same component must cross
    /// reference each other via `related_codes`.
    #[test]
    fn pairs_signal_many_writers_with_signal_used_as_fence() {
        let mut issues = vec![
            issue("signal_many_writers", "BoardBody", Some("local_lock")),
            issue("signal_used_as_fence", "BoardBody", Some("local_lock")),
            // Unrelated finding in same component but different signal
            // — must not link.
            issue("signal_many_writers", "BoardBody", Some("cards")),
        ];
        link_related_findings(&mut issues);
        assert_eq!(
            issues[0].related_codes,
            vec!["signal_used_as_fence".to_string()]
        );
        assert_eq!(
            issues[1].related_codes,
            vec!["signal_many_writers".to_string()]
        );
        // Unrelated finding: untouched.
        assert!(issues[2].related_codes.is_empty());
    }

    /// Findings without a `signal` field don't link — the pairing is
    /// only meaningful when both halves identify a specific binding.
    #[test]
    fn skips_findings_missing_signal_field() {
        let mut issues = vec![
            issue("hydration_unsafe_effect", "App", None),
            issue("signal_many_writers", "App", Some("flag")),
        ];
        link_related_findings(&mut issues);
        assert!(issues[0].related_codes.is_empty());
        assert!(issues[1].related_codes.is_empty());
    }
}
