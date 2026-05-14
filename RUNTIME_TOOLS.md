# dioxus-mcp runtime tools

Static analysis answers "what does the code look like" questions. The runtime
tools answer "what is the code doing right now": which components rendered,
which signals wrote, how long server fns took, what panicked. They read a
JSON-lines event log written by the [`dioxus-mcp-probe`](crates/probe/) crate that
you install in your Dioxus app.

## Setting up the probe

In **your Dioxus app's** `Cargo.toml`:

```toml
[dev-dependencies]
dioxus-mcp-probe = { git = "https://github.com/tonybierman/dioxus-mcp", tag = "probe-v0.1.2" }
```

In `src/main.rs`:

```rust
fn main() {
    let _probe = dioxus_mcp_probe::install();   // keep alive for process lifetime
    // ...your existing dioxus launch...
}
```

The probe is a no-op in release builds unless the `force` cargo feature is
enabled. After your app runs at least once, events accumulate under
`target/dioxus-mcp/events.jsonl`. If that file is empty, widen what the
probe captures by setting `extra_targets` on `ProbeConfig` and/or
emitting your own `tracing::info_span!` calls (see the smoke app at
[`examples/smoke-app`](examples/smoke-app)).

Every log line is one JSON object on schema `v: 1`. Field-by-field
breakdown lives in [TOOLS_REFERENCE](TOOLS_REFERENCE.md#event-schema).

---

## `runtime_events`

**Purpose:** Tail the JSON-lines event log and return events matching a filter.

**Ask Claude — phrasings that route to this tool:**
- "Show me the last few renders of the Home component."
- "Which signals wrote in the past minute?"
- "List the server-fn calls for `fetch_user` today."
- "Was there a panic? Where did it happen?"
- "Show every signal write to `count`."
- "What renders has UserPage done since I started the app?"
- "Tail the runtime log — give me the latest 50 events of any kind."

Args, JSON examples, and the fixture this tool is exercised against
live in [TOOLS_REFERENCE](TOOLS_REFERENCE.md#runtime_events).

---

## `server_fn_summary`

**Purpose:** Per-server-fn latency summary derived from the probe log — counts, ok/err, and p50/p95/max latency (µs) per `#[server]` fn.

**Ask Claude — phrasings that route to this tool:**
- "What's the latency distribution for `fetch_user` this hour?"
- "Which server functions are slowest?"
- "Are any server fns erroring?"
- "How many `save_post` calls have run, and how many failed?"
- "What's still pending? Anything stuck mid-flight?"
- "Give me a summary of server-fn activity over the last 5 minutes."
- "Show p95 latency for every server fn."

Args, response shape, and the fixture this tool is exercised against
live in [TOOLS_REFERENCE](TOOLS_REFERENCE.md#server_fn_summary).

---

## Caveats

- **The probe is best-effort.** A bounded queue drops events under load
  rather than blocking renders, and the log rotates at 10 MiB by default
  (configurable via `ProbeConfig`).
- **The classifier is not contractual.** It maps `tracing` targets/span
  names to schema kinds heuristically; Dioxus internals change, so first
  time you wire the probe into a real app, expect to widen
  `extra_targets` or emit your own spans.
- **Server-side only for now.** WASM target has no filesystem; capturing
  browser-side events would need a different transport. Out of scope for
  v1.
- **Dev tool, not production telemetry.** No-op in release unless the
  `force` feature is on; do not point a production app at this.
