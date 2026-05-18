//! Source-level lints over the project's `src/` tree.
//!
//! Each individual lint is a self-contained tool; `lint_project` is the
//! aggregator that runs every lint in the suite and merges results.

pub mod check_rsx;
pub mod components_audit;
pub mod derived_view_no_memo;
pub mod duplicate_helper_client_server;
pub mod empty_async_error_arm;
pub mod insecure_set_cookie;
pub mod lint_project;
pub mod magic_id_prefix;
pub mod optimistic_lock_gate;
pub mod polling_future_no_backoff;
pub mod presence_map_unbounded;
pub mod props_lint;
pub mod reinvented_widget;
pub mod repeated_auth_extractor;
pub mod server_state_blocking_locks;
pub mod shared_enum_validation;
pub mod signal_drilled_2_levels;
pub mod signal_lint;
pub mod vec_or_owned_prop_passthrough;
