# dioxus-mcp tools

Every tool returns pretty-printed JSON. The "Ask Claude" lines are example
natural-language prompts; Claude Code will pick the matching tool
automatically.

Per-tool argument schemas, example JSON-RPC calls, and the fixtures each
tool is exercised against live in [TOOLS_REFERENCE](TOOLS_REFERENCE.md).

---

## Project introspection

### `project_tour`
**Purpose:** One-shot project overview combining the audit, route map, component/server-fn index, and asset audit into a single response.

**Ask Claude:** "Give me a tour of this codebase."

---

### `route_map`
**Purpose:** List every route in the project's `#[derive(Routable)]` enum, with nest/layout context.

**Ask Claude:** "Show me all the URL routes and which component renders each."

---

### `project_index`
**Purpose:** Index every `#[component]` and `#[server]` fn with file:line and typed signatures.

**Ask Claude:** "What server functions exist in this project?"

---

### `server_fn_call_graph`
**Purpose:** For every `#[server]` fn, list every call site, and flag the ones nothing calls.

**Ask Claude:** "Are any server functions never called? Show the call graph."

---

### `dead_components`
**Purpose:** Components defined but never used in any `rsx!` invocation.

**Ask Claude:** "Find components that are defined but never rendered."

---

### `asset_audit`
**Purpose:** Cross-reference files under your assets dir(s) with every `asset!()` call; flag unreferenced files and broken paths.

**Ask Claude:** "Which files in assets/ are no longer used?"

---

### `openapi_spec`
**Purpose:** Generate an OpenAPI 3.1 document from the project's `#[server]` fns (and optionally router routes).

**Ask Claude:** "Generate an OpenAPI spec for the server functions in this project."

---

## Lints

### `check_rsx`
**Purpose:** Lint a single Rust file's `rsx!` blocks for common 0.7 mistakes.

**Ask Claude:** "Lint the rsx in `src/lint_demo.rs`."

---

### `signal_lint`
**Purpose:** Flag hook calls (`use_signal`, `use_memo`, `use_resource`, `use_effect`) inside loops — each iteration would otherwise allocate a new hook.

**Ask Claude:** "Are there any hooks being called inside loops?"

---

### `props_lint`
**Purpose:** Flag `#[derive(Props, ...)]` structs that don't also derive `PartialEq` — without it, Dioxus can't memoize the component.

**Ask Claude:** "Check my Props structs for missing PartialEq."

---

### `prop_drill`
**Purpose:** Detect props passed unchanged from a parent component into a child — a hint that context might be a better fit.

**Ask Claude:** "Where am I prop-drilling? I should probably use context."

---

### `audit_feature_flags`
**Purpose:** Cargo.toml sanity check for the `dioxus` dependency's platform features.

**Ask Claude:** "Is my Cargo.toml dioxus configuration right?"

---

### `explain_signal_graph`
**Purpose:** Inside a single file, list each component's reactive bindings and what signals they read.

**Ask Claude:** "Walk me through the reactive graph in home.rs."

---

## Scaffolding

### `create_component`
**Purpose:** Generate a new `#[component]` file with optional typed `Props`.

**Ask Claude:** "Create a UserCard component that takes an id and an optional label."

---

### `create_route`
**Purpose:** Insert a new variant into the project's `#[derive(Routable)]` enum, wiring a URL pattern to a component.

**Ask Claude:** "Add a route at /settings rendering Settings."

---

### `create_server_fn`
**Purpose:** Generate a new `#[server]` fn under `src/server/`.

**Ask Claude:** "Add a server function fetch_users(limit: u32) returning Vec<User>."

---

## Runtime

Runtime tools read events captured by the `dioxus-mcp-probe` crate while
the app is running. They live in [RUNTIME_TOOLS](RUNTIME_TOOLS.md):

- `runtime_events` — filter the raw JSONL event log.
- `server_fn_summary` — per-server-fn latency stats (count, ok/err, p50/p95).

---

## Docs

### `search_docs`
**Purpose:** Live full-text search of dioxuslabs.com, scoped to the project's detected Dioxus version.

**Ask Claude:** "Find the Dioxus docs on use_resource."

---

### `find_example`
**Purpose:** Search the official Dioxus examples on GitHub for a concept or API.

**Ask Claude:** "Show me an official Dioxus example using fullstack."
