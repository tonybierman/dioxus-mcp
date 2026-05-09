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
        description = "Spawn `dx serve` for the given platform. Blocks up to ~60s for the server-ready signal, then returns a session_id. Use dx_stop to terminate."
    )]
    async fn dx_serve(
        &self,
        Parameters(p): Parameters<tools::cli::DxServeParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::cli::dx_serve(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(description = "Kill a `dx serve` session previously started by dx_serve.")]
    async fn dx_stop(
        &self,
        Parameters(p): Parameters<tools::cli::DxStopParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::cli::dx_stop(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(description = "Run `dx bundle` for the given platform, capturing structured warnings/errors and final artifact paths.")]
    async fn dx_bundle(
        &self,
        Parameters(p): Parameters<tools::cli::DxBundleParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::cli::dx_bundle(&self.state, p).await {
            Ok(r) => ok_json(&r),
            Err(e) => Err(err(e)),
        }
    }

    #[tool(description = "Run `cargo check --message-format=json` (optionally with a platform feature) and return structured diagnostics.")]
    async fn dx_check(
        &self,
        Parameters(p): Parameters<tools::cli::DxCheckParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::cli::dx_check(&self.state, p).await {
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
}

#[tool_handler]
impl ServerHandler for DioxusMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder().enable_tools().build(),
        )
        .with_server_info(Implementation::from_build_env())
        .with_instructions(
            "Dioxus project assistant. Tools: dx_serve, dx_stop, dx_bundle, dx_check, \
             create_component, create_route, create_server_fn, check_rsx, \
             audit_feature_flags, explain_signal_graph, search_docs, find_example."
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
