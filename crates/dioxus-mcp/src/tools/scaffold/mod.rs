use std::path::PathBuf;
use std::sync::Arc;

use crate::state::State;

pub mod component;
pub mod discovery;
pub mod mod_tree;
pub mod route;
pub mod server_fn;
pub mod types;

pub use component::create_component;
pub use discovery::{
    existing_route_paths, find_crate_root_file, find_routable, has_derive, upsert_crate_mod,
};
pub use mod_tree::upsert_mod_entry;
pub use route::create_route;
pub use server_fn::create_server_fn;
pub use types::{
    ArgSpec, CreateComponentParams, CreateRouteParams, CreateServerFnParams, ModUpsert, PropSpec,
    ScaffoldResult,
};

pub(crate) async fn crate_root(
    state: &Arc<State>,
    project_root: Option<&str>,
) -> Result<PathBuf, String> {
    match project_root {
        Some(root) => {
            let info = crate::project::ProjectInfo::detect(std::path::Path::new(root));
            info.manifest_dir()
                .ok_or_else(|| format!("no Cargo.toml with a dioxus dep found under {root}"))
        }
        None => {
            let project = state.project.lock().await;
            project
                .manifest_dir()
                .ok_or_else(|| "no Cargo.toml found from project root".to_string())
        }
    }
}
