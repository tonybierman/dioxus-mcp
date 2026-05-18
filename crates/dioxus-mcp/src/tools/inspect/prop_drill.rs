use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use proc_macro2::TokenTree;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::visit::Visit;

use crate::state::State;
use crate::tools::ast::{ParseError, collect_parse_errors, walk_rs_files};
use crate::tools::scaffold::crate_root;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PropDrillParams {
    pub project_root: Option<String>,
    /// When true, drop `callback_passthrough` findings. Callback drills are
    /// the correct pattern when there's no shared context provider, so they
    /// usually drown out the signal from state passthroughs.
    #[serde(default)]
    pub ignore_callbacks: bool,
    /// Optional kind filter — e.g. `["state_passthrough"]` to see only state
    /// drills. Applied after `ignore_callbacks`. Empty = no filter.
    #[serde(default)]
    pub kinds: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct Passthrough {
    pub parent_prop: String,
    pub child: String,
    pub child_prop: String,
    pub via: &'static str,
    pub line: usize,
    /// Classification: `callback_passthrough` when the prop's type looks like
    /// a callback (Callback<…>, EventHandler<…>, Fn/FnMut/FnOnce, or prop
    /// name starts with `on_`); otherwise `state_passthrough`. Callback
    /// drills are usually a correct pattern; state drills are the real
    /// signal a context provider is missing.
    pub kind: &'static str,
    /// `info` | `warning`. A single-level Signal<T> handed to a single child
    /// is the correct shape for shared ephemeral state (drag selection, focus
    /// ring, etc.) — downgrade those to `info` so the warning list stays the
    /// real "this should be a context" signal. `warning` is reserved for
    /// state passthroughs that fan out to multiple distinct children in this
    /// parent (any pattern that probably wants `use_context`). Callback
    /// passthroughs are always `info` — they're a correct pattern.
    pub severity: &'static str,
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

    let index = crate::tools::inspect::project_index::project_index(
        state,
        crate::tools::inspect::project_index::ProjectIndexParams {
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
        /// Per-prop type strings, used to classify findings as
        /// callback_passthrough vs state_passthrough.
        prop_types: HashMap<String, String>,
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
                    prop_types: c
                        .props
                        .iter()
                        .map(|p| (p.name.clone(), p.ty.clone()))
                        .collect(),
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
            let prop_types = info.prop_types.clone();
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
                    &prop_types,
                    &mut passthroughs,
                );
            }

            assign_severity(&mut passthroughs, &prop_types);

            // Apply kind filters.
            if p.ignore_callbacks {
                passthroughs.retain(|pt| pt.kind != "callback_passthrough");
            }
            if let Some(kinds) = &p.kinds
                && !kinds.is_empty()
            {
                let allowed: HashSet<&str> = kinds.iter().map(|s| s.as_str()).collect();
                passthroughs.retain(|pt| allowed.contains(pt.kind));
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
    parent_prop_types: &HashMap<String, String>,
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
                analyze_invocation(
                    &name,
                    &inner,
                    parent_props,
                    parent_arg,
                    parent_prop_types,
                    out,
                );
            }
        }
        i += 1;
    }
    // Recurse into groups.
    for tt in tokens {
        if let TokenTree::Group(g) = tt {
            let inner: Vec<TokenTree> = g.stream().into_iter().collect();
            find_invocations(
                &inner,
                known,
                parent_props,
                parent_arg,
                parent_prop_types,
                out,
            );
        }
    }
}

fn analyze_invocation(
    child: &str,
    tokens: &[TokenTree],
    parent_props: &HashSet<String>,
    parent_arg: Option<&str>,
    parent_prop_types: &HashMap<String, String>,
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
            let kind = classify_prop(&parent_prop, parent_prop_types);
            out.push(Passthrough {
                parent_prop,
                child: child.to_string(),
                child_prop: key_s,
                via,
                line,
                kind,
                // Severity is assigned after the parent's full passthrough
                // set is collected (fan-out depends on cross-finding state).
                severity: "warning",
            });
        }
    }
}

