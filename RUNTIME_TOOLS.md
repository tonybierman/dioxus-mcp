# dioxus-mcp runtime tools

Static analysis answers "what does the code look like" questions. The runtime
tools answer "what is the code doing right now": which components rendered,
which signals wrote, how long server fns took, what panicked. They read a
JSON-lines event log written by the [`dioxus-mcp-probe`](probe/) crate that
you install in your Dioxus app.

The flow:

```
+---------------------+     writes JSONL     +-------------------------------+
| Dioxus app          | -------------------> | target/dioxus-mcp/events.jsonl |
|  + dioxus-mcp-probe |                      +-------------------------------+
+---------------------+                                    |
                                                           | tails on demand
                                                           v
                                              +-----------------------------+
                                              | runtime_events              |
                                              | server_fn_summary           |
                                              +-----------------------------+
```

## Setting up the probe

In **your Dioxus app's** `Cargo.toml`:

```toml
[dev-dependencies]
dioxus-mcp-probe = { git = "https://github.com/tonybierman/dioxus-mcp", tag = "probe-v0.1.1" }
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
emitting your own `tracing::info_span!` calls (see the smoke example at
[`probe/examples/smoke.rs`](probe/examples/smoke.rs)).

## Event schema

Every line is one JSON object on schema `v: 1`:

| Field | Always | Notes |
|---|---|---|
| `v` | yes | Schema version. Currently `1`. |
| `ts` | yes | RFC 3339 UTC. |
| `kind` | yes | `render` \| `signal_write` \| `signal_read` \| `signal` \| `server_fn` \| `route` \| `panic` \| `event`. |
| `span` | for span events | The original `tracing` span name. |
| `component` | renders / signal events tied to a component | |
| `trigger` | renders | What caused the re-render (e.g. `signal:count`, `resource:fetch_user`). |
| `duration_us` | optional | Microseconds. Present on render and server-fn end events when the probe captures it. |
| `signal` | signal_write | Signal name. |
| `subscriber_count` | signal_write | Number of listeners notified. |
| `name` | server_fn | Server fn name. |
| `phase` | server_fn | `start` or `end`. |
| `call_id` | server_fn | Probe-local correlation ID for pairing start/end. |
| `ok` | server_fn end | `true` if the call succeeded. |
| `to` | route | Target path. |
| `message` / `file` / `line` | panic | Panic info. |

---

## `runtime_events`

**Purpose:** Tail the JSON-lines event log and return events matching a
filter. When the log doesn't exist (probe never installed or the app
hasn't been run yet) the tool returns an empty `events` array with a clear
note — it never errors on a missing log. Up to one rotated file
(`events.1.jsonl`) is read when the live file is newer than the `since`
cutoff.

**Args:** `kind?` (one of the schema kinds above), `since?` (RFC 3339,
default last 5 minutes), `component?`, `signal?`, `server_fn?`, `limit?`
(default 200, hard cap 2000), `log_path?` (override; default
`target/dioxus-mcp/events.jsonl` under the crate root), `project_root?`.

**Example call:**
```json
{
  "name": "runtime_events",
  "arguments": {"kind": "render", "component": "Home", "since": "2026-05-14T18:30:00Z"}
}
```

**Ask Claude:** "Show me the last few renders of the Home component."

**Demonstrated in:** [`tests/fixtures/runtime_events/events.jsonl`](tests/fixtures/runtime_events/events.jsonl) — hand-crafted log with one of every event kind; the `tool_runtime_events` test in `tests/integration.rs` exercises kind/component/server_fn/limit filtering and the missing-log empty-list path.

**Try it end-to-end:** [`probe/examples/smoke.rs`](probe/examples/smoke.rs) installs the probe with a custom log path, emits a Dioxus-shaped span for each event kind, and panics in a child thread.

```
cargo run --example smoke -p dioxus-mcp-probe -- /tmp/probe-smoke.jsonl
```

then call `runtime_events` with `{"log_path": "/tmp/probe-smoke.jsonl"}` to see the seven captured events come back through the MCP tool. Useful when validating a fresh checkout of the probe without standing up a real Dioxus app.

---

## `server_fn_summary`

**Purpose:** Per-server-fn latency summary derived from the probe log.
Pairs `phase=start` with `phase=end` events by `call_id` and returns
count, ok/err, and min/p50/p95/max latency (µs) for each `#[server]` fn
called in the window. Starts without a matching end (still in flight, or
dropped) are surfaced as `pending`. Latencies use the `duration_us` field
when the probe recorded one; otherwise the tool computes it from
timestamps. Percentiles use the nearest-rank method.

**Args:** `since?` (RFC 3339, default last 5 minutes), `server_fn?` (limit
to one name), `log_path?` (override), `project_root?`.

**Example call:**
```json
{"name": "server_fn_summary", "arguments": {}}
```

**Ask Claude:** "What's the latency distribution for fetch_user this hour?"

**Response shape:**
```json
{
  "summaries": [
    {
      "name": "fetch_user",
      "completed": {
        "count": 10, "ok": 10, "err": 0,
        "min_us": 100, "p50_us": 600, "p95_us": 1000, "max_us": 1000,
        "total_ms": 5.5
      },
      "pending": 1
    }
  ],
  "log_files_scanned": ["/path/to/target/dioxus-mcp/events.jsonl"],
  "notes": []
}
```

**Demonstrated in:** [`tests/fixtures/server_fn_summary/events.jsonl`](tests/fixtures/server_fn_summary/events.jsonl) — 10 `fetch_user` calls with durations 100…1000 µs, 2 `save_post` calls (one ok, one err), and one `fetch_user` start with no matching end (the `pending` case). The `tool_server_fn_summary` test in `tests/integration.rs` asserts on counts, percentiles, ok/err split, the `pending` field, the `server_fn` filter, and the missing-log empty-list path.

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
