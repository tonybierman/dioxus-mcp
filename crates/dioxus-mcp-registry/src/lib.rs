//! Shared descriptor schema for dioxus-mcp's theme/component/layout registry.
//!
//! Pure serde data so both the host MCP server and the wasm playground can
//! depend on it. The host loads descriptors from TOML on disk and seeds the
//! built-in defaults; the playground receives the merged [`Registry`] as JSON
//! over MCP. Neither a parser nor any host-only dep lives here.

mod component;
mod ids;
mod layout;
mod render_model;
mod theme;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use component::ComponentDescriptor;
pub use ids::{LayoutId, ThemeId};
pub use layout::{LayoutDescriptor, PreviewSkeleton};
pub use render_model::{Behavior, RenderField, RenderModel, RenderNode, Slot};
pub use theme::{ThemeDescriptor, ThemeTokens};

/// The merged registry: built-in defaults overlaid by runtime-loaded
/// descriptors. String-keyed so entries can be added at runtime. Loading and
/// the built-in seed live host-side (they touch the filesystem / host consts);
/// this type and [`Registry::overlay`] are pure so the playground shares them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub themes: BTreeMap<String, ThemeDescriptor>,
    #[serde(default)]
    pub components: BTreeMap<String, ComponentDescriptor>,
    #[serde(default)]
    pub layouts: BTreeMap<String, LayoutDescriptor>,
}

impl Registry {
    /// Merge `other` on top of `self` (higher precedence): per-id entries in
    /// `other` replace those in `self`. Used to layer global, then project
    /// descriptors over the embedded built-ins.
    pub fn overlay(&mut self, other: Registry) {
        self.themes.extend(other.themes);
        self.components.extend(other.components);
        self.layouts.extend(other.layouts);
    }
}
