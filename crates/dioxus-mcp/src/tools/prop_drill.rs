use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use proc_macro2::TokenTree;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::visit::Visit;

use crate::state::State;
use crate::tools::scaffold::crate_root;
use crate::tools::scan::{ParseError, collect_parse_errors, walk_rs_files};

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PropDrillParams {
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Passthrough {
    pub parent_prop: String,
    pub child: String,
    pub child_prop: String,
    pub via: &'static str,
    pub line: usize,
}

#[derive(Debug, Serialize)]
pub struct ParentEntry {
    pub component: String,
    pub file: PathBuf,
    pub passthroughs: Vec<Passthrough>,
}

#[derive(Debug, Serialize)]
pub struct PropDrillReport {
    pub parents: Vec<ParentEntry>,
    pub known_gaps: Vec<&'static str>,
    pub parse_errors: Vec<ParseError>,
}

pub async fn prop_drill(state: &Arc<State>, p: PropDrillParams) -> Result<PropDrillReport, String> {
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

    struct ParentInfo {
        props: HashSet<String>,
        /// For Props-struct components, the local var bound to the props (e.g. "props").
        props_arg: Option<String>,
    }

    let parent_info: HashMap<String, ParentInfo> = index
        .components
        .iter()
        .map(|c| {
            (
                c.name.clone(),
                ParentInfo {
                    props: c.props.iter().map(|p| p.name.clone()).collect(),
                    props_arg: None, // filled in below when we have the fn AST
                },
            )
        })
        .collect();
    let mut parent_info = parent_info;
    let via_props_struct: HashMap<String, bool> = index
        .components
        .iter()
        .map(|c| (c.name.clone(), c.via_props_struct))
        .collect();
    let known_components: HashSet<String> =
        index.components.iter().map(|c| c.name.clone()).collect();

    let mut parents: Vec<ParentEntry> = Vec::new();
    let files = walk_rs_files(&src_root);

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
            let name = f.sig.ident.to_string();
            let Some(info) = parent_info.get(&name) else {
                continue;
            };
            let props = info.props.clone();
            let props_arg = if via_props_struct.get(&name).copied().unwrap_or(false) {
                fn_first_arg_name(f)
            } else {
                None
            };
            // Persist back for any downstream use.
            if let Some(slot) = parent_info.get_mut(&name) {
                slot.props_arg = props_arg.clone();
            }

            let mut collector = RsxCollector {
                rsx_bodies: Vec::new(),
            };
            collector.visit_block(&f.block);

            let mut passthroughs: Vec<Passthrough> = Vec::new();
            for body in &collector.rsx_bodies {
                let tokens: Vec<TokenTree> = body.clone().into_iter().collect();
                find_invocations(
                    &tokens,
                    &known_components,
                    &props,
                    props_arg.as_deref(),
                    &mut passthroughs,
                );
            }

            if !passthroughs.is_empty() {
                parents.push(ParentEntry {
                    component: name,
                    file: sf.path.clone(),
                    passthroughs,
                });
            }
        }
    }

    parents.sort_by(|a, b| a.component.cmp(&b.component));

    Ok(PropDrillReport {
        parents,
        known_gaps: vec![
            "rsx! `..props` spread syntax is not detected",
            "method chains deeper than one call (e.g. `prop.clone().to_string()`) are not detected",
        ],
        parse_errors: collect_parse_errors(&files),
    })
}

struct RsxCollector {
    rsx_bodies: Vec<proc_macro2::TokenStream>,
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
            self.rsx_bodies.push(m.tokens.clone());
        }
        syn::visit::visit_macro(self, m);
    }
}

fn fn_first_arg_name(f: &syn::ItemFn) -> Option<String> {
    let arg = f.sig.inputs.first()?;
    let syn::FnArg::Typed(pt) = arg else {
        return None;
    };
    let syn::Pat::Ident(pi) = &*pt.pat else {
        return None;
    };
    Some(pi.ident.to_string())
}

