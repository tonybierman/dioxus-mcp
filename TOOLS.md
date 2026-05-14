# dioxus-mcp tools

Every tool returns pretty-printed JSON. Every project-aware tool accepts an
optional `project_root` (absolute path); when omitted, the project is
detected by walking up from the server's CWD to the first `Cargo.toml`
with a `dioxus` dependency.

The "Ask Claude" lines are example natural-language prompts; Claude Code
will pick the matching tool automatically. The "Demonstrated in" links
point at the fixture file the integration tests exercise for each tool
(see `tests/fixtures/sample-project/` and `tests/integration.rs`).

---

## Project introspection

### `project_tour`
**Purpose:** One-shot project overview — feature audit, route map,
component/server-fn index, and asset audit, plus a pre-rendered markdown
summary you can drop straight into a prompt or PR description.

**Args:** `include?`, `exclude?` (subset of `["audit","routes","index","assets"]`),
`max_items_per_section?` (default 50), `project_root?`.

**Example call:**
```json
{"name": "project_tour", "arguments": {}}
```

**Ask Claude:** "Give me a tour of this codebase."

**Demonstrated in:** the whole `tests/fixtures/sample-project/` tree.

---

### `route_map`
**Purpose:** List every route in the `#[derive(Routable)]` enum with raw
path, nest-prefixed `full_path`, target component, typed params, and the
`layouts` / `nests` stacks each route sits under.

**Args:** `router_file?` (relative to crate root; auto-detected by default),
`project_root?`.

**Example call:**
```json
{"name": "route_map", "arguments": {}}
```

**Ask Claude:** "Show me all the URL routes and which component renders each."

**Demonstrated in:** [`src/router.rs`](tests/fixtures/sample-project/src/router.rs) — `Route` enum with `#[layout]`, `#[nest]`, and typed params.

---

### `project_index`
**Purpose:** Index every `#[component]` and `#[server]` fn with file:line,
typed signature (props/args with optional flag), and the unwrapped return
type for server fns (`ServerFnResult<T>` → `T`).

**Args:** `kind?` (`"component"` or `"server_fn"` to filter), `path?` (subdir
to scan, default `src/`), `project_root?`.

**Example call:**
```json
{"name": "project_index", "arguments": {}}
```

**Ask Claude:** "What server functions exist in this project?"

**Demonstrated in:** [`src/components/home.rs`](tests/fixtures/sample-project/src/components/home.rs) and [`src/server/fetch_user.rs`](tests/fixtures/sample-project/src/server/fetch_user.rs) — Props-struct component and a server fn with typed args.

---

### `server_fn_call_graph`
**Purpose:** For every `#[server]` fn, list every call site
(`caller_file`, `caller_line`, `enclosing_fn`, `full_path`). Server fns
with zero callers are returned under `orphans`.

**Args:** `project_root?`.

**Example call:**
```json
{"name": "server_fn_call_graph", "arguments": {}}
```

**Ask Claude:** "Are any server functions never called? Show the call graph."

**Demonstrated in:** [`src/components/user_page.rs`](tests/fixtures/sample-project/src/components/user_page.rs) (calls `fetch_user`) and [`src/server/orphan_fn.rs`](tests/fixtures/sample-project/src/server/orphan_fn.rs) (never called).

---

### `dead_components`
**Purpose:** Components defined but never used in any `rsx!` invocation.
`App` plus every component referenced from the Routable enum (route
targets + layouts) are treated as roots.

**Args:** `roots?` (extra component names to treat as alive), `project_root?`.

**Example call:**
```json
{"name": "dead_components", "arguments": {"roots": ["RootLayout"]}}
```

**Ask Claude:** "Find components that are defined but never rendered."

**Demonstrated in:** [`src/components/unused.rs`](tests/fixtures/sample-project/src/components/unused.rs) — defined but referenced nowhere in any `rsx!`.

---

