//! `signal_drilled_2_levels`: flag a `Signal<T>` prop that is passed
//! unchanged through ≥2 parents — the canonical "this wants
//! `use_context_provider`" shape that `prop_drill` only sees as a
//! single-level passthrough at each hop.
//!
//! Detection: walk `prop_drill`'s state_passthrough edges, keep only those
//! whose parent-side prop type is `Signal<T>` / `ReadSignal<T>` /
//! `WriteSignal<T>`, then look for two-hop chains A → B → C where the
//! same signal flows unchanged. iter03's `dragging: Signal<Option<String>>`
//! (BoardBody → Column → CardItem) is the canonical case.
//!
//! Origin hop: iter03 collapsed the signal-owning component (BoardBody)
//! into the topmost forwarder, so BoardBody has no prop named `dragging`
//! — it creates the signal locally via `use_signal(…)` and hands it down
//! to `Column`. `prop_drill` can't see that hop because there's no parent
//! prop to follow. We add a second scan that walks each component fn,
//! collects `let <name> = use_signal(…)` bindings, and synthesizes edges
//! `(component, name) → (child, child_prop)` whenever the binding is
//! forwarded as a shorthand or `prop: <name>` field. Those synthetic
//! edges plug directly into the chain walk below.
//!
//! Fix suggestion: lift the signal into a context provider at the nearest
//! common ancestor (often the topmost component that creates the signal)
//! and have each consumer call `use_context::<Signal<T>>()` directly.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use proc_macro2::TokenTree;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::visit::Visit;

