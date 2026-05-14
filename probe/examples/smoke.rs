//! End-to-end smoke test for the probe.
//!
//! Installs the probe pointed at a configurable log path, emits a handful
//! of Dioxus-shaped tracing spans, triggers a panic in a child thread, and
//! flushes. After running, query the log via the MCP `runtime_events` tool
//! or just `cat` it.
//!
//! ```
//! cargo run --example smoke -p dioxus-mcp-probe -- /tmp/probe-smoke.jsonl
//! ```

use std::path::PathBuf;
use std::thread;
use std::time::Duration;

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

    // Spans carry an explicit name; the layer's classifier maps `target` +
    // `name` to one of the schema kinds.
    let _ = info_span!(target: "dioxus_core", "render", component = "Home", trigger = "signal:count", duration_us = 412).entered();
    let _ = info_span!(target: "dioxus_signals", "signal_write", signal = "count", subscriber_count = 3).entered();
    let _ = info_span!(target: "dioxus_core", "render", component = "UserPage", trigger = "resource:fetch_user", duration_us = 910).entered();
    let _ = info_span!(target: "dioxus_fullstack", "server_fn", name = "fetch_user", phase = "start", call_id = "abc").entered();
    let _ = info_span!(target: "dioxus_fullstack", "server_fn", name = "fetch_user", phase = "end", call_id = "abc", duration_us = 12345, ok = true).entered();
    let _ = info_span!(target: "dioxus_router", "navigate", to = "/users/42").entered();

    // Provoke a panic in a child thread so the main process keeps running
    // and the probe gets a chance to flush the panic event.
    let _ = thread::spawn(|| panic!("synthetic boom from probe smoke test")).join();

    // Let the writer drain.
    thread::sleep(Duration::from_millis(300));

    eprintln!("wrote events to {}", log_path.display());
}
