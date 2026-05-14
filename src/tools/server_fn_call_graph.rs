use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::visit::Visit;

use crate::state::State;
use crate::tools::scaffold::crate_root;
use crate::tools::scan::{collect_parse_errors, walk_rs_files, ParseError};

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ServerFnCallGraphParams {
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CallEdge {
    pub server_fn: String,
    pub caller_file: PathBuf,
    pub caller_line: usize,
    pub enclosing_fn: Option<String>,
    pub full_path: String,
}

#[derive(Debug, Serialize)]
pub struct OrphanServerFn {
    pub name: String,
    pub file: PathBuf,
    pub line: usize,
}

#[derive(Debug, Serialize)]
pub struct ServerFnCallGraphReport {
    pub edges: Vec<CallEdge>,
    pub orphans: Vec<OrphanServerFn>,
    pub notes: Vec<&'static str>,
    pub parse_errors: Vec<ParseError>,
}

pub async fn server_fn_call_graph(
    state: &Arc<State>,
    p: ServerFnCallGraphParams,
) -> Result<ServerFnCallGraphReport, String> {
    let crate_root = crate_root(state, p.project_root.as_deref()).await?;
    let src_root = crate_root.join("src");

    let index = crate::tools::project_index::project_index(
        state,
        crate::tools::project_index::ProjectIndexParams {
            path: None,
            kind: Some("server_fn".into()),
            project_root: p.project_root.clone(),
        },
    )
    .await?;

    let server_fn_names: HashSet<String> =
        index.server_fns.iter().map(|s| s.name.clone()).collect();
    let definitions: HashMap<String, (PathBuf, usize)> = index
        .server_fns
        .iter()
        .map(|s| (s.name.clone(), (s.file.clone(), s.line)))
        .collect();

    let mut edges: Vec<CallEdge> = Vec::new();
    let mut callees_seen: HashSet<String> = HashSet::new();

    let files = walk_rs_files(&src_root);
    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
        let mut v = CallVisitor {
            stack: Vec::new(),
            file: &sf.path,
            known: &server_fn_names,
            edges: &mut edges,
            seen: &mut callees_seen,
        };
        v.visit_file(ast);
    }

    let mut orphans: Vec<OrphanServerFn> = server_fn_names
        .iter()
        .filter(|n| !callees_seen.contains(*n))
        .filter_map(|n| definitions.get(n).map(|(f, l)| OrphanServerFn {
            name: n.clone(),
            file: f.clone(),
            line: *l,
        }))
        .collect();
    orphans.sort_by(|a, b| a.name.cmp(&b.name));

    edges.sort_by(|a, b| {
        a.server_fn
            .cmp(&b.server_fn)
            .then_with(|| a.caller_file.cmp(&b.caller_file))
            .then_with(|| a.caller_line.cmp(&b.caller_line))
    });

    Ok(ServerFnCallGraphReport {
        edges,
        orphans,
        notes: vec![
            "callee resolution matches the path's last segment; locally shadowed names may produce false positives",
            "cross-crate callers are not detected",
        ],
        parse_errors: collect_parse_errors(&files),
    })
}

struct CallVisitor<'a> {
    stack: Vec<String>,
    file: &'a std::path::Path,
    known: &'a HashSet<String>,
    edges: &'a mut Vec<CallEdge>,
    seen: &'a mut HashSet<String>,
}

impl<'a, 'ast> Visit<'ast> for CallVisitor<'a> {
    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        self.stack.push(f.sig.ident.to_string());
        syn::visit::visit_item_fn(self, f);
        self.stack.pop();
    }
    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        self.stack.push(f.sig.ident.to_string());
        syn::visit::visit_impl_item_fn(self, f);
        self.stack.pop();
    }
    fn visit_expr_closure(&mut self, e: &'ast syn::ExprClosure) {
        self.stack.push("<closure>".into());
        syn::visit::visit_expr_closure(self, e);
        self.stack.pop();
    }
    fn visit_expr_call(&mut self, e: &'ast syn::ExprCall) {
        if let syn::Expr::Path(p) = &*e.func {
            if let Some(seg) = p.path.segments.last() {
                let name = seg.ident.to_string();
                if self.known.contains(&name) {
                    let line = seg.ident.span().start().line;
                    let full_path = render_path(&p.path);
                    let enclosing = enclosing_fn_name(&self.stack);
                    self.seen.insert(name.clone());
                    self.edges.push(CallEdge {
                        server_fn: name,
                        caller_file: self.file.to_path_buf(),
                        caller_line: line,
                        enclosing_fn: enclosing,
                        full_path,
                    });
                }
            }
        }
        syn::visit::visit_expr_call(self, e);
    }
}

fn enclosing_fn_name(stack: &[String]) -> Option<String> {
    for s in stack.iter().rev() {
        if s != "<closure>" {
            return Some(s.clone());
        }
    }
    None
}

fn render_path(p: &syn::Path) -> String {
    let mut out = String::new();
    if p.leading_colon.is_some() {
        out.push_str("::");
    }
    let mut first = true;
    for seg in &p.segments {
        if !first {
            out.push_str("::");
        }
        out.push_str(&seg.ident.to_string());
        first = false;
    }
    out
}
