//! Project-wide audits that surface misconfiguration or unused resources.
//!
//! - `asset_audit` — files in `assets/` not referenced by any `asset!()` macro
//!   and `asset!()` references to files that don't exist
//! - `audit_feature_flags` — Cargo / dioxus.toml feature combos that conflict
//!   or mis-wire fullstack
//! - `openapi_spec` — derive an OpenAPI 3.1 spec from `#[server]` functions

pub mod asset_audit;
pub mod audit_feature_flags;
pub mod openapi_spec;
