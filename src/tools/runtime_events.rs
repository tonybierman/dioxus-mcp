use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::State;
use crate::tools::scaffold::crate_root;

const DEFAULT_LIMIT: usize = 200;
const HARD_CAP: usize = 2000;
const DEFAULT_WINDOW_SECS: i64 = 300;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RuntimeEventsParams {
    /// Absolute path to the Dioxus project root. Defaults to the path the MCP server was
    /// started in.
    pub project_root: Option<String>,
    /// Filter by event kind: "render", "signal_write", "signal_read", "signal",
    /// "server_fn", "route", "panic", or "event". Omit for all kinds.
    pub kind: Option<String>,
    /// RFC 3339 cutoff (e.g. "2026-05-14T18:30:00Z"); only events with `ts >= since`
    /// are returned. Defaults to the last 5 minutes of wall clock.
    pub since: Option<String>,
    /// Filter render/signal events to those tagged with this component name.
    pub component: Option<String>,
    /// Filter signal_write events to those tagged with this signal name.
    pub signal: Option<String>,
    /// Filter server_fn events to those tagged with this function name.
    pub server_fn: Option<String>,
    /// Max events to return. Default 200, hard-capped at 2000.
    pub limit: Option<usize>,
    /// Override the log location. Default: `<project_root>/target/dioxus-mcp/events.jsonl`.
    /// Absolute or relative-to-crate-root.
    pub log_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RuntimeEventsReport {
    pub events: Vec<Value>,
    pub truncated: bool,
    pub log_files_scanned: Vec<PathBuf>,
    pub notes: Vec<String>,
}

pub async fn runtime_events(
    state: &Arc<State>,
    p: RuntimeEventsParams,
) -> Result<RuntimeEventsReport, String> {
    let crate_root = crate_root(state, p.project_root.as_deref()).await?;

    let live_path = resolve_log_path(&crate_root, p.log_path.as_deref());
    let limit = p.limit.unwrap_or(DEFAULT_LIMIT).min(HARD_CAP);
    let since = p.since.clone().unwrap_or_else(|| default_since_iso());

    let mut notes: Vec<String> = Vec::new();
    let mut scanned: Vec<PathBuf> = Vec::new();

    if !live_path.exists() {
        notes.push(format!(
            "log file not found at {} — install dioxus-mcp-probe in your app and run it at least once",
            live_path.display()
        ));
        return Ok(RuntimeEventsReport {
            events: Vec::new(),
            truncated: false,
            log_files_scanned: scanned,
            notes,
        });
    }

    // Read the live file. If the oldest event in live is still newer than the
    // `since` cutoff, scan the most recent rotation as well (events.1.jsonl).
    let mut collected: Vec<Value> = Vec::new();
    let mut truncated = false;

    scanned.push(live_path.clone());
    read_into(&live_path, &p, &since, limit, &mut collected, &mut truncated, &mut notes);

    if !truncated && needs_prev_scan(&collected, &since) {
        let prev = rotated_path(&live_path, 1);
        if prev.exists() {
            scanned.push(prev.clone());
            // Read older file first so chronological order is preserved.
            let mut older: Vec<Value> = Vec::new();
            let mut older_trunc = false;
            read_into(
                &prev,
                &p,
                &since,
                limit.saturating_sub(collected.len()),
                &mut older,
                &mut older_trunc,
                &mut notes,
            );
            // The older file feeds the chronological prefix.
            older.append(&mut collected);
            collected = older;
            if older_trunc {
                truncated = true;
            }
        }
    }

    if collected.is_empty() {
        notes.push(format!(
            "no events matched (since={since}); the probe may not have emitted any since that cutoff"
        ));
    }

    Ok(RuntimeEventsReport {
        events: collected,
        truncated,
        log_files_scanned: scanned,
        notes,
    })
}

fn resolve_log_path(crate_root: &Path, override_path: Option<&str>) -> PathBuf {
    match override_path {
        Some(p) => {
            let pb = PathBuf::from(p);
            if pb.is_absolute() {
                pb
            } else {
                crate_root.join(pb)
            }
        }
        None => crate_root.join("target").join("dioxus-mcp").join("events.jsonl"),
    }
}

fn default_since_iso() -> String {
    let cutoff = time::OffsetDateTime::now_utc() - time::Duration::seconds(DEFAULT_WINDOW_SECS);
    cutoff
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"))
}

fn read_into(
    path: &Path,
    p: &RuntimeEventsParams,
    since: &str,
    remaining: usize,
    out: &mut Vec<Value>,
    truncated: &mut bool,
    notes: &mut Vec<String>,
) {
    if remaining == 0 {
        *truncated = true;
        return;
    }
    let f = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            notes.push(format!("could not open {}: {e}", path.display()));
            return;
        }
    };
    let reader = BufReader::new(f);
    let mut parse_errors = 0usize;

    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                parse_errors += 1;
                continue;
            }
        };
        if !matches_filters(&v, p, since) {
            continue;
        }
        out.push(v);
        if out.len() >= remaining {
            *truncated = true;
            break;
        }
    }
    if parse_errors > 0 {
        notes.push(format!(
            "skipped {parse_errors} malformed line(s) in {}",
            path.display()
        ));
    }
}

fn matches_filters(v: &Value, p: &RuntimeEventsParams, since: &str) -> bool {
    let Some(obj) = v.as_object() else { return false };

    if let Some(ts) = obj.get("ts").and_then(|x| x.as_str()) {
        if ts < since {
            return false;
        }
    }
    if let Some(k) = &p.kind {
        if obj.get("kind").and_then(|x| x.as_str()) != Some(k.as_str()) {
            return false;
        }
    }
    if let Some(c) = &p.component {
        if obj.get("component").and_then(|x| x.as_str()) != Some(c.as_str()) {
            return false;
        }
    }
    if let Some(s) = &p.signal {
        if obj.get("signal").and_then(|x| x.as_str()) != Some(s.as_str()) {
            return false;
        }
    }
    if let Some(sf) = &p.server_fn {
        if obj.get("name").and_then(|x| x.as_str()) != Some(sf.as_str()) {
            return false;
        }
    }
    true
}

fn needs_prev_scan(collected: &[Value], since: &str) -> bool {
    let Some(first) = collected.first() else { return true };
    let Some(ts) = first.as_object().and_then(|o| o.get("ts")).and_then(|x| x.as_str()) else {
        return false;
    };
    // Only chase a rotation if the oldest event we found is still newer than the cutoff
    // — that means older matching events may live in the previous file.
    ts > since
}

fn rotated_path(live: &Path, n: usize) -> PathBuf {
    let parent = live.parent().unwrap_or_else(|| Path::new("."));
    let stem = live.file_stem().and_then(|s| s.to_str()).unwrap_or("events");
    let ext = live.extension().and_then(|s| s.to_str()).unwrap_or("jsonl");
    parent.join(format!("{stem}.{n}.{ext}"))
}
