# dioxus-mcp

[![CI](https://github.com/tonybierman/dioxus-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/tonybierman/dioxus-mcp/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

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
- **`lint_project`** — run every static lint (`check_rsx`,
  `dead_components`, `prop_drill`, `signal_lint`, `props_lint`) over
  `src/` and merge the results into a single response with a
  pre-rendered markdown summary. Scope via `include` / `exclude`.

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
  Props, registered in `components/mod.rs`. `template:` picks the body
  skeleton (`empty` | `form` | `list` | `crud_table` | `resource_view`,
  default `empty`).
- **`create_route`** — insert a variant into the existing
  `#[derive(Routable)]` enum.
- **`create_server_fn`** — new `#[server]` fn under `src/server/`,
  refuses if the project isn't fullstack-capable.
- **`get_dsl_spec`** — return the YAML DSL vocabulary used by
  `execute_code`. The core covers `models`, `stores`, `resources`,
  `components`, `screens`, `server_fns`, and `modify`, plus the
  primitives they compose on (forms, lists, tables, signals, sockets,
  feeds). Pass `extensions: ["crud", "realtime", "auth"]` to include
  extra primitive groups.
- **`execute_code`** — materialize a whole Dioxus 0.7 file set from one
  YAML doc: screens, components, forms, lists/tables, signals, sockets,
  feeds, login screens, protected routes, shared models, client-side
  stores, resource bundles (model + store + 5 server fns + list/new
  screens), idempotent `modify:` edits, and `remove:` operations that
  clear demo Routable variants / components from a `dx new` starter
  before adding your own. Pre-flights name and route-path collisions
  plus cross-references before any file is written. See
  [TOOLS_REFERENCE.md](TOOLS_REFERENCE.md#execute_code) for the
  data-layer-only path and the `client_crud` "learning aid, not
  production UI" caveat.

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

For the runtime probe install, transports, configuration flags, and the
test suite layout, see
[TOOLS_REFERENCE.md](TOOLS_REFERENCE.md#runtime-probe).

## Contributing

Contributions welcome. See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Copyright (c) 2026 Tony Bierman

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms
or conditions.
