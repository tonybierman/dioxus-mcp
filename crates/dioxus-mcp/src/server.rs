use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    tool, tool_handler, tool_router,
};

use crate::state::State;
use crate::tools;

#[derive(Clone)]
pub struct DioxusMcp {
    pub state: Arc<State>,
    #[allow(dead_code)]
    tool_router: ToolRouter<DioxusMcp>,
}

#[tool_router]
impl DioxusMcp {
    pub fn new(state: Arc<State>) -> Self {
        Self {
            state,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Audit Cargo.toml + dioxus.toml for misconfigurations (conflicting platform features, fullstack mis-wiring, version mismatches)."
    )]
    async fn audit_feature_flags(
        &self,
        Parameters(p): Parameters<tools::audit_feature_flags::AuditFeatureFlagsParams>,
    ) -> Result<CallToolResult, McpError> {
        let report = tools::audit_feature_flags::audit_feature_flags(&self.state, p).await;
        ok_json(&report)
    }

    #[tool(
        description = "Lint Rust file(s)' rsx! blocks for common 0.7 mistakes (missing keys on iterators, parameter-less event handlers, attribute writes that trigger E0034 ambiguity — e.g. `autofocus: true` on `input`/`button`/`textarea`/`select`). The response includes a `checks_run` list naming the lints that fired so a clean `issues: []` is distinguishable from an empty/skipped scan. Pass `file` for a single file (single-file response shape: `file`, `rsx_block_count`, `checks_run`, `issues`). Pass `files: [...]` for batch mode (adds `per_file: [...]`; top-level `issues` is the flat merge with each issue tagged by file)."
    )]
    async fn check_rsx(
        &self,
        Parameters(p): Parameters<tools::check_rsx::CheckRsxParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::check_rsx::check_rsx(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Server-fn call graph: for every #[server] fn, list every call site (caller_file, caller_line, enclosing_fn) and emit an orphan list of server fns nobody calls. Cross-crate callers not detected."
    )]
    async fn server_fn_call_graph(
        &self,
        Parameters(p): Parameters<tools::server_fn_call_graph::ServerFnCallGraphParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::server_fn_call_graph::server_fn_call_graph(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Audit assets/: list files under the assets dir(s) not referenced by any `asset!(\"...\")` macro, and `asset!()` references to files that don't exist on disk. Dynamic (non-string-literal) args are counted but skipped."
    )]
    async fn asset_audit(
        &self,
        Parameters(p): Parameters<tools::asset_audit::AssetAuditParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::asset_audit::asset_audit(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "List components defined but never used in any rsx! block. Components reachable from the Routable enum (route targets + layouts) plus `App` are treated as roots."
    )]
    async fn dead_components(
        &self,
        Parameters(p): Parameters<tools::dead_components::DeadComponentsParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::dead_components::dead_components(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Find props passed unchanged from a parent component into a child (drilling). Matches bare ident and one-level wrappers `.clone()`, `.into()`, `.to_owned()`, `.read()`, `.peek()`, `.cloned()`; each finding tagged with a `via` field."
    )]
    async fn prop_drill(
        &self,
        Parameters(p): Parameters<tools::prop_drill::PropDrillParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::prop_drill::prop_drill(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Lint signals: flag `use_signal` / `use_memo` / `use_resource` / `use_effect` calls inside `for` / `while` / `loop` bodies in component fns — a new hook is created on every iteration."
    )]
    async fn signal_lint(
        &self,
        Parameters(p): Parameters<tools::signal_lint::SignalLintParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::signal_lint::signal_lint(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Lint Props structs: flag `#[derive(Props, ...)]` structs that don't also derive `PartialEq`. Dioxus needs PartialEq on Props for memoization."
    )]
    async fn props_lint(
        &self,
        Parameters(p): Parameters<tools::props_lint::PropsLintParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::props_lint::props_lint(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Run every project-wide lint (`check_rsx`, `dead_components`, `prop_drill`, `signal_lint`, `props_lint`) over the crate's `src/` tree and merge the results. Returns a markdown summary, per-lint issue counts (`issues_by_lint`), the raw report from each lint under its name, deduplicated `parse_errors`, and a `total_issues` count. Use `include` / `exclude` to scope (e.g. `include: [\"check_rsx\", \"signal_lint\"]`), and `dead_component_roots` to mark extra components alive."
    )]
    async fn lint_project(
        &self,
        Parameters(p): Parameters<tools::lint_project::LintProjectParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::lint_project::lint_project(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "One-shot project tour: feature-flag audit + route map + component/server-fn index + asset audit, plus a pre-rendered markdown summary. Use `include`/`exclude` to scope, `max_items_per_section` to cap output."
    )]
    async fn project_tour(
        &self,
        Parameters(p): Parameters<tools::project_tour::ProjectTourParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::project_tour::project_tour(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Index every #[component] and #[server] function in the crate. Returns each symbol's name, file:line, signature (props/args + types, optional flag), and for server fns the unwrapped ServerFnResult<T> return type."
    )]
    async fn project_index(
        &self,
        Parameters(p): Parameters<tools::project_index::ProjectIndexParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::project_index::project_index(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "List every route in the project's #[derive(Routable)] enum: URL path (raw + nest-prefixed), target component, params, and any #[layout(...)] / #[nest(...)] it's nested under."
    )]
    async fn route_map(
        &self,
        Parameters(p): Parameters<tools::route_map::RouteMapParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::route_map::route_map(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Explain the reactive graph of a Dioxus component: which use_signal / use_memo / use_resource / use_effect bindings exist and which signals each one reads."
    )]
    async fn explain_signal_graph(
        &self,
        Parameters(p): Parameters<tools::explain_signal_graph::ExplainSignalGraphParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::explain_signal_graph::explain_signal_graph(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    // create_component / create_route / create_server_fn used to be exposed
    // as MCP tools but were almost always the wrong call — `get_dsl_spec` +
    // `execute_code` are the supported scaffold path. Removed from the tool
    // surface; the underlying `tools::scaffold::*` functions are still used
    // by `execute_code` internally to materialize each DSL primitive.

    #[tool(
        description = "Call this BEFORE `execute_code` whenever the user asks to build, scaffold, add, or create anything in a Dioxus 0.7 project — a model, a screen, a server fn, a full CRUD slice, or a whole app. Returns the YAML DSL vocabulary used by `execute_code`. Pass `extensions: [\"crud\", \"realtime\", \"auth\"]` to include extra primitive groups; empty / omitted returns core only (Model, Store, ClientStore, Resource, Component, Screen, ServerFn). Each primitive lists its fields and a runnable example. The Resource primitive expands into a model+store+server-fn+screens slice in one entry — prefer it for server-backed features. ClientStore + Screen `kind: client_crud` covers client-only in-memory state with no server fn round-trip."
    )]
    async fn get_dsl_spec(
        &self,
        Parameters(p): Parameters<tools::dsl::GetDslSpecParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::dsl::get_dsl_spec(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "List the official Dioxus 0.7 component catalog (45 widgets installable via `dx components add <name>`). Returns each entry's snake_case name, one-line description, and `use crate::components::...;` import path. Pass `query` to filter by case-insensitive substring match against name OR description (e.g. `query: \"date\"` returns calendar + date_picker). Cheaper than calling `get_dsl_spec { sections: [components] }` when you just want to pick a widget; the spec section wraps the same catalog in authoring guidance you don't need here."
    )]
    async fn list_components(
        &self,
        Parameters(p): Parameters<tools::dsl::ListComponentsParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::dsl::list_components(p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Map a user prompt to catalog widgets. Pass the user's verbatim ask as `prompt` — the matcher scans for UI-primitive keywords (drag/dialog/combobox/calendar/toast/menu/tabs/etc.) and returns the canonical Dioxus 0.7 catalog entries that cover the request. Use this BEFORE writing event handlers for anything that looks like a UI primitive: a positive hit avoids hand-rolling drag listeners, modal trap-focus, autocomplete logic, etc. Empty `components` means no keywords matched — fall back to `list_components`."
    )]
    async fn suggest_components(
        &self,
        Parameters(p): Parameters<tools::dsl::SuggestComponentsParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::dsl::suggest_components(p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Verify a project's `dx components add` wiring. Reports which one-time setup steps are still missing (`mod components;` in src/main.rs or src/lib.rs, `asset!(\"/assets/dx-components-theme.css\")` mounted in the App, `src/components/` directory present). Returns `fully_wired: bool`, a `missing: [step_id]` summary, and a `steps: [...]` list with each step's `ok`, the paths it looked at (`looked_in`), and the exact fix line + paste location when `ok: false`. Use this after `dx components add` (or after the user reports compile errors about an unresolved `crate::components` path) to finish wiring without re-running the CLI."
    )]
    async fn verify_install(
        &self,
        Parameters(p): Parameters<tools::dsl::VerifyInstallParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::dsl::verify_install(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Full prop / event surface for a Dioxus 0.7 catalog component (the data needed to author rsx! against it). Returns the component fn signature, every prop (name, type, optional?, has_default, default expression, extends targets, doc comment), every variant enum (e.g. ButtonVariant + its #[default]), aggregated `extends` and `event_handlers` lists, plus `ambiguous_attributes` (E0034 setters that need the literal-string form) and `referenced_enums` (variants for enum types referenced inside any prop type, e.g. `CheckboxState`). When the wrapper just forwards `props: SomeProps` the primitive's props are promoted to the top-level `props` list and `props_source: \"primitive\"` is set so the first read isn't misleadingly empty. Reads from the upstream cargo git checkout (~/.cargo/git/checkouts/components-*) when available, otherwise falls back to the project-local install at `src/components/<name>/component.rs`. Call this BEFORE writing rsx! that uses a catalog widget — saves 5+ file reads per widget."
    )]
    async fn describe_component(
        &self,
        Parameters(p): Parameters<tools::dsl::DescribeComponentParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::dsl::describe_component(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Use this whenever the user asks to build, scaffold, add, or create anything in a Dioxus 0.7 project — a model, a screen, a server fn, a full CRUD slice, or a whole app. Materializes a file set from a single YAML DSL doc (see `get_dsl_spec`). Pre-flights name collisions across the whole doc; rejects unknown fields, multi-document YAML, and missing cross-refs (List/Table → ServerFn, Feed → Socket). On success returns the merged ScaffoldResult with files_created, files_modified, next_steps, and (when applicable) collisions. \
\
Flags: pass `dry_run: true` to compute a plan (`would_create` / `would_modify`) without writing anything. Pass `if_missing: true` to skip primitives whose target leaf file already exists (reported in `collisions`) instead of erroring — makes re-runs during iteration safe."
    )]
    async fn execute_code(
        &self,
        Parameters(p): Parameters<tools::dsl::ExecuteCodeParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::dsl::execute_code(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Live-search dioxuslabs.com docs (scoped to the project's Dioxus version) and return ranked snippets. 15-min cache."
    )]
    async fn search_docs(
        &self,
        Parameters(p): Parameters<tools::search_docs::SearchDocsParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::search_docs::search_docs(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Find Dioxus examples — official ones in DioxusLabs/dioxus on GitHub, merged with a small local registry of pattern examples that the upstream repo doesn't ship a folder for (e.g. `optimistic-with-reconcile`). Pass `concept` to rank by name + blurb match ('router', 'fullstack', 'use_signal'); omit it for an alphabetically-sorted listing of every example. Each hit carries `kind: \"upstream\"` (browsable via `url` / `raw_url`) or `kind: \"local\"` (inline `body:` field with paste-ready Rust source; no follow-up fetch needed). `limit` defaults to 3 with a concept, 100 without."
    )]
    async fn find_example(
        &self,
        Parameters(p): Parameters<tools::find_example::FindExampleParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::find_example::find_example(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Generate an OpenAPI 3.1 spec from #[server] functions (POST endpoints) and, optionally, router routes (GET). Schemas for arg/return types are walked from local #[derive(Serialize)] / #[derive(Deserialize)] structs and enums; unresolved type names are reported. Defaults: server_fn_prefix=\"/api\", include_routes=false."
    )]
    async fn openapi_spec(
        &self,
        Parameters(p): Parameters<tools::openapi_spec::OpenapiSpecParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::openapi_spec::openapi_spec(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Read runtime events captured by the dioxus-mcp-probe crate. Tails target/dioxus-mcp/events.jsonl and returns events matching the filters: kind (render | signal_write | signal_read | server_fn | route | panic | event), since (RFC 3339 cutoff, default last 5 min), component, signal, server_fn, limit (default 200, hard cap 2000). Returns an empty list with a clear note if the probe hasn't been installed yet. \
\
USE THIS (don't ask the user to paste logs) when they ask things like: \"Was there a panic? Where did it happen?\", \"Did the app crash?\", \"Which signals wrote in the past minute?\", \"Show the last few renders of <Component>\", \"List server-fn calls for <name>\", \"What navigations happened?\", \"Tail the runtime log\". \
\
If the user references \"the last run\" or a specific log file, pass `log_path` and widen `since` (default cutoff is only 5 min back). On \"no Cargo.toml from project root\", set `project_root` to the actual Dioxus app directory."
    )]
    async fn runtime_events(
        &self,
        Parameters(p): Parameters<tools::runtime_events::RuntimeEventsParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::runtime_events::runtime_events(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Per-server-fn latency summary derived from the dioxus-mcp-probe log. Pairs phase=start with phase=end by call_id and returns count, ok/err, and min/p50/p95/max latency in microseconds for each #[server] fn called in the window. Filters: since (RFC 3339, default last 5 min), server_fn (one name only), log_path (override). \
\
USE THIS when the user asks: \"What's the latency distribution for <fn>?\", \"Which server fns are slowest?\", \"Are any server fns erroring?\", \"How many <fn> calls ran and how many failed?\", \"What's still pending mid-flight?\", \"Summary of server-fn activity over the last N minutes\", \"Show p95 latency for every server fn\"."
    )]
    async fn server_fn_summary(
        &self,
        Parameters(p): Parameters<tools::server_fn_summary::ServerFnSummaryParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::server_fn_summary::server_fn_summary(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Run `cargo check` against the Dioxus project with a structured diagnostic shape — the closing-the-loop step after `execute_code`. Auto-picks a sensible feature combo (no extras when `fullstack` is already on the dep, `server` for the canonical 0.7 `default=[\"web\"]` + opt-in `server` sibling, `web,server` for older layouts) or accepts an explicit `features:` list. Set `target_wasm: true` to also catch client-only errors via `--target wasm32-unknown-unknown`. Parses `--message-format=json` and returns separate `errors` / `warnings` lists with file/line/column/code + cargo's pre-rendered diagnostic text; both lists are capped via `max_messages` (default 20), and `truncated: true` signals when caps fired. `status` is one of `passed | failed | timed_out | spawn_failed`. Default timeout 300s (override with `timeout_secs`). Does NOT shell out to `dx serve`; this is a static compile check, not an end-to-end serve probe."
    )]
    async fn build_and_smoke(
        &self,
        Parameters(p): Parameters<tools::build_and_smoke::BuildAndSmokeParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::build_and_smoke::build_and_smoke(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }
}

#[tool_handler]
impl ServerHandler for DioxusMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions(
                "Dioxus 0.7 project assistant. When a question maps to a tool, call it.\n\
                 \n\
                 Routing:\n\
                 - Scaffold STRUCTURED slices (model / store / server-fn-backed Resource / \
                   client_crud / whole-app skeleton): `get_dsl_spec` then `execute_code`. \
                   Use `Resource` for a full server-backed slice; `ClientStore` + \
                   `kind: client_crud` for in-memory state. For one-off handwritten screens \
                   or single-component edits, skip the DSL and write the file directly — \
                   `execute_code` is for multi-file, cross-wired primitives, not ad-hoc UI.\n\
                 - UI primitive widgets (button / dialog / date-picker / drag-to-reorder / \
                   combobox / toast / etc.): BEFORE writing any handler code, \
                   `list_components` (or `suggest_components { prompt: \"...\" }` with the \
                   user's verbatim ask) to scan the catalog, then `dx components add <name>` \
                   from the project root. Call `describe_component` for the full prop / \
                   event surface before authoring rsx! that uses it. If you find yourself \
                   hand-rolling event listeners for a UI primitive that the catalog likely \
                   covers (drag, sortable, autocomplete, calendar, modal, toast), stop and \
                   check the catalog first.\n\
                 - Runtime behavior (panics, renders, signal writes, navigations) -> \
                   runtime_events. Server-fn latency / errors -> server_fn_summary.\n\
                 - Project structure (what routes / components / server fns exist) -> \
                   route_map, project_index, project_tour, server_fn_call_graph.\n\
                 - Static analysis (dead code, prop drilling, signal/props lints, asset \
                   audit, feature flags, OpenAPI) -> dead_components, prop_drill, \
                   signal_lint, props_lint, asset_audit, audit_feature_flags, openapi_spec, \
                   explain_signal_graph, lint_project.\n\
                 - Docs / canonical examples -> search_docs, find_example. RSX check -> \
                   check_rsx. Catalog widget prop / event surface -> describe_component.\n\
                 \n\
                 Probe note: runtime_events + server_fn_summary read a JSONL log written \
                 by the dioxus-mcp-probe crate. Pass `project_root` when cwd isn't the app; \
                 widen `since` (default 5 min) for older runs."
                    .to_string(),
            )
    }
}

pub fn ok_json<T: serde::Serialize>(value: &T) -> Result<CallToolResult, McpError> {
    let s = serde_json::to_string_pretty(value)
        .map_err(|e| McpError::internal_error(format!("serialize: {e}"), None))?;
    Ok(CallToolResult::success(vec![Content::text(s)]))
}

pub fn err(msg: impl Into<String>) -> McpError {
    McpError::invalid_request(msg.into(), None)
}
