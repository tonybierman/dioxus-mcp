use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use proc_macro2::TokenTree;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::visit::Visit;

use crate::state::State;
use crate::tools::scaffold::crate_root;
use crate::tools::scan::{collect_parse_errors, walk_rs_files, ParseError};

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct DeadComponentsParams {
    /// Additional component names to treat as alive. `App` and all components reachable
    /// from a Routable enum are always treated as roots.
    #[serde(default)]
    pub roots: Option<Vec<String>>,
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DeadComponent {
    pub name: String,
    pub file: PathBuf,
    pub line: usize,
}

#[derive(Debug, Serialize)]
pub struct DeadComponentsReport {
    pub dead: Vec<DeadComponent>,
    pub roots: Vec<String>,
    pub total_components: usize,
    pub parse_errors: Vec<ParseError>,
}

pub async fn dead_components(
    state: &Arc<State>,
    p: DeadComponentsParams,
) -> Result<DeadComponentsReport, String> {
    let crate_root = crate_root(state, p.project_root.as_deref()).await?;
    let src_root = crate_root.join("src");

    let index = crate::tools::project_index::project_index(
        state,
        crate::tools::project_index::ProjectIndexParams {
            path: None,
            kind: Some("component".into()),
            project_root: p.project_root.clone(),
        },
    )
    .await?;

    let route_roots: HashSet<String> = match crate::tools::route_map::route_map(
        state,
        crate::tools::route_map::RouteMapParams {
            router_file: None,
            project_root: p.project_root.clone(),
        },
    )
    .await
    {
        Ok(rm) => {
            let mut s: HashSet<String> = rm
                .routes
                .iter()
                .map(|r| r.component.clone())
                .collect();
            for r in &rm.routes {
                for l in &r.layouts {
                    s.insert(l.clone());
                }
            }
            s
        }
        Err(_) => HashSet::new(),
    };

    let mut roots: HashSet<String> = HashSet::new();
    roots.insert("App".to_string());
    roots.extend(route_roots);
    if let Some(extra) = p.roots {
        roots.extend(extra);
    }

    let component_names: HashSet<String> = index.components.iter().map(|c| c.name.clone()).collect();

    // Walk src, count invocations of each known component inside rsx! blocks.
    let mut used: HashSet<String> = HashSet::new();
    let files = walk_rs_files(&src_root);
    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
        let mut v = RsxComponentVisitor {
            known: &component_names,
            used: &mut used,
        };
        v.visit_file(ast);
    }

    let mut roots_vec: Vec<String> = roots.iter().cloned().collect();
    roots_vec.sort();

    let total_components = index.components.len();
    let mut dead: Vec<DeadComponent> = index
        .components
        .into_iter()
        .filter(|c| !used.contains(&c.name) && !roots.contains(&c.name))
        .map(|c| DeadComponent {
            name: c.name,
            file: c.file,
            line: c.line,
        })
        .collect();
    dead.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(DeadComponentsReport {
        dead,
        roots: roots_vec,
        total_components,
        parse_errors: collect_parse_errors(&files),
    })
}

struct RsxComponentVisitor<'a> {
    known: &'a HashSet<String>,
    used: &'a mut HashSet<String>,
}

impl<'a, 'ast> Visit<'ast> for RsxComponentVisitor<'a> {
    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        let is_rsx = m
            .path
            .segments
            .last()
            .map(|s| s.ident == "rsx")
            .unwrap_or(false);
        if is_rsx {
            let tokens: Vec<TokenTree> = m.tokens.clone().into_iter().collect();
            scan_for_components(&tokens, self.known, self.used);
        }
        syn::visit::visit_macro(self, m);
    }
}

fn scan_for_components(
    tokens: &[TokenTree],
    known: &HashSet<String>,
    used: &mut HashSet<String>,
) {
    let mut i = 0;
    while i < tokens.len() {
        if let TokenTree::Ident(id) = &tokens[i] {
            let name = id.to_string();
            // Component invocation: `Ident {` (or `path::Ident {`).
            if known.contains(&name) {
                if let Some(TokenTree::Group(g)) = tokens.get(i + 1) {
                    if g.delimiter() == proc_macro2::Delimiter::Brace {
                        used.insert(name);
                    }
                }
            }
        }
        i += 1;
    }
    // Recurse into groups for nested rsx! children.
    for tt in tokens {
        if let TokenTree::Group(g) = tt {
            let inner: Vec<TokenTree> = g.stream().into_iter().collect();
            scan_for_components(&inner, known, used);
        }
    }
}
