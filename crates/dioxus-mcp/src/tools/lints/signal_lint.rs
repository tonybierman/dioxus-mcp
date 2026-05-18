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
}
