# dioxus-mcp tool reference

Per-tool technical details: argument schemas, example JSON-RPC calls,
and the fixtures/integration tests each tool is exercised against.
The user-facing overviews — purpose + how to ask Claude for them —
live in [TOOLS.md](TOOLS.md) and [RUNTIME_TOOLS.md](RUNTIME_TOOLS.md).

All project-aware tools accept an optional `project_root` (absolute
path); when omitted, the project is detected by walking up from the
server's CWD to the first `Cargo.toml` with a `dioxus` dependency.

---

## Project introspection

### `project_tour`

Also returns a pre-rendered markdown summary suitable for dropping straight into a prompt or PR description.

**Args:** `include?`, `exclude?` (subset of `["audit","routes","index","assets"]`),
`max_items_per_section?` (default 50), `project_root?`.

**Example call:**
```json
{"name": "project_tour", "arguments": {}}
```

**Demonstrated in:** the whole `tests/fixtures/sample-project/` tree.

---

### `route_map`

Each route comes back with raw `path`, nest-prefixed `full_path`, target component, typed params, and the `layouts` / `nests` stacks it sits under.

**Args:** `router_file?` (relative to crate root; auto-detected by default),
`project_root?`.

**Example call:**
```json
{"name": "route_map", "arguments": {}}
```

**Demonstrated in:** [`src/router.rs`](tests/fixtures/sample-project/src/router.rs) — `Route` enum with `#[layout]`, `#[nest]`, and typed params.

---

### `project_index`

Props and server-fn args are reported with an optional flag; server-fn return types are unwrapped (`ServerFnResult<T>` → `T`).

**Args:** `kind?` (`"component"` or `"server_fn"` to filter), `path?` (subdir
to scan, default `src/`), `project_root?`.

**Example call:**
```json
{"name": "project_index", "arguments": {}}
```

**Demonstrated in:** [`src/components/home.rs`](tests/fixtures/sample-project/src/components/home.rs) and [`src/server/fetch_user.rs`](tests/fixtures/sample-project/src/server/fetch_user.rs) — Props-struct component and a server fn with typed args.

---

### `server_fn_call_graph`

Call sites carry `caller_file`, `caller_line`, `enclosing_fn`, and `full_path`. Server fns with zero callers are returned under `orphans`.

**Args:** `project_root?`.

**Example call:**
```json
{"name": "server_fn_call_graph", "arguments": {}}
```

**Demonstrated in:** [`src/components/user_page.rs`](tests/fixtures/sample-project/src/components/user_page.rs) (calls `fetch_user`) and [`src/server/orphan_fn.rs`](tests/fixtures/sample-project/src/server/orphan_fn.rs) (never called).

---

### `dead_components`

`App` plus every component referenced from the Routable enum (route targets + layouts) is treated as a root.

**Args:** `roots?` (extra component names to treat as alive), `project_root?`.

**Example call:**
```json
{"name": "dead_components", "arguments": {"roots": ["RootLayout"]}}
```

**Demonstrated in:** [`src/components/unused.rs`](tests/fixtures/sample-project/src/components/unused.rs) — defined but referenced nowhere in any `rsx!`.

---

### `asset_audit`

Dynamic (non-string-literal) `asset!()` calls can't be resolved statically; they're returned as a skipped count.

**Args:** `assets_dirs?` (default `["assets"]`), `project_root?`.

**Example call:**
```json
{"name": "asset_audit", "arguments": {"assets_dirs": ["assets", "public"]}}
```

**Demonstrated in:** [`assets/`](tests/fixtures/sample-project/assets/) (with `orphan.css` unreferenced) and [`src/main.rs`](tests/fixtures/sample-project/src/main.rs) (referencing a missing `missing.svg`).

---

### `openapi_spec`

Server fns become POST endpoints at `/api/{ServerName}` with JSON request and response. Arg and return-type schemas are resolved by walking local `#[derive(Serialize)] / #[derive(Deserialize)]` definitions; unknown type names land in `unresolved_types`. Server fns without an explicit `#[server(Name)]` use the fn ident and are listed under `guessed_paths`, since Dioxus may hash the path at runtime.

