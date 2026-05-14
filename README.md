# dioxus-mcp

An MCP server that gives Claude Code (and any other MCP client) deep static
understanding of a Dioxus 0.7 project: route maps, component/server-fn
indexes, dead-code detection, prop-drilling reports, signal/props lints,
asset audits, and scaffolding helpers — all from the source tree, with no
need to spawn `dx`.

## Tools

See [TOOLS.md](TOOLS.md) for each tool's args, a JSON call example, and a
natural-language prompt that Claude Code will route to it.

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

### Docs
- **`search_docs`** — live-search dioxuslabs.com, scoped to the project's
  Dioxus version, 15-min cached.
- **`find_example`** — search the official Dioxus examples on GitHub.

Every project-aware tool accepts an optional `project_root` (absolute
path). When omitted, the path is resolved from the server's CWD by
walking up for the first `Cargo.toml` with a `dioxus` dependency.

## Install

```
cargo build --release
```

Then register the binary with Claude Code:

```
claude mcp add dioxus /absolute/path/to/dioxus-mcp/target/release/dioxus-mcp -s user
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

## Tests

```
cargo test                    # 15 offline integration tests
cargo test -- --ignored       # also runs live-HTTP search_docs / find_example
```

Tests spawn the binary over stdio and assert on each tool's response
against `tests/fixtures/sample-project/` — a hand-crafted Dioxus
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
