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
        Ok(Self {
            project_root,
            project: Mutex::new(project),
            doc_cache: Cache::builder()
                .time_to_live(Duration::from_secs(15 * 60))
                .max_capacity(256)
                .build(),
            http,
            proposals: Proposals::with_path(proposals_path),
            dsl_spec_prologue_seen: AtomicBool::new(false),
        })
    }

    /// Load the theme/component/layout registry fresh from disk (built-in
    /// defaults overlaid by runtime descriptors). Loaded on demand rather than
    /// cached, so descriptors **hot-reload** — add or edit a file and the next
    /// call sees it with no server restart. The set is a handful of small TOML
    /// files, so the per-call cost is negligible and these aren't hot paths.
    pub fn registry(&self) -> dioxus_mcp_registry::Registry {
        crate::registry::load(&self.project_root)
    }

    #[allow(dead_code)]
    pub async fn refresh_project(&self) {
        let fresh = ProjectInfo::detect(&self.project_root);
        *self.project.lock().await = fresh;
    }
}
