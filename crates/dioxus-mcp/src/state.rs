use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::Result;
use moka::future::Cache;
use tokio::sync::Mutex;

use crate::project::ProjectInfo;
use crate::proposal::Proposals;

pub struct State {
    pub project_root: PathBuf,
    pub project: Mutex<ProjectInfo>,
    pub doc_cache: Cache<String, Arc<CachedDoc>>,
    pub http: reqwest::Client,
    /// Human-in-the-loop scaffold proposals (M6). Shared across all clients of
    /// this server process.
    pub proposals: Proposals,
    /// Theme/component/layout registry: embedded defaults overlaid by
    /// runtime-loaded descriptors. Loaded once at construct (see `registry.rs`).
    pub registry: dioxus_mcp_registry::Registry,
    /// Set to `true` once `get_dsl_spec` has emitted the authoring-guide
    /// prologue at least once. Subsequent calls within the same MCP server
    /// process default `include_prologue` to `false` — the prologue is most
    /// useful exactly once, and re-shipping ~5KB on every refresh wastes
    /// agent context. Callers can still pass `include_prologue: true`
    /// explicitly to force the full payload.
    pub dsl_spec_prologue_seen: AtomicBool,
}

#[derive(Debug, Clone)]
pub struct CachedDoc {
    pub body: String,
}

impl State {
    pub fn new(project_root: PathBuf) -> Result<Self> {
        let project = ProjectInfo::detect(&project_root);
        let http = reqwest::Client::builder()
            .user_agent("dioxus-mcp/0.1")
            .build()?;
        // Persist proposals under the project's target dir so they survive a
        // server respawn (e.g. an embedded cockpit dying with its session).
        let proposals_path = project_root.join("target/dioxus-mcp/proposals.json");
        // Built-in registry defaults overlaid by any runtime descriptors under
        // the project's (or global) registry dir.
        let registry = crate::registry::load(&project_root);
        Ok(Self {
            project_root,
            project: Mutex::new(project),
            doc_cache: Cache::builder()
                .time_to_live(Duration::from_secs(15 * 60))
                .max_capacity(256)
                .build(),
            http,
            proposals: Proposals::with_path(proposals_path),
            registry,
            dsl_spec_prologue_seen: AtomicBool::new(false),
        })
    }

    #[allow(dead_code)]
    pub async fn refresh_project(&self) {
        let fresh = ProjectInfo::detect(&self.project_root);
        *self.project.lock().await = fresh;
    }
}
