//! Project-wide audits that surface misconfiguration or unused resources.
//!
//! - `asset_audit` — files in `assets/` not referenced by any `asset!()` macro
//!   and `asset!()` references to files that don't exist
//! - `audit_feature_flags` — Cargo / dioxus.toml feature combos that conflict
//!   or mis-wire fullstack
//! - `auth_map` — cross-references route guards with server-fn cookie
//!   extractors and reports likely auth mismatches
//! - `openapi_spec` — derive an OpenAPI 3.1 spec from `#[server]` functions

pub mod asset_audit;
pub mod audit_feature_flags;
pub mod auth_map;
pub mod openapi_spec;
