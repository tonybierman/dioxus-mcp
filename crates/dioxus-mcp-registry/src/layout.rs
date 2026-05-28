//! Layout descriptors — what it takes to both *generate* a screen (a minijinja
//! template the host renders) and *preview* it (a [`PreviewSkeleton`] the
//! playground tree-walks). The two are deliberately separate: the template
//! emits Rust/RSX text, the skeleton is a constrained node tree, so the wasm
//! playground never has to parse RSX.

use serde::{Deserialize, Serialize};

use crate::render_model::{Behavior, RenderNode};

/// The preview half of a layout: a node tree (with [`Slot`](crate::Slot)s the
/// interpreter fills from resolved screen data) plus an optional interaction
/// model.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PreviewSkeleton {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<RenderNode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior: Option<Behavior>,
}

/// One layout. `template` drives codegen for `complex: false` layouts; complex
/// ones keep their host-side Rust sub-renderer and the registry is just the
/// dispatch table. `preview` drives the cockpit's approximate render.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LayoutDescriptor {
    /// Stable id == the screen `kind` string (`resource_list`, `client_crud`, …).
    pub id: String,
    /// Short navigator-rail label hint ("List"/"New"/"Edit"/<name>).
    #[serde(default)]
    pub label: String,
    /// Sort key within a resource group in the navigator rail.
    #[serde(default)]
    pub nav_rank: u8,
    /// minijinja template text for codegen (`complex: false` layouts only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// When true, codegen dispatches to a host Rust sub-renderer rather than
    /// `template` (crud table, edit form, client_crud body builder).
    #[serde(default)]
    pub complex: bool,
    /// Raw CSS the layout's markup depends on, written alongside the screen as
    /// `assets/{snake}.css` on scaffold. This is what makes a layout
    /// *structurally reproducible* in any project: a layout that styles via
    /// semantic class names (rather than Tailwind utilities) carries the rules
    /// that define those classes here, so the structure survives regardless of
    /// the target project's CSS toolchain. Empty for utility-class layouts that
    /// declare `requires` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub styles: Option<String>,
    /// The styling family the layout's markup needs to *look* structural when it
    /// ships no `styles` of its own (`tailwind` | `vanilla_css`). A hint, not a
    /// gate: scaffold surfaces it as an advisory so a utility-class layout
    /// dropped into a project without that toolchain fails loudly instead of
    /// rendering as unstyled divs. `None` means self-contained (see `styles`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires: Option<String>,
    /// Documentation of the context variables `template`/sub-renderer expects.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_vars: Vec<String>,
    /// The approximate-preview skeleton.
    #[serde(default)]
    pub preview: PreviewSkeleton,
}
