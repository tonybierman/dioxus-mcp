use std::path::{Path, PathBuf};
use std::sync::Arc;

use proc_macro2::TokenTree;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::visit::Visit;

use crate::state::State;
use crate::tools::scaffold::crate_root;
use crate::tools::scan::{ParseError, collect_parse_errors, walk_rs_files};

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
    pub parse_errors: Vec<ParseError>,
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
        }
    }

    Ok(SignalLintReport {
        issues,
        parse_errors: collect_parse_errors(&files),
    })
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
