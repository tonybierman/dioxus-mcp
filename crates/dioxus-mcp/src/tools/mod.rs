pub mod asset_audit;
pub mod audit_feature_flags;
pub mod check_rsx;
pub mod dead_components;
pub mod dsl;
pub mod explain_signal_graph;
pub mod find_example;
pub mod lint_project;
pub mod openapi_spec;
pub mod project_index;
pub mod project_tour;
pub mod prop_drill;
pub mod props_lint;
pub mod route_map;
pub mod runtime_events;
pub mod scaffold;
pub mod scan;
pub mod search_docs;
pub mod server_fn_call_graph;
pub mod server_fn_summary;
pub mod signal_lint;

use std::path::PathBuf;
use std::sync::Arc;

use crate::state::State;

pub(crate) fn tighten_type(s: &str) -> String {
    // proc-macro2's stringification spaces every punct: `Vec < String >`, `T :: U`.
    s.replace(" < ", "<")
        .replace(" > ", ">")
        .replace("< ", "<")
        .replace(" >", ">")
        .replace(" , ", ", ")
        .replace(" :: ", "::")
}

/// Resolve a file path argument against the project root. Absolute paths are
/// returned as-is; relative paths are joined to `project_root` if provided,
/// otherwise to the state's project manifest dir (or starting cwd).
pub(crate) async fn resolve_in_project(
    state: &Arc<State>,
    file: &str,
    project_root: Option<&str>,
) -> PathBuf {
    let p = PathBuf::from(file);
    if p.is_absolute() {
        return p;
    }
    let base = if let Some(root) = project_root {
        let info = crate::project::ProjectInfo::detect(std::path::Path::new(root));
        info.manifest_dir().unwrap_or_else(|| PathBuf::from(root))
    } else {
        let project = state.project.lock().await;
        project
            .manifest_dir()
            .unwrap_or_else(|| state.project_root.clone())
    };
    base.join(p)
}
