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
    pub kind: String, // "signal" | "memo" | "resource" | "effect"
    pub line: usize,
    pub reads: Vec<String>,
    /// True when this signal is referenced anywhere in the component's `rsx!`
    /// invocations — either as a formatted-string interpolation (`{sig}`) or
    /// as a bare ident / call (`sig`, `sig()`, `sig.read()`). A leaf
    /// `use_signal` will have `reads: []` but still be consumed by rsx; this
    /// flag distinguishes "consumed by rendering" from "truly unused".
    pub read_in_rsx: bool,
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

    for stmt in &block.stmts {
        let syn::Stmt::Local(local) = stmt else {
            continue;
        };
        let Some(init) = &local.init else { continue };
        let kind = classify_init_call(&init.expr);
        let Some(kind) = kind else { continue };

        let binding_name = match &local.pat {
            syn::Pat::Ident(p) => p.ident.to_string(),
            syn::Pat::Type(t) => match &*t.pat {
                syn::Pat::Ident(p) => p.ident.to_string(),
                _ => "<unnamed>".into(),
            },
            _ => "<unnamed>".into(),
        };

        let line = local.let_token.span.start().line;
        let reads = collect_reads(&init.expr, &known_bindings);
        nodes.push(SignalNode {
            name: binding_name.clone(),
            kind: kind.into(),
            line,
            reads,
            read_in_rsx: false,
        });
        known_bindings.push(binding_name);
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
        _ => None,
    }
}

fn collect_reads(expr: &syn::Expr, known: &[String]) -> Vec<String> {
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
