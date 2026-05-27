//! Minimal Server-Sent-Events frame parsing.
//!
//! The MCP Streamable HTTP transport (stateful, default config) answers every
//! POST with `text/event-stream` rather than plain JSON. The browser's native
//! `EventSource` is GET-only and can't drive a POST, so we read the response
//! body as text and pull the JSON payload out of the `data:` frames ourselves.

use serde_json::Value;

/// Return the JSON payload of the first SSE `data:` frame whose parsed object
/// has `id == want_id`. A frame may span multiple `data:` lines (per the SSE
/// spec they concatenate); blank-line-separated blocks delimit frames.
pub fn frame_with_id(raw: &str, want_id: i64) -> Option<Value> {
    for block in raw.split("\n\n") {
        let mut payload = String::new();
        for line in block.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                payload.push_str(rest.strip_prefix(' ').unwrap_or(rest));
            }
        }
        if payload.trim().is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(&payload) {
            if v.get("id").and_then(Value::as_i64) == Some(want_id) {
                return Some(v);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_frame_by_id() {
        let raw = "data: \nid: 0\nretry: 3000\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"ok\":true}}\n\n";
        let v = frame_with_id(raw, 2).expect("frame");
        assert_eq!(v["result"]["ok"], serde_json::json!(true));
    }

    #[test]
    fn ignores_non_matching_and_empty() {
        let raw = "data: {\"id\":1,\"result\":1}\n\ndata: \n\n";
        assert!(frame_with_id(raw, 2).is_none());
    }
}