**Args:** `server_fn_prefix?` (default `"/api"`), `include_routes?`
(default `false`), `title?` (default crate name), `version?` (default
crate version), `router_file?` (forwarded to `route_map` when
`include_routes`), `project_root?`.

**Example call:**
```json
{"name": "openapi_spec", "arguments": {"include_routes": true}}
```

**Demonstrated in:** [`src/server/list_posts.rs`](tests/fixtures/sample-project/src/server/list_posts.rs) — `#[server(ListPosts)]` taking a `ListPostsInput` struct and returning `Vec<Post>`; both types are resolved from local `#[derive(Serialize, Deserialize)]` definitions.

---

## Lints

### `check_rsx`

Currently flags `for` loops missing a `key:` attribute and event-handler closures that omit the event parameter.

**Args:** `file` (required, relative to crate root or absolute), `project_root?`.

**Example call:**
```json
{"name": "check_rsx", "arguments": {"file": "src/lint_demo.rs"}}
```

**Demonstrated in:** [`src/lint_demo.rs`](tests/fixtures/sample-project/src/lint_demo.rs) — `for` loop without `key:` and an `onclick: move || {}` with no event arg.

---

### `signal_lint`

Covers `for` / `while` / `loop` bodies, including loops nested inside `rsx!` macro bodies.

**Args:** `project_root?`.

**Example call:**
```json
{"name": "signal_lint", "arguments": {}}
```

**Demonstrated in:** [`src/components/home.rs`](tests/fixtures/sample-project/src/components/home.rs) — `use_signal` inside an rsx! `for` loop.

---

### `props_lint`

**Args:** `project_root?`.

**Example call:**
```json
{"name": "props_lint", "arguments": {}}
```

**Demonstrated in:** [`src/components/child.rs`](tests/fixtures/sample-project/src/components/child.rs) — `ChildProps` derives `Props, Clone` but not `PartialEq`.

---

### `prop_drill`

Matches bare ident, `prop.clone()`, `prop.into()`, `prop.to_owned()`, `prop.read()`, `prop.peek()`, `prop.cloned()`, and the `props.NAME` equivalents for Props-struct components. Each finding is tagged with the matched form via `via`.

**Args:** `project_root?`.

**Example call:**
```json
{"name": "prop_drill", "arguments": {}}
```

**Demonstrated in:** [`src/components/home.rs`](tests/fixtures/sample-project/src/components/home.rs) — `Child { name: props.title.clone(), user_id: props.user_id }`.

---

### `audit_feature_flags`

Flags conflicting render targets (web + desktop without fullstack), broken fullstack wiring (missing `server` or `web`), and `[features] default = ["web","server"]` footguns. Also confirms the detected Dioxus version.

**Args:** `project_root?`.

**Example call:**
```json
{"name": "audit_feature_flags", "arguments": {}}
```

**Demonstrated in:** [`Cargo.toml`](tests/fixtures/sample-project/Cargo.toml) — clean `fullstack + web + server` setup.

---

### `explain_signal_graph`

Covers `use_signal` / `use_memo` / `use_resource` / `use_effect`. Memos and effects that capture no other signals are flagged — they'll never re-run on state change.

**Args:** `file` (required), `component?` (filter to one), `project_root?`.

**Example call:**
```json
{"name": "explain_signal_graph", "arguments": {"file": "src/components/home.rs"}}
```

**Demonstrated in:** [`src/components/home.rs`](tests/fixtures/sample-project/src/components/home.rs) — `use_signal` plus a `use_memo` that reads it.

---

## Scaffolding

### `create_component`

Writes under `src/components/` by default (override via `path`) and wires the new module into `components/mod.rs`.

**Args:** `name` (required; normalized to PascalCase/snake_case),
`props?` (`[{name, type, optional?}]`), `path?` (default `src/components`),
`project_root?`.

**Example call:**
```json
{
  "name": "create_component",
  "arguments": {
    "name": "UserCard",
    "props": [
      {"name": "id", "type": "i32"},
      {"name": "label", "type": "String", "optional": true}
    ]
  }
}
```

