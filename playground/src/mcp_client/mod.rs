//! Browser-side client for the `dioxus-mcp` Streamable HTTP transport.
//!
//! Public surface is [`dry_run`]: send a DSL doc to `execute_code` with
//! `dry_run: true` and get back the resolved [`ScaffoldResult`] — no files
//! written, no compile. The transport handshake (initialize/initialized) and
//! `Mcp-Session-Id` plumbing live in [`session`]; SSE frame parsing in [`sse`].

mod session;
mod sse;
mod types;

pub use types::{McpError, Proposal, ScaffoldResult};

use std::cell::Cell;

use serde_json::{Value, json};

thread_local! {
    /// Monotonic JSON-RPC id source (wasm is single-threaded). Each `call_tool`
    /// uses a fresh id so `sse::frame_with_id` matches the right response.
    static RPC_ID: Cell<i64> = const { Cell::new(1) };
}

fn next_id() -> i64 {
    RPC_ID.with(|c| {
        let v = c.get() + 1;
        c.set(v);
        v
    })
}

/// Where the MCP server's HTTP transport listens. Matches
/// `dioxus-mcp --transport http` default bind. The service ignores the path.
const MCP_URL: &str = "http://127.0.0.1:8731/";

/// MCP protocol version we advertise in `initialize`.
const PROTOCOL_VERSION: &str = "2025-03-26";

/// POST a JSON-RPC body and return `(Mcp-Session-Id header, raw body text)`.
/// The body text is SSE-framed; callers parse it via [`sse`].
pub(crate) async fn post_rpc(
    body: Value,
    session: Option<&str>,
) -> Result<(Option<String>, String), McpError> {
    let mut builder = gloo_net::http::Request::post(MCP_URL)
        .header("Accept", "application/json, text/event-stream");
    if let Some(s) = session {
        builder = builder.header("Mcp-Session-Id", s);
    }
    let request = builder
        .json(&body)
        .map_err(|e| McpError::Http(e.to_string()))?;
    let response = request
        .send()
        .await
        .map_err(|e| McpError::Http(e.to_string()))?;

    let session_id = response.headers().get("mcp-session-id");
    let text = response
        .text()
        .await
        .map_err(|e| McpError::Http(e.to_string()))?;
    Ok((session_id, text))
}

/// Call an MCP tool by name; returns the tool's result payload (the JSON inside
/// the `content[0].text` block) as a `Value`. Retries once on a transport
/// failure after resetting the cached session (covers server restarts).
async fn call_tool(name: &str, arguments: Value) -> Result<Value, McpError> {
    match call_tool_once(name, &arguments).await {
        Err(McpError::Http(_)) => {
            session::reset();
            call_tool_once(name, &arguments).await
        }
        other => other,
    }
}

async fn call_tool_once(name: &str, arguments: &Value) -> Result<Value, McpError> {
    let session_id = session::ensure().await?;
    let id = next_id();
    let (_sid, raw) = post_rpc(
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        }),
        Some(&session_id),
    )
    .await?;

    let envelope = sse::frame_with_id(&raw, id)
        .ok_or_else(|| McpError::Protocol("no matching JSON-RPC frame in response".into()))?;

    if let Some(err) = envelope.get("error") {
        return Err(McpError::Rpc {
            code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        });
    }

    // The tool result wraps its payload as a text content block; that text is
    // itself JSON.
    let text = envelope
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .ok_or_else(|| McpError::Protocol("missing result.content[0].text".into()))?;
    serde_json::from_str::<Value>(text)
        .map_err(|e| McpError::Protocol(format!("could not parse tool result: {e}")))
}

fn parse_scaffold(value: Value) -> Result<ScaffoldResult, McpError> {
    serde_json::from_value(value)
        .map_err(|e| McpError::Protocol(format!("could not parse ScaffoldResult: {e}")))
}

/// `execute_code` in dry-run mode: resolved plan (file tree, source, collisions,
/// render models), no writes.
pub async fn dry_run(code: &str) -> Result<ScaffoldResult, McpError> {
    let v = call_tool(
        "execute_code",
        json!({ "code": code, "dry_run": true, "if_missing": true }),
    )
    .await?;
    parse_scaffold(v)
}

/// `execute_code` for real (`dry_run: false`): materialize the slice into the
/// server's project root.
pub async fn apply(code: &str) -> Result<ScaffoldResult, McpError> {
    let v = call_tool(
        "execute_code",
        json!({ "code": code, "dry_run": false, "if_missing": true }),
    )
    .await?;
    parse_scaffold(v)
}

/// List scaffold proposals awaiting a human decision (the cockpit inbox).
pub async fn list_proposals() -> Result<Vec<Proposal>, McpError> {
    let v = call_tool("list_proposals", json!({})).await?;
    let arr = v.get("proposals").cloned().unwrap_or(Value::Array(vec![]));
    serde_json::from_value(arr)
        .map_err(|e| McpError::Protocol(format!("could not parse proposals: {e}")))
}

/// Approve or reject a proposal. `edited_code` (approve only) round-trips a
/// human edit back as the DSL that actually runs.
pub async fn resolve_proposal(
    id: &str,
    action: &str,
    edited_code: Option<&str>,
    reason: Option<&str>,
) -> Result<Value, McpError> {
    let mut args = json!({ "proposal_id": id, "action": action });
    if let Some(code) = edited_code {
        args["edited_code"] = json!(code);
    }
    if let Some(reason) = reason {
        args["reason"] = json!(reason);
    }
    call_tool("resolve_proposal", args).await
}
