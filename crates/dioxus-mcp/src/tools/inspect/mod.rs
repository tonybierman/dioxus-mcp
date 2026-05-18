//! Read-only project introspection.
//!
//! These tools walk the project's AST to answer "what exists" questions:
//! routes, components, server fns, their relationships, and where they live.

pub mod dead_components;
pub mod explain_signal_graph;
pub mod project_index;
pub mod project_tour;
pub mod prop_drill;
pub mod route_map;
pub mod server_fn_call_graph;
