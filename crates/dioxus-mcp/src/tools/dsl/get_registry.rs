//! `get_registry`: return the merged theme/component/layout [`Registry`] as
//! JSON. The cockpit fetches this once per session to drive the theme selector,
//! the navigator's per-layout labels/ranks, and (once layouts emit generic
//! `nodes`) the approximate preview. It's the registry counterpart to
//! `get_dsl_spec` — structured data the wasm client can't reconstruct locally.

use dioxus_mcp_registry::Registry;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::state::State;

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct GetRegistryParams {}

/// The server's registry, loaded fresh from disk (built-ins overlaid by runtime
/// descriptors). Reflects any descriptor added/edited since the server started.
pub async fn get_registry(state: &State, _p: GetRegistryParams) -> Result<Registry, String> {
    Ok(state.registry())
}
