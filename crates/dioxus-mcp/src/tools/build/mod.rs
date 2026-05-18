//! Build-time verification.
//!
//! `build_and_smoke` runs `cargo check` against the Dioxus project with a
//! sensible feature combo and returns structured diagnostics — the
//! closing-the-loop step after `execute_code` scaffolds new files.

pub mod build_and_smoke;
