//! MCP session handshake + cache.
//!
//! Stateful Streamable HTTP requires `initialize` → `notifications/initialized`
//! before any `tools/call`, and the server-issued `Mcp-Session-Id` header must
//! be echoed on every later request. We do the handshake once and cache the id
//! (wasm is single-threaded, so a `thread_local` cell is sufficient).

use std::cell::RefCell;

use serde_json::json;

use super::types::McpError;
use super::{PROTOCOL_VERSION, post_rpc};

thread_local! {
    static SESSION: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Drop the cached session id so the next call re-handshakes. Used when a
/// request fails in a way that suggests the server restarted or GC'd the
/// session.
pub fn reset() {
    SESSION.with(|c| *c.borrow_mut() = None);
}

/// Return a usable session id, performing the handshake if we don't have one
/// cached.
pub async fn ensure() -> Result<String, McpError> {
    if let Some(s) = SESSION.with(|c| c.borrow().clone()) {
        return Ok(s);
    }

    let (session_id, _body) = post_rpc(
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "dx-playground", "version": "0.1.0" }
            }
        }),
        None,
    )
    .await?;

    let session_id = session_id
        .ok_or_else(|| McpError::Protocol("server did not return an Mcp-Session-Id".into()))?;

    // The `initialized` notification has no id and gets a 202 with no frame.
    post_rpc(
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        Some(&session_id),
    )
    .await?;

    SESSION.with(|c| *c.borrow_mut() = Some(session_id.clone()));
    Ok(session_id)
}
