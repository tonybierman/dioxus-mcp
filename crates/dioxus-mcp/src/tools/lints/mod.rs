//! Source-level lints over the project's `src/` tree.
//!
//! Each individual lint is a self-contained tool; `lint_project` is the
//! aggregator that runs every lint in the suite and merges results.

pub mod check_rsx;
pub mod lint_project;
pub mod optimistic_lock_gate;
pub mod props_lint;
pub mod reinvented_widget;
pub mod server_state_blocking_locks;
pub mod signal_lint;
