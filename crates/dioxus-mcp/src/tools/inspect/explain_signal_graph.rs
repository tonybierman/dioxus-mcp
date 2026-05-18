use std::path::PathBuf;
use std::sync::Arc;

use proc_macro2::TokenTree;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::visit::Visit;

use crate::state::State;
use crate::tools::resolve_in_project;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ExplainSignalGraphParams {
    pub file: String,
    /// Optional component name. If omitted, every #[component] in the file is analyzed.
    pub component: Option<String>,
    /// Absolute path to the Dioxus project root. Required when `file` is relative and the
    /// server was not started in the target project directory.
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct SignalNode {
    pub name: String,
    /// `signal` | `memo` | `resource` | `effect` | `future` | `callback`.
    /// `future` covers `use_future` (one-off task; reactive on signal reads).
    /// `callback` covers `use_callback` (memoized closure; useful for sharing
    /// async handlers across handlers).
    pub kind: String,
    pub line: usize,
    /// Signals (other `SignalNode` names) that this node's body reads. The
    /// scan descends into nested closures, `async move { … }` blocks, and
    /// `spawn(…)` calls — so the reads list reflects every signal accessed
    /// inside a memo/effect/future/resource closure, not just the top-level
    /// init expression. **Writes** (`sig.set(…)`, `sig.write()`,
    /// `sig.with_mut(…)`, plain `sig = …` and `*sig = …`) are tracked
    /// separately under `writes` and do *not* appear here — callers asking
    /// "what re-triggers this closure?" want subscription points, not
    /// dependency-free emissions.
    pub reads: Vec<String>,
    /// Signals (other `SignalNode` names) this node's body *writes* — via
    /// `sig.set(…)`, `sig.write()`, `sig.with_mut(…)`, `sig.replace(…)`,
    /// `sig.swap(…)`, `sig.take()`, `sig.set_silent(…)`, `sig = …`, or
    /// `*sig = …`. Split out from `reads` so a caller can tell at a glance
    /// which closures emit vs. which subscribe. A signal that's both
    /// `sig.set(sig() + 1)` will appear in BOTH lists.
    #[serde(default)]
    pub writes: Vec<String>,
    /// Other `SignalNode` names whose body reads THIS signal — the inverse
    /// projection of `reads` (not `writes`). Lets a caller spot which leaf
    /// `use_signal`s are consumed by which closure-bound handler / effect
    /// without re-walking the graph.
    pub read_by: Vec<String>,
    /// True when this signal is referenced anywhere in the component's `rsx!`
    /// invocations — either as a formatted-string interpolation (`{sig}`) or
    /// as a bare ident / call (`sig`, `sig()`, `sig.read()`). A leaf
    /// `use_signal` will have `reads: []` but still be consumed by rsx; this
    /// flag distinguishes "consumed by rendering" from "truly unused".
    pub read_in_rsx: bool,
    /// Identifiers this node touches that are bound to *other* `use_*`
    /// helpers (e.g. `let presence = use_presence();`,
    /// `let session = use_context::<Signal<Session>>();`,
    /// `let nav = use_navigator();`). The graph walker can't follow the
    /// helper's return type into a Signal, so these aren't promoted to
    /// first-class nodes — but listing them makes hidden reactivity visible.
    /// A `<use_future@52> context_signals: ["presence"]` tells the reader
    /// "this future depends on something the static analysis couldn't
    /// classify — likely a context-provided Signal." Non-reactive helpers
    /// (e.g. `use_navigator`) will land here too; the reader disambiguates.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_signals: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ComponentGraph {
    pub component: String,
    pub nodes: Vec<SignalNode>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ExplainSignalGraphReport {
    pub file: PathBuf,
    pub components: Vec<ComponentGraph>,
}

pub async fn explain_signal_graph(
    state: &Arc<State>,
    p: ExplainSignalGraphParams,
) -> Result<ExplainSignalGraphReport, String> {
    let path = resolve_in_project(state, &p.file, p.project_root.as_deref()).await;
    let src = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let file = syn::parse_file(&src).map_err(|e| format!("rust parse error: {e}"))?;

    let mut out = Vec::new();
    for item in &file.items {
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
        let name = f.sig.ident.to_string();
        if let Some(filter) = &p.component
            && &name != filter
        {
            continue;
        }

        let nodes = analyze_component_body(&f.block);
        let warnings = lint_signal_graph(&nodes);
        out.push(ComponentGraph {
            component: name,
            nodes,
            warnings,
        });
    }

    Ok(ExplainSignalGraphReport {
        file: path,
        components: out,
    })
}

fn analyze_component_body(block: &syn::Block) -> Vec<SignalNode> {
    let mut nodes: Vec<SignalNode> = Vec::new();
    let mut known_bindings: Vec<String> = Vec::new();

    // First pass: pick up every `let foo = use_*(…)` binding AND every
    // standalone `use_effect(…)` / `use_future(…)` / `use_resource(…)` call
    // (statement expression with no `let` on the left). The standalone form
    // is what real-world fullstack apps use for polling loops, presence
    // heartbeats, and side-effecting reactions — without seeding nodes for
    // them, every signal they read shows up as `read_by: []`.
    let mut pending: Vec<(String, &'static str, usize, &syn::Expr)> = Vec::new();
    for stmt in &block.stmts {
        match stmt {
            syn::Stmt::Local(local) => {
                let Some(init) = &local.init else { continue };
                let Some(kind) = classify_init_call(&init.expr) else {
                    continue;
                };

                let binding_name = match &local.pat {
                    syn::Pat::Ident(p) => p.ident.to_string(),
                    syn::Pat::Type(t) => match &*t.pat {
                        syn::Pat::Ident(p) => p.ident.to_string(),
                        _ => "<unnamed>".into(),
                    },
                    _ => "<unnamed>".into(),
                };
                let line = local.let_token.span.start().line;
                pending.push((binding_name.clone(), kind, line, &init.expr));
                known_bindings.push(binding_name);
            }
            // `use_future(...);` — a statement expression with a trailing
            // semicolon. We attach a synthetic node so its closure body
            // contributes to `read_by` for the signals it touches. The name
            // encodes the `use_*` form + line for traceability:
            // `<use_future@33>`.
            syn::Stmt::Expr(expr, semi) if semi.is_some() => {
                if let Some(kind) = classify_init_call(expr) {
                    let line = stmt_line(stmt);
                    let use_form = use_form_for_kind(kind);
                    let synthetic = format!("<{use_form}@{line}>");
                    pending.push((synthetic.clone(), kind, line, expr));
                    known_bindings.push(synthetic);
                }
            }
            _ => {}
        }
    }
    // Bindings that look like context-provided signals or other `use_*`
    // helpers — included so we can attribute "hidden reactivity" touches to
    // nodes via `context_signals`. Computed once, then reused per node.
    let context_bindings = collect_context_bindings(block, &known_bindings);
    for (binding_name, kind, line, expr) in pending {
        let (reads, writes) = collect_reads_writes(expr, &known_bindings);
        let context_signals = collect_known_references(expr, &context_bindings);
        nodes.push(SignalNode {
            name: binding_name,
            kind: kind.into(),
            line,
            reads,
            writes,
            read_by: Vec::new(),
            read_in_rsx: false,
            context_signals,
        });
    }

    // Inverse projection: for each node, accumulate the names of *other*
    // nodes whose `reads` mention it. Helps spot which leaf `use_signal`s
    // are consumed by which closure-bound handler / effect.
    let read_by_map: std::collections::BTreeMap<String, Vec<String>> = {
        let mut m: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for n in &nodes {
            for r in &n.reads {
                m.entry(r.clone()).or_default().push(n.name.clone());
            }
        }
        m
    };
    for node in &mut nodes {
        if let Some(rb) = read_by_map.get(&node.name) {
            node.read_by = rb.clone();
        }
    }

    // Second pass: walk every statement and the trailing expression looking
    // for macro invocations (notably `rsx!`). Inside a macro the tokens are
    // not parsed as Rust expressions, so we walk the raw `TokenStream` for
    // ident references to a known binding. We also pick up
    // `format!`-style interpolations: `{sig}` is tokenized as the literal
    // `"{sig}"`, so we scan literal strings for `{name[..]}` placeholders.
    let mut rsx_hits: std::collections::HashSet<String> = std::collections::HashSet::new();
    for stmt in &block.stmts {
        collect_macro_idents(stmt_to_tokens_visitor(stmt), &known_bindings, &mut rsx_hits);
    }

    for node in &mut nodes {
        if rsx_hits.contains(&node.name) {
            node.read_in_rsx = true;
        }
    }

    nodes
}

/// Tiny adapter: produce an "iter all macros nested in this statement" by
/// walking the syn tree and feeding their token streams to a callback.
fn stmt_to_tokens_visitor(stmt: &syn::Stmt) -> Vec<proc_macro2::TokenStream> {
    struct MacroFinder {
        macros: Vec<proc_macro2::TokenStream>,
    }
    impl<'ast> syn::visit::Visit<'ast> for MacroFinder {
        fn visit_macro(&mut self, m: &'ast syn::Macro) {
            self.macros.push(m.tokens.clone());
            syn::visit::visit_macro(self, m);
        }
    }
    let mut f = MacroFinder { macros: Vec::new() };
    f.visit_stmt(stmt);
    f.macros
}

/// Walk a list of macro token streams and add any ident reference (or
/// `{name…}` interpolation in a string literal) matching one of `known` to
/// `hits`.
fn collect_macro_idents(
    streams: Vec<proc_macro2::TokenStream>,
    known: &[String],
    hits: &mut std::collections::HashSet<String>,
) {
    fn walk(
        ts: proc_macro2::TokenStream,
        known: &[String],
        hits: &mut std::collections::HashSet<String>,
    ) {
        for tt in ts {
            match tt {
                TokenTree::Group(g) => walk(g.stream(), known, hits),
                TokenTree::Ident(i) => {
                    let s = i.to_string();
                    if known.iter().any(|k| k == &s) {
                        hits.insert(s);
                    }
                }
                TokenTree::Literal(lit) => {
                    // Strip surrounding quotes when present, then look for
                    // `{name}` / `{name:fmt}` / `{name.field}` placeholders.
                    let s = lit.to_string();
                    let inner = s.strip_prefix('"').and_then(|s| s.strip_suffix('"'));
                    if let Some(inner) = inner {
                        scan_interpolations(inner, known, hits);
                    }
                }
                TokenTree::Punct(_) => {}
            }
        }
    }
    for s in streams {
        walk(s, known, hits);
    }
}

/// Pick `{ident…}` placeholders out of a format string and add matching
/// known names to `hits`. Handles `{name}`, `{name:fmt}`, `{name.field}` and
/// `{{` / `}}` escapes.
fn scan_interpolations(s: &str, known: &[String], hits: &mut std::collections::HashSet<String>) {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' if i + 1 < bytes.len() && bytes[i + 1] == b'{' => {
                i += 2;
            }
            b'{' => {
                let start = i + 1;
                let mut end = start;
                while end < bytes.len() && bytes[end] != b'}' && bytes[end] != b':' {
                    end += 1;
                }
                let token = &s[start..end];
                // Strip trailing `.field` so `{sig.read()}` still matches `sig`.
                let head = token.split(['.', '(', ' ']).next().unwrap_or("");
                if !head.is_empty() && known.iter().any(|k| k == head) {
                    hits.insert(head.to_string());
                }
                // Skip past the closing `}` if present
                while i < bytes.len() && bytes[i] != b'}' {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
}

/// Approximate the source line of a statement using whatever token we can
/// reach — for standalone `use_future(...);` calls there's no `let` token,
/// so we fall back to the leading token of the expression. Used to name
/// synthetic nodes (`<use_future@33>`) so `read_by` entries stay traceable.
fn stmt_line(stmt: &syn::Stmt) -> usize {
    match stmt {
        syn::Stmt::Local(l) => l.let_token.span.start().line,
        syn::Stmt::Expr(e, _) => expr_line(e),
        syn::Stmt::Item(_) | syn::Stmt::Macro(_) => 0,
    }
}

fn expr_line(expr: &syn::Expr) -> usize {
    use syn::spanned::Spanned;
    expr.span().start().line
}

/// Inverse of `classify_init_call`: reconstruct the `use_*` form a `kind`
/// came from, so synthetic node names mirror what the reader sees in source.
fn use_form_for_kind(kind: &str) -> &'static str {
    match kind {
        "signal" => "use_signal",
        "memo" => "use_memo",
        "resource" => "use_resource",
        "effect" => "use_effect",
        "future" => "use_future",
        "callback" => "use_callback",
        _ => "use_?",
    }
}

fn classify_init_call(expr: &syn::Expr) -> Option<&'static str> {
    let call = match expr {
        syn::Expr::Call(c) => c,
        syn::Expr::MethodCall(m) => return classify_init_call(&m.receiver),
        syn::Expr::Try(t) => return classify_init_call(&t.expr),
        syn::Expr::Await(a) => return classify_init_call(&a.base),
        _ => return None,
    };
    let syn::Expr::Path(p) = &*call.func else {
        return None;
    };
    let last = p.path.segments.last()?.ident.to_string();
    match last.as_str() {
        "use_signal" => Some("signal"),
        "use_memo" => Some("memo"),
        "use_resource" => Some("resource"),
        "use_effect" => Some("effect"),
        "use_future" => Some("future"),
        "use_callback" => Some("callback"),
        _ => None,
    }
}

/// Collect every `let foo = use_X(...)` binding where `use_X` is *not* one
/// of the well-known hooks (`use_signal`/`use_memo`/etc — those are tracked
/// as full nodes). The result feeds `SignalNode.context_signals` so an
/// effect that touches `presence` (from `use_presence()` returning a
/// context Signal) doesn't silently report `reads: []` when the static
/// walker can't follow into the helper.
fn collect_context_bindings(block: &syn::Block, known_bindings: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for stmt in &block.stmts {
        let syn::Stmt::Local(local) = stmt else {
            continue;
        };
        let Some(init) = &local.init else { continue };
        // Skip the standard hooks — they're already first-class nodes.
        if classify_init_call(&init.expr).is_some() {
            continue;
        }
        if !init_is_use_call(&init.expr) {
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
        // Don't double-classify when a name also matched a known binding
        // (shouldn't happen with rust shadowing rules but cheap to guard).
        if known_bindings.iter().any(|k| k == &name) || out.iter().any(|k| k == &name) {
            continue;
        }
        out.push(name);
    }
    out
}

/// True when `expr` is (transitively) a call to a function whose final path
/// segment starts with `use_`. Matches bare calls and method chains
/// (`use_session().unwrap()`, `use_x()?`, `use_x().await`).
fn init_is_use_call(expr: &syn::Expr) -> bool {
    match expr {
        syn::Expr::Call(c) => match &*c.func {
            syn::Expr::Path(p) => p
                .path
                .segments
                .last()
                .map(|s| s.ident.to_string().starts_with("use_"))
                .unwrap_or(false),
            _ => false,
        },
        syn::Expr::MethodCall(m) => init_is_use_call(&m.receiver),
        syn::Expr::Try(t) => init_is_use_call(&t.expr),
        syn::Expr::Await(a) => init_is_use_call(&a.base),
        syn::Expr::Paren(p) => init_is_use_call(&p.expr),
        _ => false,
    }
}

/// Walk `expr` and record every ident reference matching a name in
/// `known` (in source order, deduplicated). Used to attribute
/// `context_signals` touches per node without needing the read/write
/// partition — a context binding mention is interesting either way.
fn collect_known_references(expr: &syn::Expr, known: &[String]) -> Vec<String> {
    if known.is_empty() {
        return Vec::new();
    }
    struct R<'a> {
        known: &'a [String],
        hits: Vec<String>,
    }
    impl<'a, 'ast> Visit<'ast> for R<'a> {
        fn visit_ident(&mut self, i: &'ast syn::Ident) {
            let s = i.to_string();
            if self.known.iter().any(|k| k == &s) && !self.hits.contains(&s) {
                self.hits.push(s);
            }
        }
    }
    let mut r = R {
        known,
        hits: Vec::new(),
    };
    r.visit_expr(expr);
    r.hits
}

/// Walk `expr` and partition every known-binding reference into reads vs
/// writes. A reference counts as a *write* when the binding is the receiver
/// of a Signal mutation method (`.set`, `.write`, `.with_mut`, `.replace`,
/// `.swap`, `.take`, `.set_silent`) or the LHS of `=` / `*sig = …`.
/// Everything else — including the inner `cards` in `cards.set(cards() + 1)`
/// — is a read. A signal that is both read and written ends up in BOTH
/// lists, which is what callers usually want (it subscribes AND emits).
fn collect_reads_writes(expr: &syn::Expr, known: &[String]) -> (Vec<String>, Vec<String>) {
    struct V<'a> {
        known: &'a [String],
        reads: Vec<String>,
        writes: Vec<String>,
    }
    impl<'a> V<'a> {
        fn note_write(&mut self, name: &str) {
            if self.known.iter().any(|k| k == name) && !self.writes.iter().any(|w| w == name) {
                self.writes.push(name.to_string());
            }
        }
    }
    impl<'a, 'ast> Visit<'ast> for V<'a> {
        fn visit_expr_method_call(&mut self, mc: &'ast syn::ExprMethodCall) {
            // The set of methods that mutate a Signal. `write` returns a
            // write handle; we attribute the write to the receiver regardless
            // of whether the user chains `.push(...)` after — by the time
            // `.write()` is called the subscription has already fired.
            let is_write = matches!(
                mc.method.to_string().as_str(),
                "set" | "set_silent" | "write" | "with_mut" | "replace" | "swap" | "take"
            );
            if is_write && let Some(name) = root_ident(&mc.receiver) {
                self.note_write(&name);
                // Skip default recursion (which would re-visit the receiver
                // as an ident and double-count it as a read). Still walk the
                // arguments — `sig.set(other_sig() + 1)` reads other_sig.
                for a in &mc.args {
                    self.visit_expr(a);
                }
                return;
            }
            syn::visit::visit_expr_method_call(self, mc);
        }
        fn visit_expr_assign(&mut self, ea: &'ast syn::ExprAssign) {
            // `sig = x` and `*sig = x` are both writes. `root_ident` peels
            // `Unary` / `Reference` / `Paren` so the deref form is caught
            // alongside the direct form.
            if let Some(name) = root_ident(&ea.left) {
                self.note_write(&name);
                self.visit_expr(&ea.right);
                return;
            }
            syn::visit::visit_expr_assign(self, ea);
        }
        fn visit_ident(&mut self, i: &'ast syn::Ident) {
            let s = i.to_string();
            if self.known.iter().any(|k| k == &s) && !self.reads.iter().any(|r| r == &s) {
                self.reads.push(s);
            }
        }
    }
    let mut v = V {
        known,
        reads: Vec::new(),
        writes: Vec::new(),
    };
    v.visit_expr(expr);
    (v.reads, v.writes)
}

/// Peel `Reference` / `Paren` / `Unary` / single-segment `Path` until we
/// reach a bare identifier — used to classify the target of a write so
/// `*sig = …`, `(&mut sig).set(…)`, and `sig.set(…)` all attribute the
/// write to `sig`. Returns `None` for receivers like `state.lock()` where
/// the write target isn't a simple binding.
fn root_ident(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Path(p)
            if p.path.segments.len() == 1
                && p.path.leading_colon.is_none()
                && p.qself.is_none() =>
        {
            Some(p.path.segments[0].ident.to_string())
        }
        syn::Expr::Reference(r) => root_ident(&r.expr),
        syn::Expr::Paren(p) => root_ident(&p.expr),
        syn::Expr::Unary(u) => root_ident(&u.expr),
        _ => None,
    }
}

fn lint_signal_graph(nodes: &[SignalNode]) -> Vec<String> {
    let mut out = Vec::new();
    for n in nodes {
        if (n.kind == "memo" || n.kind == "effect") && n.reads.is_empty() {
            out.push(format!(
                "`{}` is a {} that captures no other signals — it will never re-run on state change",
                n.name, n.kind
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyze(src: &str) -> Vec<SignalNode> {
        let file = syn::parse_file(src).expect("parse");
        let item = file
            .items
            .iter()
            .find_map(|i| match i {
                syn::Item::Fn(f) => Some(f),
                _ => None,
            })
            .expect("fn item");
        analyze_component_body(&item.block)
    }

    #[test]
    fn use_future_and_use_callback_are_classified() {
        let nodes = analyze(
            r#"
#[component]
fn App() -> Element {
    let cards = use_signal(|| Vec::<String>::new());
    let submit = use_callback(move |_| { let _ = cards.read(); });
    let tick = use_future(move || async move { let _ = cards.read(); });
    rsx!{}
}
"#,
        );
        let by_name = |n: &str| nodes.iter().find(|x| x.name == n).cloned().unwrap();
        assert_eq!(by_name("cards").kind, "signal");
        assert_eq!(by_name("submit").kind, "callback");
        assert_eq!(by_name("tick").kind, "future");
    }

    #[test]
    fn reads_descend_into_closures_and_async_blocks() {
        let nodes = analyze(
            r#"
#[component]
fn App() -> Element {
    let cards = use_signal(|| Vec::<String>::new());
    let title = use_signal(|| String::new());
    let submit = use_callback(move |_| {
        let _ = cards.read();
        let _ = title.read();
    });
    let action = use_future(move || async move {
        let _ = cards.read();
    });
    rsx!{}
}
"#,
        );
        let by_name = |n: &str| nodes.iter().find(|x| x.name == n).cloned().unwrap();
        let submit_reads = by_name("submit").reads;
        assert!(
            submit_reads.contains(&"cards".to_string()),
            "submit should read `cards` from inside the closure body; got {submit_reads:?}"
        );
        assert!(
            submit_reads.contains(&"title".to_string()),
            "submit should read `title` from inside the closure body; got {submit_reads:?}"
        );
        let action_reads = by_name("action").reads;
        assert!(
            action_reads.contains(&"cards".to_string()),
            "action should read `cards` through the async move block; got {action_reads:?}"
        );
    }

    #[test]
    fn read_by_inverts_the_reads_graph() {
        let nodes = analyze(
            r#"
#[component]
fn App() -> Element {
    let cards = use_signal(|| Vec::<String>::new());
    let submit = use_callback(move |_| { let _ = cards.read(); });
    let action = use_future(move || async move { let _ = cards.read(); });
    rsx!{}
}
"#,
        );
        let cards = nodes.iter().find(|n| n.name == "cards").unwrap();
        assert!(
            cards.read_by.iter().any(|n| n == "submit"),
            "cards should be read_by `submit`; got {:?}",
            cards.read_by,
        );
        assert!(
            cards.read_by.iter().any(|n| n == "action"),
            "cards should be read_by `action`; got {:?}",
            cards.read_by,
        );
    }

    /// Real-world fullstack apps (e.g. `dioxus_standup`'s `BoardBody`) use
    /// `use_future(move || async move { ... })` as a *statement expression*
    /// — no `let` binding. Before the fix, those polling loops were skipped
    /// entirely and the signals they read showed up with `read_by: []`.
    /// Synthesizing an anonymous `<use_future@LINE>` node lets the inverse
    /// projection see those reads.
    #[test]
    fn standalone_use_future_contributes_to_read_by() {
        let nodes = analyze(
            r#"
#[component]
fn BoardBody() -> Element {
    let mut cards = use_signal(|| Vec::<String>::new());
    let mut local_lock = use_signal(|| 0u32);
    use_future(move || async move {
        loop {
            let lock = local_lock();
            cards.set(Vec::new());
            let _ = lock;
        }
    });
    rsx!{}
}
"#,
        );
        let local_lock = nodes
            .iter()
            .find(|n| n.name == "local_lock")
            .expect("local_lock node present");
        assert!(
            local_lock
                .read_by
                .iter()
                .any(|n| n.starts_with("<use_future@")),
            "local_lock should be read_by a synthetic use_future node; got {:?}",
            local_lock.read_by,
        );
        let synthetic = nodes
            .iter()
            .find(|n| n.name.starts_with("<use_future@"))
            .expect("synthetic use_future node should be added for standalone calls");
        assert_eq!(synthetic.kind, "future");
        assert!(
            synthetic.reads.contains(&"local_lock".to_string()),
            "synthetic future node should record reading `local_lock`; got {:?}",
            synthetic.reads,
        );
    }

    /// dioxus_standup's `BoardBody` is the canonical "writes mis-labeled as
    /// reads" case from the TODO: a polling `use_future` that *reads*
    /// `local_lock()` (drives nothing — the lock just gates application of
    /// stale results) and *writes* `cards.set(...)` / `status.set(...)`.
    /// Before the split, all three landed in `reads`, making the
    /// "what re-triggers this loop?" answer indistinguishable from "what
    /// does it touch?". After: only `local_lock` is a read.
    #[test]
    fn writes_split_away_from_reads_for_set_calls() {
        let nodes = analyze(
            r#"
#[component]
fn BoardBody() -> Element {
    let mut cards = use_signal(|| Vec::<String>::new());
    let mut status = use_signal(|| String::new());
    let mut local_lock = use_signal(|| 0u32);
    use_future(move || async move {
        loop {
            let lock = local_lock();
            cards.set(Vec::new());
            status.set(String::from("loaded"));
            let _ = lock;
        }
    });
    rsx!{}
}
"#,
        );
        let fut = nodes
            .iter()
            .find(|n| n.name.starts_with("<use_future@"))
            .expect("synthetic future node present");
        assert_eq!(
            fut.reads,
            vec!["local_lock".to_string()],
            "only local_lock is genuinely read; cards/status are writes"
        );
        let mut writes_sorted = fut.writes.clone();
        writes_sorted.sort();
        assert_eq!(
            writes_sorted,
            vec!["cards".to_string(), "status".to_string()],
            "cards.set(...) and status.set(...) belong in writes"
        );

        // read_by should follow reads only — cards/status get no read_by
        // entry from this future, but local_lock does.
        let cards = nodes.iter().find(|n| n.name == "cards").unwrap();
        assert!(
            cards.read_by.is_empty(),
            "cards is only written by the future, not read — read_by must stay empty: {:?}",
            cards.read_by,
        );
        let local_lock = nodes.iter().find(|n| n.name == "local_lock").unwrap();
        assert!(
            local_lock
                .read_by
                .iter()
                .any(|n| n.starts_with("<use_future@")),
            "local_lock is read by the future; read_by should reflect that: {:?}",
            local_lock.read_by,
        );
    }

    /// `sig.set(sig() + 1)` reads AND writes — both lists should contain
    /// the binding. Guards against the dedupe heuristic accidentally
    /// suppressing one or the other.
    #[test]
    fn read_and_write_in_same_call_appears_in_both_lists() {
        let nodes = analyze(
            r#"
#[component]
fn Demo() -> Element {
    let mut count = use_signal(|| 0u32);
    let inc = use_callback(move |_| { count.set(count() + 1); });
    rsx!{}
}
"#,
        );
        let inc = nodes.iter().find(|n| n.name == "inc").unwrap();
        assert!(
            inc.reads.contains(&"count".to_string()),
            "inc reads count via `count()`; reads = {:?}",
            inc.reads
        );
        assert!(
            inc.writes.contains(&"count".to_string()),
            "inc writes count via `count.set(...)`; writes = {:?}",
            inc.writes
        );
    }

    /// dioxus_standup's presence flow binds `let presence = use_presence();`
    /// where `use_presence` returns a `Signal<Vec<String>>` from
    /// `use_context`. The graph walker can't follow into the helper, so
    /// before this fix the heartbeat `use_future` showed `reads: []` and
    /// gave the reader no hint the future was reactive at all. Now the
    /// future's `context_signals` lists `presence`, surfacing the hidden
    /// reactivity even when the type isn't resolvable from this file.
    #[test]
    fn context_use_helpers_surface_under_context_signals() {
        let nodes = analyze(
            r#"
#[component]
fn Roster() -> Element {
    let presence = use_presence();
    let nav = use_navigator();
    use_future(move || async move {
        let _ = presence();
        nav.push("/");
    });
    rsx!{}
}
"#,
        );
        let fut = nodes
            .iter()
            .find(|n| n.name.starts_with("<use_future@"))
            .expect("synthetic future node present for standalone use_future");
        let mut ctx_sorted = fut.context_signals.clone();
        ctx_sorted.sort();
        assert_eq!(
            ctx_sorted,
            vec!["nav".to_string(), "presence".to_string()],
            "both unresolved use_* bindings should surface as context_signals; \
             reader can disambiguate (nav is a Navigator, presence is the Signal). \
             got = {:?}",
            fut.context_signals,
        );
        // And neither should be promoted to a node — they're not local
        // use_signal bindings, just context handles.
        assert!(
            !nodes
                .iter()
                .any(|n| n.name == "presence" || n.name == "nav"),
            "context bindings stay out of `nodes`; they only appear in `context_signals`"
        );
    }

    /// When a `use_*` helper binding is shadowed or unused, it still appears
    /// in `context_bindings` but no node will report it under
    /// `context_signals`. This guards the per-node filter — we only want to
    /// surface helpers the node actually touches.
    #[test]
    fn unused_context_binding_does_not_pollute_signal_nodes() {
        let nodes = analyze(
            r#"
#[component]
fn Demo() -> Element {
    let _nav = use_navigator();
    let mut count = use_signal(|| 0u32);
    use_effect(move || { count.set(1); });
    rsx!{}
}
"#,
        );
        for n in &nodes {
            assert!(
                n.context_signals.is_empty() || n.context_signals.iter().all(|s| s != "_nav"),
                "no node touches `_nav`; it should not appear in any context_signals: {:?}",
                n,
            );
        }
    }

    #[test]
    fn leaf_signals_keep_empty_reads_but_get_read_by() {
        let nodes = analyze(
            r#"
#[component]
fn App() -> Element {
    let cards = use_signal(|| Vec::<String>::new());
    let count = use_memo(move || cards.read().len());
    rsx!{}
}
"#,
        );
        let cards = nodes.iter().find(|n| n.name == "cards").unwrap();
        assert!(cards.reads.is_empty(), "leaf signals have no reads");
        assert_eq!(cards.read_by, vec!["count".to_string()]);
    }
}