use crate::state::State;
use crate::tools::ast::{ParseError, collect_parse_errors, walk_rs_files};
use crate::tools::inspect::project_index::{ProjectIndexParams, project_index};
use crate::tools::inspect::prop_drill::{PropDrillParams, prop_drill};
use crate::tools::scaffold::crate_root;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SignalDrilledParams {
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SignalDrilledFinding {
    pub code: &'static str,
    /// Severity is `warning` — a Signal flowing through two unmodified hops
    /// is almost always the missing-context-provider shape. We never
    /// downgrade to `info` here; that's what `prop_drill`'s severity tier
    /// is for (single-hop, single-child drills).
    pub severity: &'static str,
    pub file: PathBuf,
    pub line: usize,
    /// Top of the chain — the component whose render hands the signal off
    /// to the first parent in the chain. This is where the fix would land
    /// (or somewhere above it, depending on where the signal originates).
    pub root_component: String,
    /// Full forwarding chain, root first. Always ≥3 entries:
    /// `[root, middle, leaf]`. The leaf is the actual consumer.
    pub chain: Vec<String>,
    /// Prop name in the root component (the binding that holds the
    /// signal at the top of the chain).
    pub root_prop: String,
    /// Signal type as it appears on `root_component.root_prop`. Surfaced
    /// so the fix snippet can name the right type.
    pub signal_type: String,
    pub message: String,
    /// Concrete code suggestion ready to paste into the root component.
    pub fix_snippet: String,
}

#[derive(Debug, Serialize)]
pub struct SignalDrilledReport {
    pub findings: Vec<SignalDrilledFinding>,
    pub parse_errors: Vec<ParseError>,
}

pub async fn signal_drilled_2_levels(
    state: &Arc<State>,
    p: SignalDrilledParams,
) -> Result<SignalDrilledReport, String> {
    let index = project_index(
        state,
        ProjectIndexParams {
            path: None,
            kind: Some("component".into()),
            project_root: p.project_root.clone(),
        },
    )
    .await?;

    // (component, prop) -> type
    let mut prop_types: HashMap<(String, String), String> = HashMap::new();
    for c in &index.components {
        for prop in &c.props {
            prop_types.insert((c.name.clone(), prop.name.clone()), prop.ty.clone());
        }
    }

    let drills = prop_drill(
        state,
        PropDrillParams {
            project_root: p.project_root.clone(),
            ignore_callbacks: true,
            kinds: Some(vec!["state_passthrough".into()]),
            // signal_drilled_2_levels does its own chain reconstruction
            // (it needs the full graph) — don't apply the prop_drill
            // chain filter or we'd lose the leaf edges it walks back from.
            min_chain_depth: None,
        },
    )
    .await?;

    // Forwarding edges from "what flows in to this parent" to "where it
    // flows out": (parent_component, parent_prop) -> [(child_component,
    // child_prop, file, line)].
    let mut graph: HashMap<(String, String), Vec<Edge>> = HashMap::new();
    for parent in &drills.parents {
        for pt in &parent.passthroughs {
            let key = (parent.component.clone(), pt.parent_prop.clone());
            let Some(ty) = prop_types.get(&key) else {
                continue;
            };
            if !is_signal_type(ty) {
                continue;
            }
            graph.entry(key).or_default().push(Edge {
                child: pt.child.clone(),
                child_prop: pt.child_prop.clone(),
                file: parent.file.clone(),
                line: pt.line,
            });
        }
    }

    // Synthesize "origin" edges: `let <sig> = use_signal(…)` in component A
    // that is then forwarded into a known child's prop is a hop that
    // `prop_drill` can't see because A has no prop named `<sig>`. We
    // attribute the edge's type from the *child* prop's declared type
    // (e.g. `Column.dragging: Signal<Option<String>>`) so the chain
    // walker treats the synthetic edge identically to a real passthrough.
    let known_components: std::collections::HashSet<String> =
        index.components.iter().map(|c| c.name.clone()).collect();
    let origin_edges = collect_origin_edges(
        p.project_root.as_deref(),
        state,
        &known_components,
        &prop_types,
    )
    .await?;
    for (origin_key, edge, edge_type) in origin_edges {
        // Don't replace a real prop_drill edge — if the component already
        // has a prop named `<sig>` it owns the signal-flow story; we'd
        // otherwise double up findings.
        if prop_types.contains_key(&origin_key) {
            continue;
        }
        if !is_signal_type(&edge_type) {
            continue;
        }
        // Mirror the child's prop type onto the synthetic origin key so the
        // chain walker's `prop_types.get(&origin_key)` lookup succeeds and
        // the finding can name the right Signal<T>.
        prop_types
            .entry(origin_key.clone())
            .or_insert(edge_type.clone());
        graph.entry(origin_key).or_default().push(edge);
    }

    let mut findings: Vec<SignalDrilledFinding> = Vec::new();
    // For each "root" forwarding edge (A.X -> B.Y), look for a second hop
    // (B.Y -> C.Z). When found, emit one finding per (A.X, B, C) triple.
    for ((root_comp, root_prop), edges) in &graph {
        let Some(root_ty) = prop_types.get(&(root_comp.clone(), root_prop.clone())) else {
            continue;
        };
        for e1 in edges {
            let next_key = (e1.child.clone(), e1.child_prop.clone());
            let Some(e2s) = graph.get(&next_key) else {
                continue;
            };
            for e2 in e2s {
                let chain = vec![root_comp.clone(), e1.child.clone(), e2.child.clone()];
                let signal_type = root_ty.clone();
                let fix_snippet = build_fix_snippet(root_comp, root_prop, &signal_type, &chain);
                findings.push(SignalDrilledFinding {
                    code: "signal_drilled_2_levels",
                    severity: "warning",
                    file: e1.file.clone(),
                    line: e1.line,
                    root_component: root_comp.clone(),
                    chain,
                    root_prop: root_prop.clone(),
                    signal_type,
                    message: format!(
                        "`{root_prop}: {ty}` is drilled through 2 parents \
                         ({root} → {mid} → {leaf}) without being modified — this is the \
                         missing-`use_context_provider` shape. Lifting the signal into a \
                         context provider near the top of the tree lets each component \
                         pull it directly with `use_context::<{ty}>()` instead of \
                         threading the prop down.",
                        ty = root_ty,
                        root = root_comp,
                        mid = e1.child,
                        leaf = e2.child,
                    ),
                    fix_snippet,
                });
            }
        }
    }

    // De-dup: two synthetic edges from the same origin to the same chain
    // can land here when the signal is forwarded from multiple rsx! blocks
    // (e.g. conditional rendering). Collapse to the lowest-line variant.
    findings.sort_by(|a, b| {
        a.root_component
            .cmp(&b.root_component)
            .then(a.root_prop.cmp(&b.root_prop))
            .then(a.chain.cmp(&b.chain))
            .then(a.line.cmp(&b.line))
    });
    findings.dedup_by(|a, b| {
        a.root_component == b.root_component && a.root_prop == b.root_prop && a.chain == b.chain
    });

    // Stable order: by file, then line, then chain.
    findings.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.line.cmp(&b.line))
            .then(a.chain.cmp(&b.chain))
    });

    Ok(SignalDrilledReport {
        findings,
        parse_errors: drills.parse_errors,
    })
}

