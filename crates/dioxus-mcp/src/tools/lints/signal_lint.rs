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
}

#[derive(Debug, Serialize)]
pub struct SignalLintReport {
    pub issues: Vec<SignalIssue>,
    /// Suggestions to collapse 3+ sibling `provide_X` / `use_X` context-signal
    /// modules into one `Store`. Empty when the project has fewer than 3 such
    /// modules — a small number of bespoke context signals is fine.
    #[serde(default)]
    pub context_signal_triads: Vec<ContextSignalTriad>,
    pub parse_errors: Vec<ParseError>,
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
            let mut e = EffectSpawnVisitor {
                effect_depth: 0,
                spawn_depth: 0,
                saw_await: false,
                set_lines: Vec::new(),
                effect_line: 0,
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
        }
    }

    let context_signal_triads = detect_context_signal_triads(&files);

    Ok(SignalLintReport {
        issues,
        context_signal_triads,
        parse_errors: collect_parse_errors(&files),
    })
}

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
) -> Vec<ContextSignalTriad> {
    let mut modules: Vec<ContextSignalModule> = Vec::new();
    for sf in files {
        let Ok(ast) = &sf.ast else { continue };
        let mut provides: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let mut uses: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for item in &ast.items {
            let syn::Item::Fn(f) = item else { continue };
            if !matches!(f.vis, syn::Visibility::Public(_)) {
                continue;
            }
            let name = f.sig.ident.to_string();
            let line = f.sig.ident.span().start().line;
            if let Some(suffix) = name.strip_prefix("provide_") {
                provides.insert(suffix.to_string(), line);
            } else if let Some(suffix) = name.strip_prefix("use_") {
                uses.insert(suffix.to_string(), line);
            }
        }
        for (suffix, provide_line) in &provides {
            if let Some(use_line) = uses.get(suffix) {
                modules.push(ContextSignalModule {
                    file: sf.path.clone(),
                    name: suffix.clone(),
                    provide_line: *provide_line,
                    use_line: *use_line,
                });
            }
        }
    }
    if modules.len() < 3 {
        return Vec::new();
    }
    // Stable, human-friendly order: sort by file path so the report is
    // deterministic across runs and OS-specific dir-read orders.
    modules.sort_by(|a, b| a.file.cmp(&b.file).then(a.name.cmp(&b.name)));
    let names: Vec<String> = modules.iter().map(|m| m.name.clone()).collect();
    let message = format!(
        "{} sibling context-signal modules detected ({}). Three or more `provide_X` + `use_X` pairs is a smell — consolidate into a single `Store` (see the `Store` primitive in `get_dsl_spec`) so callers share one provider and one type, instead of N near-identical files.",
        modules.len(),
        names.join(", ")
    );
    vec![ContextSignalTriad { modules, message }]
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
    /// Line of the enclosing `use_effect(` call — used as the issue line so
    /// the report points at the hook (where the fix lives) rather than at
    /// the buried `.set`.
    effect_line: usize,
    issues: &'a mut Vec<SignalIssue>,
    file: &'a std::path::Path,
    component: String,
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
            self.saw_await = false;
            syn::visit::visit_expr_call(self, e);
            self.flush_pending_issues();
            self.saw_await = saved_await;
            self.set_lines = saved_sets;
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
            self.set_lines.push(e.method.span().start().line);
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
        });
        self.set_lines.clear();
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

    for (prop, lines) in &clones {
        if lines.len() < 3 {
            continue;
        }
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
        });
    }
}

/// Pre-scan a component body for `let X = use_signal(...)` bindings — the
/// set of names whose reactive reads matter for the polling-future lint.
/// Mirrors the scope inspector in `explain_signal_graph`, but lives here so
/// the signal lints stay independent.
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
        // `wasm-bindgen-futures`. We only need the path *tail* to match.
        if self.in_future
            && let Some(t) = tail.as_deref()
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
        let triads = detect_context_signal_triads(&files);
        assert!(
            triads.is_empty(),
            "two paired modules is not a triad: {triads:?}"
        );
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
        let triads = detect_context_signal_triads(&files);
        assert_eq!(triads.len(), 1, "expected one triad suggestion: {triads:?}");
        let names: Vec<&str> = triads[0].modules.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"theme"));
        assert!(names.contains(&"user"));
        assert!(names.contains(&"locale"));
        assert!(triads[0].message.contains("Store"));
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
        let triads = detect_context_signal_triads(&files);
        assert!(
            triads.is_empty(),
            "lone halves don't form pairs: {triads:?}"
        );
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
        let triads = detect_context_signal_triads(&files);
        assert!(
            triads.is_empty(),
            "private helpers aren't the public context-signal idiom: {triads:?}"
        );
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
            let mut v = EffectSpawnVisitor {
                effect_depth: 0,
                spawn_depth: 0,
                saw_await: false,
                set_lines: Vec::new(),
                effect_line: 0,
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
}
