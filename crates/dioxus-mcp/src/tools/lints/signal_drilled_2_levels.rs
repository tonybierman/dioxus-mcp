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
//! Fix suggestion: lift the signal into a context provider at the nearest
//! common ancestor (often the topmost component that creates the signal)
//! and have each consumer call `use_context::<Signal<T>>()` directly.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::state::State;
use crate::tools::ast::ParseError;
use crate::tools::inspect::project_index::{ProjectIndexParams, project_index};
use crate::tools::inspect::prop_drill::{PropDrillParams, prop_drill};

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
    #[derive(Clone)]
    struct Edge {
        child: String,
        child_prop: String,
        file: PathBuf,
        line: usize,
    }
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