### `asset_audit`
**Purpose:** Cross-references files under your assets dir(s) with every
`asset!("...")` macro call. Reports unreferenced files, broken references
(`asset!()` paths that don't exist on disk), and a count of dynamic
(non-string-literal) calls that were skipped.

**Args:** `assets_dirs?` (default `["assets"]`), `project_root?`.

**Example call:**
```json
{"name": "asset_audit", "arguments": {"assets_dirs": ["assets", "public"]}}
```

**Ask Claude:** "Which files in assets/ are no longer used?"

**Demonstrated in:** [`assets/`](tests/fixtures/sample-project/assets/) (with `orphan.css` unreferenced) and [`src/main.rs`](tests/fixtures/sample-project/src/main.rs) (referencing a missing `missing.svg`).

---

## Lints

### `check_rsx`
**Purpose:** Lint a single Rust file's `rsx!` blocks for common 0.7
mistakes — loops without a `key:` attribute, event handlers with no
closure parameters.

**Args:** `file` (required, relative to crate root or absolute), `project_root?`.

**Example call:**
```json
{"name": "check_rsx", "arguments": {"file": "src/lint_demo.rs"}}
```

**Ask Claude:** "Lint the rsx in `src/lint_demo.rs`."

**Demonstrated in:** [`src/lint_demo.rs`](tests/fixtures/sample-project/src/lint_demo.rs) — `for` loop without `key:` and an `onclick: move || {}` with no event arg.

---

### `signal_lint`
**Purpose:** Flag `use_signal` / `use_memo` / `use_resource` /
`use_effect` calls inside `for` / `while` / `loop` bodies, including
loops nested inside `rsx!` macro bodies. Each iteration would otherwise
allocate a new hook.

**Args:** `project_root?`.

**Example call:**
```json
{"name": "signal_lint", "arguments": {}}
```

**Ask Claude:** "Are there any hooks being called inside loops?"

**Demonstrated in:** [`src/components/home.rs`](tests/fixtures/sample-project/src/components/home.rs) — `use_signal` inside an rsx! `for` loop.

---

### `props_lint`
**Purpose:** Flag `#[derive(Props, ...)]` structs that don't also derive
`PartialEq` — Dioxus needs `PartialEq` on Props for memoization to work.

**Args:** `project_root?`.

**Example call:**
```json
{"name": "props_lint", "arguments": {}}
```

**Ask Claude:** "Check my Props structs for missing PartialEq."

**Demonstrated in:** [`src/components/child.rs`](tests/fixtures/sample-project/src/components/child.rs) — `ChildProps` derives `Props, Clone` but not `PartialEq`.

---

### `prop_drill`
**Purpose:** Detect props passed unchanged from a parent component into a
child. Matches bare ident, `prop`, `prop.clone()`, `prop.into()`,
`prop.to_owned()`, `prop.read()`, `prop.peek()`, `prop.cloned()` — and
the `props.NAME` equivalents for Props-struct components. Each finding
is tagged with the matched form via the `via` field.

**Args:** `project_root?`.

**Example call:**
```json
{"name": "prop_drill", "arguments": {}}
```

**Ask Claude:** "Where am I prop-drilling? I should probably use context."

**Demonstrated in:** [`src/components/home.rs`](tests/fixtures/sample-project/src/components/home.rs) — `Child { name: props.title.clone(), user_id: props.user_id }`.

---

### `audit_feature_flags`
**Purpose:** Cargo.toml platform-feature sanity. Flags conflicting render
targets (web + desktop without fullstack), broken fullstack wiring
(missing `server` or `web`), and `[features] default = ["web","server"]`
footguns. Also confirms the detected Dioxus version.

**Args:** `project_root?`.

**Example call:**
```json
{"name": "audit_feature_flags", "arguments": {}}
```

**Ask Claude:** "Is my Cargo.toml dioxus configuration right?"

**Demonstrated in:** [`Cargo.toml`](tests/fixtures/sample-project/Cargo.toml) — clean `fullstack + web + server` setup.

---

### `explain_signal_graph`
**Purpose:** Inside a single file, list every `use_signal` /
`use_memo` / `use_resource` / `use_effect` binding inside each
`#[component]`, and which signals each one reads. Flags memos/effects
that capture no other signals (they'll never re-run on state change).

**Args:** `file` (required), `component?` (filter to one), `project_root?`.

**Example call:**
```json
{"name": "explain_signal_graph", "arguments": {"file": "src/components/home.rs"}}
```

**Ask Claude:** "Walk me through the reactive graph in home.rs."

**Demonstrated in:** [`src/components/home.rs`](tests/fixtures/sample-project/src/components/home.rs) — `use_signal` plus a `use_memo` that reads it.

---

## Scaffolding

### `create_component`
**Purpose:** Generate a new `#[component]` file under `src/components/`
(or a custom path) with optional typed `Props`. Wires it into
`components/mod.rs`.

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

**Ask Claude:** "Create a UserCard component that takes an id and an optional label."

**Demonstrated in:** `tool_create_component` in [`tests/integration.rs`](tests/integration.rs) — runs against a tempdir copy of the fixture.

---

### `create_route`
**Purpose:** Insert a new variant into the project's `#[derive(Routable)]`
enum, wiring a URL pattern to a component.

**Args:** `path` (required, e.g. `/users/:id`), `component` (required,
PascalCase), `router_file?` (auto-detected), `project_root?`.

**Example call:**
```json
{
  "name": "create_route",
  "arguments": {"path": "/settings", "component": "Settings"}
}
```

**Ask Claude:** "Add a route at /settings rendering Settings."

**Demonstrated in:** `tool_create_route` in [`tests/integration.rs`](tests/integration.rs) — inserts a variant into the fixture's `Route` enum.

---

### `create_server_fn`
**Purpose:** Generate a new `#[server]` fn under `src/server/`. Refuses
if the project isn't fullstack-capable (lacks `fullstack` or
`web`+`server` on the dioxus dep) — run `audit_feature_flags` first if it
errors.

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

**Ask Claude:** "Add a server function fetch_users(limit: u32) returning Vec<User>."

**Demonstrated in:** `tool_create_server_fn` in [`tests/integration.rs`](tests/integration.rs) — generates a new file under `src/server/` of the fixture.

---

## Docs

### `search_docs`
**Purpose:** Live full-text search of dioxuslabs.com, scoped to the
project's detected Dioxus version. Returns ranked snippets with URLs.
Cached for 15 minutes.

**Args:** `query` (required), `version?` (e.g. `"0.7"`), `limit?`
(default 5).

**Example call:**
```json
{"name": "search_docs", "arguments": {"query": "use_resource"}}
```

**Ask Claude:** "Find the Dioxus docs on use_resource."

**Demonstrated in:** `tool_search_docs` in [`tests/integration.rs`](tests/integration.rs) — `#[ignore]`d offline; run with `cargo test -- --ignored`.

---

### `find_example`
**Purpose:** Search the official Dioxus examples on GitHub for a concept
or API. Returns matching files with raw URLs and short excerpts.

**Args:** `concept` (required), `ref?` (branch/tag, default `"main"`),
`limit?`.

**Example call:**
```json
{"name": "find_example", "arguments": {"concept": "fullstack"}}
```

**Ask Claude:** "Show me an official Dioxus example using fullstack."

**Demonstrated in:** `tool_find_example` in [`tests/integration.rs`](tests/integration.rs) — `#[ignore]`d offline; run with `cargo test -- --ignored`.
