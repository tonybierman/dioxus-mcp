//! Runtime probe-log readers.
//!
//! These tools read the JSONL log written by the `dioxus-mcp-probe` crate at
//! `target/dioxus-mcp/events.jsonl` — i.e. they observe what a running Dioxus
//! app did, rather than statically inspecting its source.

pub mod runtime_events;
pub mod server_fn_summary;