fn find_invocations(
    tokens: &[TokenTree],
    known: &HashSet<String>,
    parent_props: &HashSet<String>,
    parent_arg: Option<&str>,
    out: &mut Vec<Passthrough>,
) {
    let mut i = 0;
    while i < tokens.len() {
        if let TokenTree::Ident(id) = &tokens[i] {
            let name = id.to_string();
            if known.contains(&name)
                && let Some(TokenTree::Group(g)) = tokens.get(i + 1)
                && g.delimiter() == proc_macro2::Delimiter::Brace
            {
                let inner: Vec<TokenTree> = g.stream().into_iter().collect();
                analyze_invocation(&name, &inner, parent_props, parent_arg, out);
            }
        }
        i += 1;
    }
    // Recurse into groups.
    for tt in tokens {
        if let TokenTree::Group(g) = tt {
            let inner: Vec<TokenTree> = g.stream().into_iter().collect();
            find_invocations(&inner, known, parent_props, parent_arg, out);
        }
    }
}

fn analyze_invocation(
    child: &str,
    tokens: &[TokenTree],
    parent_props: &HashSet<String>,
    parent_arg: Option<&str>,
    out: &mut Vec<Passthrough>,
) {
    for field in split_top_level_commas(tokens) {
        if field.is_empty() {
            continue;
        }
        // Skip attribute-style fields (e.g. `class: "..."` is fine; we only care about
        // shorthand `prop` and `key: value` forms).
        let TokenTree::Ident(key) = &field[0] else {
            continue;
        };
        let key_s = key.to_string();
        let line = key.span().start().line;
        let value_tokens: Vec<TokenTree> = if field.len() == 1 {
            // shorthand: child_prop == parent identifier
            vec![field[0].clone()]
        } else if let TokenTree::Punct(p) = &field[1] {
            if p.as_char() == ':' {
                field[2..].to_vec()
            } else {
                continue;
            }
        } else {
            continue;
        };

        if let Some((parent_prop, via)) = match_passthrough(&value_tokens, parent_props, parent_arg)
        {
            out.push(Passthrough {
                parent_prop,
                child: child.to_string(),
                child_prop: key_s,
                via,
                line,
            });
        }
    }
}

fn split_top_level_commas(tokens: &[TokenTree]) -> Vec<Vec<TokenTree>> {
    let mut parts: Vec<Vec<TokenTree>> = Vec::new();
    let mut current: Vec<TokenTree> = Vec::new();
    for tt in tokens {
        if let TokenTree::Punct(p) = tt
            && p.as_char() == ','
        {
            if !current.is_empty() {
                parts.push(std::mem::take(&mut current));
            }
            continue;
        }
        current.push(tt.clone());
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

fn match_passthrough(
    tokens: &[TokenTree],
    parent_props: &HashSet<String>,
    parent_arg: Option<&str>,
) -> Option<(String, &'static str)> {
    if tokens.is_empty() {
        return None;
    }
    let (base, via) = strip_method_suffix(tokens);
    let prop = match_base(base, parent_props, parent_arg)?;
    Some((prop, via))
}

fn strip_method_suffix(tokens: &[TokenTree]) -> (&[TokenTree], &'static str) {
    if tokens.len() < 4 {
        return (tokens, "direct");
    }
    let n = tokens.len();
    let (TokenTree::Punct(dot), TokenTree::Ident(method), TokenTree::Group(args)) =
        (&tokens[n - 3], &tokens[n - 2], &tokens[n - 1])
    else {
        return (tokens, "direct");
    };
    if dot.as_char() != '.'
        || args.delimiter() != proc_macro2::Delimiter::Parenthesis
        || !args.stream().is_empty()
    {
        return (tokens, "direct");
    }
    let via: &'static str = match method.to_string().as_str() {
        "clone" => "clone",
        "into" => "into",
        "to_owned" => "to_owned",
        "read" => "signal_read",
        "peek" => "signal_peek",
        "cloned" => "signal_cloned",
        _ => return (tokens, "direct"),
    };
    (&tokens[..n - 3], via)
}

fn match_base(
    tokens: &[TokenTree],
    parent_props: &HashSet<String>,
    parent_arg: Option<&str>,
) -> Option<String> {
    if tokens.len() == 1 {
        if let TokenTree::Ident(i) = &tokens[0] {
            let s = i.to_string();
            if parent_props.contains(&s) {
                return Some(s);
            }
        }
        return None;
    }
    if tokens.len() == 3
        && let (TokenTree::Ident(a), TokenTree::Punct(dot), TokenTree::Ident(b)) =
            (&tokens[0], &tokens[1], &tokens[2])
        && dot.as_char() == '.'
        && parent_arg == Some(&a.to_string())
    {
        let prop = b.to_string();
        if parent_props.contains(&prop) {
            return Some(prop);
        }
    }
    None
}
