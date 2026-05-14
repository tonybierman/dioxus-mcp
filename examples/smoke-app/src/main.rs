//! End-to-end smoke test for `dioxus-mcp-probe`.
//!
//! Runs a real Dioxus `VirtualDom` headlessly so the probe captures
//! Dioxus's own TRACE-level spans (renders, diffs, signal creation) from
//! `dioxus_core` and `dioxus_signals`, then emits a few synthetic spans
//! for the subsystems Dioxus 0.7 doesn't instrument (`dioxus_router`,
//! `dioxus_fullstack`) and provokes a panic in a child thread.
//!
//! ```
//! cargo run -p dioxus-mcp-probe-smoke -- /tmp/probe-smoke.jsonl
//! ```
//!
//! Then query the log via the MCP `runtime_events` tool, or just `cat` it.

use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use dioxus::prelude::*;
use dioxus_mcp_probe::{install_with, ProbeConfig};
use tracing::info_span;

fn main() {
    let log_path = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "/tmp/probe-smoke.jsonl".into()),
    );
    let _ = std::fs::remove_file(&log_path);
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let cfg = ProbeConfig {
        log_path: log_path.clone(),
        ..ProbeConfig::default()
    };
    let _probe = install_with(cfg);

    // Drive a real VirtualDom. Dioxus 0.7 emits TRACE spans named
    // `VirtualDom::run_scope`, `render`, `VirtualDom::diff_scope`, and
    // `VirtualDom::create_scope` under target `dioxus_core`, plus
    // signal-creation spans under `dioxus_signals`. The probe's layer
    // captures them.
    let mut dom = VirtualDom::new(app);
    dom.rebuild_in_place();

    // Dioxus 0.7's router and fullstack crates carry no tracing
    // instrumentation, so emit shaped spans to cover those event kinds.
    let _ = info_span!(target: "dioxus_router", "navigate", to = "/users/42").entered();
    let _ = info_span!(
        target: "dioxus_fullstack",
        "server_fn",
        name = "fetch_user",
        phase = "start",
        call_id = "abc"
    )
    .entered();
    let _ = info_span!(
        target: "dioxus_fullstack",
        "server_fn",
        name = "fetch_user",
        phase = "end",
        call_id = "abc",
        duration_us = 12345,
        ok = true
    )
    .entered();

    // Exercise the panic hook in a child thread so the main process keeps
    // running long enough to flush.
    let _ = thread::spawn(|| panic!("synthetic boom from probe smoke test")).join();

    thread::sleep(Duration::from_millis(300));

    eprintln!("wrote events to {}", log_path.display());
}

fn app() -> Element {
    let count = use_signal(|| 0_i32);
    rsx! {
        div {
            class: "smoke",
            "count: {count}"
        }
    }
}
