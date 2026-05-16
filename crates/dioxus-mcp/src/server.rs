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
        Parameters(p): Parameters<tools::analysis::AuditFeatureFlagsParams>,
    ) -> Result<CallToolResult, McpError> {
        let report = tools::analysis::audit_feature_flags(&self.state, p).await;
        ok_json(&report)
    }

    #[tool(
        description = "Lint Rust file(s)' rsx! blocks for common 0.7 mistakes (missing keys on iterators, parameter-less event handlers). Pass `file` for a single file (single-file response shape: `file`, `rsx_block_count`, `issues`). Pass `files: [...]` for batch mode (adds `per_file: [...]`; top-level `issues` is the flat merge with each issue tagged by file)."
    )]
    async fn check_rsx(
        &self,
        Parameters(p): Parameters<tools::analysis::CheckRsxParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::analysis::check_rsx(&self.state, p).await {
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
        Parameters(p): Parameters<tools::analysis::ExplainSignalGraphParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::analysis::explain_signal_graph(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Generate a new Dioxus component file with optional typed Props, register it in the components mod tree."
    )]
    async fn create_component(
        &self,
        Parameters(p): Parameters<tools::scaffold::CreateComponentParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::scaffold::create_component(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Insert a new variant into the project's #[derive(Routable)] enum, wiring a path to a component."
    )]
    async fn create_route(
        &self,
        Parameters(p): Parameters<tools::scaffold::CreateRouteParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::scaffold::create_route(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Generate a Dioxus 0.7 server function (Axum-backed) with the right feature gating. Refuses if the project isn't fullstack-capable."
    )]
    async fn create_server_fn(
        &self,
        Parameters(p): Parameters<tools::scaffold::CreateServerFnParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::scaffold::create_server_fn(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Call this BEFORE `execute_code` whenever the user asks to build, scaffold, add, or create anything in a Dioxus 0.7 project — a model, a screen, a server fn, a full CRUD slice, or a whole app. Returns the YAML DSL vocabulary used by `execute_code`. Pass `extensions: [\"crud\", \"realtime\", \"auth\"]` to include extra primitive groups; empty / omitted returns core only (Model, Store, Resource, Component, Screen, ServerFn). Each primitive lists its fields and a runnable example. The Resource primitive expands into a model+store+server-fn+screens slice in one entry — prefer it for new CRUD features."
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
        Parameters(p): Parameters<tools::docs::SearchDocsParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::docs::search_docs(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(
        description = "Find official Dioxus examples on GitHub. Pass `concept` to rank by name match (e.g. 'router', 'fullstack', 'use_signal'); omit it for an alphabetically-sorted listing of every example (useful when you don't yet know the folder name). `limit` defaults to 3 with a concept, 100 without."
    )]
    async fn find_example(
        &self,
        Parameters(p): Parameters<tools::docs::FindExampleParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::docs::find_example(&self.state, p).await {
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
}

#[tool_handler]
impl ServerHandler for DioxusMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions(
                "Dioxus project assistant for Dioxus 0.7 codebases. \
             \
             Routing rule: when the user's question maps onto one of these tools, \
             CALL THE TOOL instead of asking them to paste output or stack traces. \
             That is what these tools are for. \
             \
             SCAFFOLDING ANY NEW CODE — models, components, server fns, routes, screens, \
             stores, or full CRUD slices — ALWAYS start with `get_dsl_spec` then \
             `execute_code`. This applies to every prompt shaped like \"build X\", \
             \"add X\", \"scaffold X\", \"create a new model / screen / server fn\", \
             \"wire up CRUD for X\", \"make a feature for X\", or \"build me an app\". \
             The DSL handles single primitives just as well as full app slices; there is \
             no \"too small\" case. The `Resource` primitive emits a complete \
             model+store+server-fn+screens CRUD slice in one entry — prefer it for any \
             new resource. Per-primitive tools (`create_component`, `create_route`, \
             `create_server_fn`) exist for narrow agent workflows and are NOT the \
             default — prefer the DSL. \
             \
             Other routing: \
             - Runtime / behavior questions (panics, crashes, renders, signal writes, \
               navigations, \"what just happened\", \"was there a panic\") -> runtime_events. \
             - Server-fn latency, error rates, in-flight calls -> server_fn_summary. \
             - \"What routes / components / server fns exist\" -> route_map, project_index, \
               server_fn_call_graph, project_tour. \
             - Static code analysis (dead code, prop drilling, signal/props lints, \
               asset audit, feature flags, OpenAPI spec) -> dead_components, prop_drill, \
               signal_lint, props_lint, asset_audit, audit_feature_flags, openapi_spec, \
               explain_signal_graph. Whole-project lint pass -> lint_project. \
             - Reading Dioxus 0.7 docs / canonical examples -> search_docs, find_example. \
             - RSX correctness check -> check_rsx. \
             \
             Probe note: runtime_events and server_fn_summary read the JSONL log written \
             by the dioxus-mcp-probe crate. If the cwd isn't the Dioxus app, pass \
             `project_root`. If the user references \"the last run\" or a specific log, \
             pass `log_path` and widen `since` (default cutoff is 5 min)."
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