/// Scan every component fn in the project and synthesize "origin" edges
/// — a `let <name> = use_signal(…)` binding that's then forwarded into a
/// known child's prop in rsx!. Returns triples of
/// `((origin_component, signal_name), Edge, edge_type)` where `edge_type`
/// is mirrored from the child's declared prop type so the chain walker
/// sees a real Signal<T>.
async fn collect_origin_edges(
    project_root: Option<&str>,
    state: &Arc<State>,
    known: &std::collections::HashSet<String>,
    prop_types: &HashMap<(String, String), String>,
) -> Result<Vec<((String, String), Edge, String)>, String> {
    let root = crate_root(state, project_root).await?;
    let src_root = root.join("src");
    let files = walk_rs_files(&src_root);

    let mut out: Vec<((String, String), Edge, String)> = Vec::new();
    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
        for item in &ast.items {
            let syn::Item::Fn(f) = item else { continue };
            if !is_component_fn(&f.attrs) {
                continue;
            }
            let comp = f.sig.ident.to_string();
            let mut bindings = SignalBindingCollector::default();
            bindings.visit_block(&f.block);

            if bindings.names.is_empty() {
                continue;
            }
            let mut rsx_bodies = RsxCollector::default();
            rsx_bodies.visit_block(&f.block);
            for body in &rsx_bodies.bodies {
                let tokens: Vec<TokenTree> = body.clone().into_iter().collect();
                find_forwarded_signal_origins(
                    &tokens,
                    known,
                    &bindings.names,
                    &comp,
                    &sf.path,
                    prop_types,
                    &mut out,
                );
            }
        }
    }
    Ok(out)
}

/// Forwarding edge in the chain graph: a prop on `parent_component` is
/// handed down to `child` as `child_prop`. Module-level so both the real
/// prop_drill-derived edges and the synthetic `use_signal` origin edges
/// share one type.
#[derive(Clone)]
struct Edge {
    child: String,
    child_prop: String,
    file: PathBuf,
    line: usize,
}

#[derive(Default)]
struct SignalBindingCollector {
    /// Idents bound to a `use_signal(…)` (or `use_signal_sync` / similar)
    /// call result in this component fn's body.
    names: std::collections::HashSet<String>,
}

impl<'ast> Visit<'ast> for SignalBindingCollector {
    fn visit_local(&mut self, local: &'ast syn::Local) {
        if let Some(init) = &local.init
            && let syn::Expr::Call(call) = &*init.expr
            && is_use_signal_path(&call.func)
            && let Some(name) = pat_ident(&local.pat)
        {
            self.names.insert(name);
        }
        syn::visit::visit_local(self, local);
    }
}

fn pat_ident(p: &syn::Pat) -> Option<String> {
    if let syn::Pat::Ident(pi) = p {
        Some(pi.ident.to_string())
    } else {
        None
    }
}

fn is_use_signal_path(e: &syn::Expr) -> bool {
    let syn::Expr::Path(p) = e else { return false };
    let last = p.path.segments.last().map(|s| s.ident.to_string());
    matches!(
        last.as_deref(),
        Some("use_signal" | "use_signal_sync" | "use_resource")
    )
}

#[derive(Default)]
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

