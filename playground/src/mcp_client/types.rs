//! Wire types for the MCP client: the `ScaffoldResult` mirror that
//! `execute_code` returns, and the client error enum.

use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;

/// Mirror of `dioxus-mcp`'s `ScaffoldResult` (the JSON `execute_code` returns
/// inside its tool-result text block). Deserialize-only; every field defaults
/// so a sparse dry-run response (most fields `skip_serializing_if` empty) still
/// parses.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct ScaffoldResult {
    #[serde(default)]
    pub files_created: Vec<String>,
    #[serde(default)]
    pub files_modified: Vec<String>,
    #[serde(default)]
    pub next_steps: Vec<String>,
    #[serde(default)]
    pub collisions: Vec<String>,
    #[serde(default)]
    pub would_create: Vec<String>,
    #[serde(default)]
    pub would_modify: Vec<String>,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub routable_file: Option<String>,
    /// Map of would-be file path → generated source body (raw RSX text).
    #[serde(default)]
    pub previews: BTreeMap<String, String>,
    /// Resolved render models for server-synthesized resource screens (empty
    /// for docs without a `resources:` slice).
    #[serde(default)]
    pub render_models: Vec<crate::model::RenderModel>,
}

/// A pending scaffold proposal from `list_proposals` (M6 cockpit inbox).
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Proposal {
    pub id: String,
    #[serde(default)]
    pub created_at: u64,
    /// The proposed DSL doc (what the human edits before approving).
    #[serde(default)]
    pub code: String,
    /// The dry-run plan for the proposal (file tree, render models, …).
    #[serde(default)]
    pub preview: ScaffoldResult,
    /// Lifecycle tag (`{"state":"pending"}` etc.) — kept opaque here.
    #[serde(default)]
    pub status: serde_json::Value,
}

/// Failure modes of an MCP call.
#[derive(Debug, Clone, PartialEq)]
pub enum McpError {
    /// Transport / HTTP-level failure (connection refused, fetch error, …).
    Http(String),
    /// A JSON-RPC `error` object — e.g. a DSL preflight rejection.
    Rpc { code: i64, message: String },
    /// The response didn't match the shape we expect (missing session id,
    /// no matching SSE frame, unparseable result).
    Protocol(String),
}

impl fmt::Display for McpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            McpError::Http(m) => write!(f, "transport error: {m}"),
            McpError::Rpc { code, message } => write!(f, "DSL error ({code}): {message}"),
            McpError::Protocol(m) => write!(f, "protocol error: {m}"),
        }
    }
}

impl std::error::Error for McpError {}
