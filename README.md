# dioxus-mcp

[![CI](https://github.com/tonybierman/dioxus-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/tonybierman/dioxus-mcp/actions/workflows/ci.yml)

An MCP server that gives Claude Code (and any other MCP client) deep static
understanding of a Dioxus 0.7 project: route maps, component/server-fn
indexes, dead-code detection, prop-drilling reports, signal/props lints,
asset audits, OpenAPI generation, and scaffolding helpers — all from the
source tree, with no need to spawn `dx`. Pair it with the companion
`dioxus-mcp-probe` crate to also read runtime events (renders, signal
writes, server-fn timings, panics) captured while the app is running.

## Tools

See [TOOLS.md](TOOLS.md) for each static tool's args, a JSON call example,
and a natural-language prompt that Claude Code will route to it.

Runtime tools (anything that reads the probe's event log) are documented
separately in [RUNTIME_TOOLS.md](RUNTIME_TOOLS.md).

### Project introspection
- **`project_tour`** — one-shot overview: feature audit + routes + index +
  asset audit, plus a pre-rendered markdown summary.
- **`route_map`** — every `#[route(...)]` in the `#[derive(Routable)]`
  enum, with raw + nest-prefixed paths, params, and layout / nest stacks.
- **`project_index`** — every `#[component]` and `#[server]` fn with
  file:line, typed props/args, and `ServerFnResult<T>` unwrapping.
- **`server_fn_call_graph`** — per server fn, every call site
  (`caller_file`, `caller_line`, `enclosing_fn`, `full_path`) plus an
  orphan list.
- **`dead_components`** — components defined but never used in any `rsx!`
  block. `App` + every Routable target + every layout count as roots.
- **`asset_audit`** — files under `assets/` not referenced by any
  `asset!()` macro, and `asset!()` paths pointing at files that don't
  exist.
- **`openapi_spec`** — generate an OpenAPI 3.1 document from `#[server]`
  fns (POST endpoints) and, optionally, router routes. Schemas are
  resolved from local `#[derive(Serialize)] / #[derive(Deserialize)]`
  types; unknowns are reported.

### Lints
- **`check_rsx`** — common `rsx!` mistakes (missing `key:` on iterators,
  parameter-less event handlers).
- **`signal_lint`** — `use_signal` / `use_memo` / `use_resource` /
  `use_effect` inside a `for` / `while` / `loop` body, including loops
  inside `rsx!` macro bodies.
- **`props_lint`** — `#[derive(Props, ...)]` structs missing `PartialEq`.
- **`prop_drill`** — props passed unchanged from a parent into a child;
  each finding tagged `via` ∈ `direct | clone | into | to_owned |
  signal_read | signal_peek | signal_cloned`.
- **`audit_feature_flags`** — Cargo.toml platform-feature sanity
  (conflicting render targets, fullstack mis-wiring, version mismatches).
- **`explain_signal_graph`** — the reactive bindings inside a single
  component: which signals each `use_memo` / `use_effect` reads.

### Scaffolding
- **`create_component`** — new `#[component]` file with optional typed
  Props, registered in `components/mod.rs`.
- **`create_route`** — insert a variant into the existing
  `#[derive(Routable)]` enum.
- **`create_server_fn`** — new `#[server]` fn under `src/server/`,
  refuses if the project isn't fullstack-capable.

### Runtime
See [RUNTIME_TOOLS.md](RUNTIME_TOOLS.md) for the full event schema and tool docs.

- **`runtime_events`** — filter the JSON-lines event log written by the
  `dioxus-mcp-probe` crate (renders, signal writes, server-fn timings,
  panics). Filters: `kind`, `since`, `component`, `signal`, `server_fn`,
  `limit`.
- **`server_fn_summary`** — derived view: per-server-fn count, ok/err,
  and min/p50/p95/max latency over a `since` window.

### Docs
- **`search_docs`** — live-search dioxuslabs.com, scoped to the project's
  Dioxus version, 15-min cached.
- **`find_example`** — search the official Dioxus examples on GitHub.

Every project-aware tool accepts an optional `project_root` (absolute
path). When omitted, the path is resolved from the server's CWD by
walking up for the first `Cargo.toml` with a `dioxus` dependency.

## Install

Build the server binary out of the workspace:

```
cargo build --release -p dioxus-mcp
```

The binary lands at `target/release/dioxus-mcp` (workspace target dir).
Register it with Claude Code:

```
claude mcp add dioxus /absolute/path/to/dioxus-mcp/target/release/dioxus-mcp -s user
```

Or install it onto your `$PATH` so the registered path doesn't change
across rebuilds:

```
cargo install --path crates/dioxus-mcp
claude mcp add dioxus dioxus-mcp -s user
```

Restart Claude Code; `/mcp` should list `dioxus`.

To remove: `claude mcp remove dioxus -s user`.
After a rebuild, restart Claude Code to pick up the new binary.

## Usage notes

- Launch Claude Code from the Dioxus project root (or any subdirectory).
  Tools walk up to find the Cargo.toml; project_root only needs to be
  passed when calling tools against a project other than the CWD.
- All tools operate on the source AST via `syn`. They do not invoke
  `cargo`, `dx`, or any subprocess.
- Targets Dioxus 0.7. Older versions will run but the audit/lints
  reflect 0.7 conventions.

## Runtime probe

The `dioxus-mcp-probe` workspace member is a tiny runtime companion. Add
it to your Dioxus app and call `install()` once at startup; it spins up a
background thread that writes a JSON-lines event log to
`target/dioxus-mcp/events.jsonl`. The MCP `runtime_events` tool tails
that file.

```toml
# in your Dioxus app's Cargo.toml
[dev-dependencies]
dioxus-mcp-probe = { git = "https://github.com/tonybierman/dioxus-mcp", tag = "probe-v0.1.2" }
```

```rust
fn main() {
    let _probe = dioxus_mcp_probe::install();
    // your dioxus app entry point
}
```

The probe is a no-op outside `debug_assertions` (override with the
`force` cargo feature). Capture is best-effort: a bounded queue drops
events under load rather than blocking renders, and the log file rotates
at 10 MiB.

## Tests

```
cargo test --workspace        # 18 main tests + 4 probe unit tests
cargo test -- --ignored       # also runs live-HTTP search_docs / find_example
```

Tests spawn the binary over stdio and assert on each tool's response
against `crates/dioxus-mcp/tests/fixtures/sample-project/` — a hand-crafted Dioxus
source tree where every file's purpose is to trigger meaningful
output from one or more tools. Headers in each fixture file name
which tool(s) it exercises.

## Transports

- `stdio` (default) — for direct MCP clients like Claude Code.
- `--transport http --bind 127.0.0.1:8731` — streamable HTTP, for IDEs
  or remote clients.

## Configuration flags

```
--transport stdio|http     transport (default: stdio)
--bind HOST:PORT           HTTP bind address (default: 127.0.0.1:8731)
--project-root PATH        pin a project root (default: CWD)
--log LEVEL                tracing filter (default: info)
```