fn find_forwarded_signal_origins(
    tokens: &[TokenTree],
    known: &std::collections::HashSet<String>,
    signal_bindings: &std::collections::HashSet<String>,
    comp: &str,
    file: &std::path::Path,
    prop_types: &HashMap<(String, String), String>,
    out: &mut Vec<((String, String), Edge, String)>,
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
                for field in split_top_level_commas(&inner) {
                    if field.is_empty() {
                        continue;
                    }
                    let TokenTree::Ident(key) = &field[0] else {
                        continue;
                    };
                    let key_s = key.to_string();
                    let line = key.span().start().line;
                    // shorthand `prop` — child_prop == ident which must be
                    // a signal binding in this component
                    let value_ident: Option<String> = if field.len() == 1 {
                        if signal_bindings.contains(&key_s) {
                            Some(key_s.clone())
                        } else {
                            None
                        }
                    } else if matches!(&field[1], TokenTree::Punct(p) if p.as_char() == ':')
                        && field.len() == 3
                    {
                        // `prop: <ident>` form — exactly one token after `:`
                        if let TokenTree::Ident(v) = &field[2] {
                            let vs = v.to_string();
                            if signal_bindings.contains(&vs) {
                                Some(vs)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    if let Some(sig) = value_ident {
                        let edge_type = prop_types
                            .get(&(name.clone(), key_s.clone()))
                            .cloned()
                            .unwrap_or_else(|| "Signal<_>".into());
                        out.push((
                            (comp.to_string(), sig),
                            Edge {
                                child: name.clone(),
                                child_prop: key_s,
                                file: file.to_path_buf(),
                                line,
                            },
                            edge_type,
                        ));
                    }
                }
            }
        }
        if let TokenTree::Group(g) = &tokens[i] {
            let inner: Vec<TokenTree> = g.stream().into_iter().collect();
            find_forwarded_signal_origins(
                &inner,
                known,
                signal_bindings,
                comp,
                file,
                prop_types,
                out,
            );
        }
        i += 1;
    }
}

fn split_top_level_commas(tokens: &[TokenTree]) -> Vec<Vec<TokenTree>> {
    let mut out = Vec::new();
    let mut cur: Vec<TokenTree> = Vec::new();
    for tt in tokens {
        if let TokenTree::Punct(p) = tt {
            if p.as_char() == ',' && p.spacing() == proc_macro2::Spacing::Alone {
                out.push(std::mem::take(&mut cur));
                continue;
            }
        }
        cur.push(tt.clone());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn is_component_fn(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path()
            .segments
            .last()
            .map(|s| s.ident == "component")
            .unwrap_or(false)
    })
}

// Surface parse errors that the origin scan touched, so consumers see
// the same partial-coverage hint as `prop_drill` does. (Currently
// unused — `drills.parse_errors` already aggregates from the same files —
// but kept here in case the origin scan grows to read files prop_drill
// skipped.)
#[allow(dead_code)]
fn collect_origin_parse_errors(src_root: &std::path::Path) -> Vec<ParseError> {
    collect_parse_errors(&walk_rs_files(src_root))
}

fn is_signal_type(ty: &str) -> bool {
    let s = ty.replace(' ', "");
    s.contains("Signal<") || s.contains("ReadSignal<") || s.contains("WriteSignal<")
}

/// Generate a copy-pasteable fix sketch. Names the actual signal type so
/// the caller can drop the snippet into the root component and let the
/// compiler verify the rest.
fn build_fix_snippet(root: &str, prop: &str, ty: &str, chain: &[String]) -> String {
    let consumers: Vec<&str> = chain.iter().skip(1).map(|s| s.as_str()).collect();
    format!(
        "// In `{root}`: stop threading `{prop}` through props — provide it once via context.\n\
         use_context_provider(|| {prop});  // `{prop}: {ty}`\n\
         \n\
         // Then in each consumer ({consumers}):\n\
         let {prop} = use_context::<{ty}>();",
        consumers = consumers.join(", "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_signal_type_recognises_common_shapes() {
        assert!(is_signal_type("Signal<Option<String>>"));
        assert!(is_signal_type("Signal < Option < String > >"));
        assert!(is_signal_type("ReadSignal<u32>"));
        assert!(is_signal_type("WriteSignal<bool>"));
        assert!(!is_signal_type("String"));
        assert!(!is_signal_type("Callback<()>"));
        assert!(
            !is_signal_type("Option<String>"),
            "plain Option must not match",
        );
    }

    #[test]
    fn build_fix_snippet_names_signal_type_and_consumers() {
        let snippet = build_fix_snippet(
            "BoardBody",
            "dragging",
            "Signal<Option<String>>",
            &["BoardBody".into(), "Column".into(), "CardItem".into()],
        );
        assert!(snippet.contains("use_context_provider(|| dragging)"));
        assert!(snippet.contains("use_context::<Signal<Option<String>>>"));
        assert!(snippet.contains("Column, CardItem"));
    }
}