**Demonstrated in:** `tool_create_component` in [`tests/integration.rs`](tests/integration.rs) — runs against a tempdir copy of the fixture.

---

### `create_route`

**Args:** `path` (required, e.g. `/users/:id`), `component` (required,
PascalCase), `router_file?` (auto-detected), `project_root?`.

**Example call:**
```json
{
  "name": "create_route",
  "arguments": {"path": "/settings", "component": "Settings"}
}
```

**Demonstrated in:** `tool_create_route` in [`tests/integration.rs`](tests/integration.rs) — inserts a variant into the fixture's `Route` enum.

---

### `create_server_fn`

Refuses if the project isn't fullstack-capable (no `fullstack` feature, and missing one of `web` / `server` on the dioxus dep) — run `audit_feature_flags` first if it errors.

**Args:** `name` (required), `args?` (`[{name, type}]`),
`return_type?` (default `String`), `project_root?`.

**Example call:**
```json
{
  "name": "create_server_fn",
  "arguments": {
    "name": "fetch_users",
    "args": [{"name": "limit", "type": "u32"}],
    "return_type": "Vec<User>"
  }
}
```

**Demonstrated in:** `tool_create_server_fn` in [`tests/integration.rs`](tests/integration.rs) — generates a new file under `src/server/` of the fixture.

---

## Runtime

### Event schema

Every line of the probe log is one JSON object on schema `v: 1`:

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

### `runtime_events`

Never errors on a missing log — if the file doesn't exist (probe not installed, or the app hasn't been run yet) the tool returns an empty `events` array with a note. Up to one rotated file (`events.1.jsonl`) is read when the live file is newer than the `since` cutoff.

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

**Demonstrated in:** [`tests/fixtures/runtime_events/events.jsonl`](tests/fixtures/runtime_events/events.jsonl) — hand-crafted log with one of every event kind; the `tool_runtime_events` test in `tests/integration.rs` exercises kind/component/server_fn/limit filtering and the missing-log empty-list path.

**Try it end-to-end:** [`examples/smoke-app`](examples/smoke-app) is a real headless Dioxus crate that drives a `VirtualDom` rebuild (so the probe captures Dioxus's own TRACE spans from `dioxus_core` and `dioxus_signals`), emits synthetic spans for the subsystems Dioxus 0.7 doesn't instrument (router, fullstack), and panics in a child thread.

From the workspace root (where claude already is):

```
cargo run -p dioxus-mcp-probe-smoke
```

The log lands at `/tmp/probe-smoke.jsonl` (override via the first arg). Then ask claude for runtime events and pass that path as `log_path`; expect ~25 entries spanning `render`, `signal`, `route`, `server_fn`, and `panic`.

---

### `server_fn_summary`

Pairs `phase=start` with `phase=end` events by `call_id`. Starts without a matching end (still in flight, or dropped) are surfaced as `pending`. Latencies use the `duration_us` field when the probe recorded one; otherwise the tool computes it from timestamps. Percentiles use the nearest-rank method.

**Args:** `since?` (RFC 3339, default last 5 minutes), `server_fn?` (limit
to one name), `log_path?` (override), `project_root?`.

**Example call:**
```json
{"name": "server_fn_summary", "arguments": {}}
```

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

## Docs

### `search_docs`

Returns ranked snippets with URLs. Cached for 15 minutes.

**Args:** `query` (required), `version?` (e.g. `"0.7"`), `limit?`
(default 5).

**Example call:**
```json
{"name": "search_docs", "arguments": {"query": "use_resource"}}
```

**Demonstrated in:** `tool_search_docs` in [`tests/integration.rs`](tests/integration.rs) — `#[ignore]`d offline; run with `cargo test -- --ignored`.

---

### `find_example`

Returns matching files with raw URLs and short excerpts.

**Args:** `concept` (required), `ref?` (branch/tag, default `"main"`),
`limit?`.

**Example call:**
```json
{"name": "find_example", "arguments": {"concept": "fullstack"}}
```

**Demonstrated in:** `tool_find_example` in [`tests/integration.rs`](tests/integration.rs) — `#[ignore]`d offline; run with `cargo test -- --ignored`.
