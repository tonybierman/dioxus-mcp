use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use moka::future::Cache;
use tokio::process::Child;
use tokio::sync::Mutex;

use crate::project::ProjectInfo;

pub struct State {
    pub project_root: PathBuf,
    pub project: Mutex<ProjectInfo>,
    pub dx_children: Mutex<HashMap<String, Child>>,
    pub doc_cache: Cache<String, Arc<CachedDoc>>,
    pub http: reqwest::Client,
}

#[derive(Debug, Clone)]
pub struct CachedDoc {
    pub url: String,
    pub title: Option<String>,
    pub body: String,
}

impl State {
    pub fn new(project_root: PathBuf) -> Result<Self> {
        let project = ProjectInfo::detect(&project_root);
        let http = reqwest::Client::builder()
            .user_agent("dioxus-mcp/0.1")
            .build()?;
        Ok(Self {
            project_root,
            project: Mutex::new(project),
            dx_children: Mutex::new(HashMap::new()),
            doc_cache: Cache::builder()
                .time_to_live(Duration::from_secs(15 * 60))
                .max_capacity(256)
                .build(),
            http,
        })
    }

    #[allow(dead_code)]
    pub async fn refresh_project(&self) {
        let fresh = ProjectInfo::detect(&self.project_root);
        *self.project.lock().await = fresh;
    }
}
