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
        description = "Lint a Rust file's rsx! blocks for common 0.7 mistakes (missing keys on iterators, parameter-less event handlers)."
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

    #[tool(description = "Generate a new Dioxus component file with optional typed Props, register it in the components mod tree.")]
    async fn create_component(
        &self,
        Parameters(p): Parameters<tools::scaffold::CreateComponentParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::scaffold::create_component(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(description = "Insert a new variant into the project's #[derive(Routable)] enum, wiring a path to a component.")]
    async fn create_route(
        &self,
        Parameters(p): Parameters<tools::scaffold::CreateRouteParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::scaffold::create_route(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(description = "Generate a Dioxus 0.7 server function (Axum-backed) with the right feature gating. Refuses if the project isn't fullstack-capable.")]
    async fn create_server_fn(
        &self,
        Parameters(p): Parameters<tools::scaffold::CreateServerFnParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::scaffold::create_server_fn(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(description = "Live-search dioxuslabs.com docs (scoped to the project's Dioxus version) and return ranked snippets. 15-min cache.")]
    async fn search_docs(
        &self,
        Parameters(p): Parameters<tools::docs::SearchDocsParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::docs::search_docs(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(description = "Find official Dioxus examples on GitHub matching a concept (e.g. 'router', 'fullstack', 'use_signal').")]
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
        description = "Read runtime events captured by the dioxus-mcp-probe crate. Tails target/dioxus-mcp/events.jsonl and returns events matching the filters: kind (render | signal_write | signal_read | server_fn | route | panic | event), since (RFC 3339 cutoff, default last 5 min), component, signal, server_fn, limit (default 200, hard cap 2000). Returns an empty list with a clear note if the probe hasn't been installed yet."
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
        description = "Per-server-fn latency summary derived from the dioxus-mcp-probe log. Pairs phase=start with phase=end by call_id and returns count, ok/err, and min/p50/p95/max latency in microseconds for each #[server] fn called in the window. Filters: since (RFC 3339, default last 5 min), server_fn (one name only), log_path (override)."
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
        ServerInfo::new(
            ServerCapabilities::builder().enable_tools().build(),
        )
        .with_server_info(Implementation::from_build_env())
        .with_instructions(
            "Dioxus project assistant. Tools: create_component, create_route, \
             create_server_fn, check_rsx, audit_feature_flags, explain_signal_graph, \
             route_map, project_index, server_fn_call_graph, asset_audit, \
             dead_components, prop_drill, signal_lint, props_lint, project_tour, \
             search_docs, find_example, openapi_spec, runtime_events, \
             server_fn_summary."
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
