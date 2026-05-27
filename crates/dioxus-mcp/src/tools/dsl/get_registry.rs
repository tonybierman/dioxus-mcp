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

/// Clone of the server's loaded registry (built-ins overlaid by runtime
/// descriptors). Cheap relative to a tool round-trip and static for the session.
pub async fn get_registry(state: &State, _p: GetRegistryParams) -> Result<Registry, String> {
    Ok(state.registry.clone())
}