/// Assign per-finding severity based on type + fan-out within the parent.
/// Callbacks are always `info` — they're a correct pattern. State
/// passthroughs that target a single child AND whose type is a `Signal<T>`
/// are also `info` (the shared-ephemeral-state shape, e.g. drag selection,
/// focus ring). Everything else is `warning` — that's the "this probably
/// wants `use_context`" signal.
fn assign_severity(passthroughs: &mut [Passthrough], types: &HashMap<String, String>) {
    // Fan-out: how many DISTINCT children receive each parent_prop in this
    // parent's rsx body. Drills that fan out to multiple children are the
    // strongest signal that the prop wants a context provider.
    let mut fanout: HashMap<&str, HashSet<String>> = HashMap::new();
    for pt in passthroughs.iter() {
        fanout
            .entry(pt.parent_prop.as_str())
            .or_default()
            .insert(pt.child.clone());
    }
    let fanout: HashMap<String, usize> = fanout
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.len()))
        .collect();

    for pt in passthroughs.iter_mut() {
        // Callbacks are a correct pattern even when drilled.
        if pt.kind == "callback_passthrough" {
            pt.severity = "info";
            continue;
        }
        let single_child = fanout.get(&pt.parent_prop).copied().unwrap_or(1) <= 1;
        let is_signal = types
            .get(&pt.parent_prop)
            .map(|t| t.replace(' ', ""))
            .map(|t| {
                t.contains("Signal<") || t.contains("ReadSignal<") || t.contains("WriteSignal<")
            })
            .unwrap_or(false);
        pt.severity = if single_child && is_signal {
            "info"
        } else {
            "warning"
        };
    }
}

/// Classify a parent prop as a callback or state passthrough based on its
/// type signature (or `on_*` name as a fallback).
fn classify_prop(name: &str, types: &HashMap<String, String>) -> &'static str {
    if let Some(ty) = types.get(name) {
        let stripped = ty.replace(' ', "");
        if stripped.contains("Callback<")
            || stripped.contains("EventHandler<")
            || stripped.contains("Fn(")
            || stripped.contains("FnMut(")
            || stripped.contains("FnOnce(")
            || stripped.contains("dynFn")
            || stripped.contains("dynFnMut")
            || stripped.contains("dynFnOnce")
        {
            return "callback_passthrough";
        }
    }
    if name.starts_with("on_") {
        return "callback_passthrough";
    }
    "state_passthrough"
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

#[cfg(test)]
mod tests {
    use super::*;

    fn passthrough(parent_prop: &str, child: &str, kind: &'static str) -> Passthrough {
        Passthrough {
            parent_prop: parent_prop.to_string(),
            child: child.to_string(),
            child_prop: parent_prop.to_string(),
            via: "shorthand",
            line: 1,
            kind,
            severity: "warning",
        }
    }

    #[test]
    fn single_child_signal_passthrough_is_info() {
        let types: HashMap<String, String> =
            [("dragging".to_string(), "Signal<Option<i64>>".to_string())].into();
        let mut pts = vec![passthrough("dragging", "CardItem", "state_passthrough")];
        assign_severity(&mut pts, &types);
        assert_eq!(pts[0].severity, "info");
    }

    #[test]
    fn signal_passthrough_to_multiple_children_is_warning() {
        let types: HashMap<String, String> =
            [("dragging".to_string(), "Signal<Option<i64>>".to_string())].into();
        let mut pts = vec![
            passthrough("dragging", "CardItem", "state_passthrough"),
            passthrough("dragging", "ColumnHeader", "state_passthrough"),
        ];
        assign_severity(&mut pts, &types);
        for pt in &pts {
            assert_eq!(
                pt.severity, "warning",
                "fan-out to multiple children must escalate severity"
            );
        }
    }

    #[test]
    fn non_signal_state_passthrough_is_warning_even_with_one_child() {
        // A plain `Vec<Card>` drilled one level is the classic
        // "this wants a context" finding — keep at warning.
        let types: HashMap<String, String> =
            [("cards".to_string(), "Vec<Card>".to_string())].into();
        let mut pts = vec![passthrough("cards", "Column", "state_passthrough")];
        assign_severity(&mut pts, &types);
        assert_eq!(pts[0].severity, "warning");
    }

    #[test]
    fn callback_passthroughs_are_always_info() {
        // Callbacks drilled across multiple children are still the correct
        // pattern.
        let types: HashMap<String, String> =
            [("on_move".to_string(), "Callback<MoveEvent>".to_string())].into();
        let mut pts = vec![
            passthrough("on_move", "Column", "callback_passthrough"),
            passthrough("on_move", "CardItem", "callback_passthrough"),
        ];
        assign_severity(&mut pts, &types);
        for pt in &pts {
            assert_eq!(pt.severity, "info");
        }
    }

    #[test]
    fn read_signal_and_write_signal_count_as_signal() {
        for ty in [
            "ReadSignal<Option<i64>>",
            "WriteSignal<Vec<Card>>",
            "Signal<bool>",
        ] {
            let types: HashMap<String, String> = [("x".to_string(), ty.to_string())].into();
            let mut pts = vec![passthrough("x", "Child", "state_passthrough")];
            assign_severity(&mut pts, &types);
            assert_eq!(
                pts[0].severity, "info",
                "Signal-family type `{ty}` to one child should be info"
            );
        }
    }
}
