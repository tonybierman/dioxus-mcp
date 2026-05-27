//! Declarative-DSL scaffolding tools.
//!
//! `get_dsl_spec` returns the YAML vocabulary describing every DSL primitive.
//! `execute_code` parses a YAML doc and materializes the corresponding Dioxus
//! 0.7 source files in one shot.
//!
//! Single source of truth: each primitive has a colocated `&'static str` spec
//! block AND a Rust struct used both for serde deserialization and to drive
//! the per-primitive generator. The `spec_examples_round_trip` unit test
//! enforces that every spec example deserializes into its struct.

mod types;

mod specs;
mod templates;

mod spec;
pub use spec::*;

mod cargo;
mod cargo_patch;
mod describe_component;
mod dx_components;
mod execute;
mod generate;
mod list_components;
mod modify;
mod plan;
mod preflight;
mod propose;
mod remove;
mod render;
mod render_model;
mod resources;
mod text_edit;
mod util;
mod verify_install;
mod wire;

pub use describe_component::*;
pub use dx_components::*;
pub use execute::*;
pub use list_components::*;
pub use propose::*;
pub use verify_install::*;

// Convenience flat re-exports for the test module — tests.rs uses `super::*`
// and exercises items from every sub-module.
#[cfg(test)]
#[allow(unused_imports)]
mod test_imports {
    pub(super) use std::collections::BTreeSet;

    pub(super) use minijinja::context;

    pub(super) use crate::state::State;
    pub(super) use crate::tools::scaffold::ScaffoldResult;

    pub(super) use super::cargo_patch::*;
    pub(super) use super::dx_components::*;
    pub(super) use super::generate::*;
    pub(super) use super::modify::*;
    pub(super) use super::plan::*;
    pub(super) use super::preflight::*;
    pub(super) use super::remove::*;
    pub(super) use super::render::*;
    pub(super) use super::resources::*;
    pub(super) use super::specs::*;
    pub(super) use super::templates::*;
    pub(super) use super::text_edit::*;
    pub(super) use super::types::*;
    pub(super) use super::util::*;
    pub(super) use super::wire::*;
}
#[cfg(test)]
#[allow(unused_imports)]
use test_imports::*;

#[cfg(test)]
mod tests;
