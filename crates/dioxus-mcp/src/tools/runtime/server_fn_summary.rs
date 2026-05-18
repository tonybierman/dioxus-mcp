use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::State;
use crate::tools::scaffold::crate_root;

const DEFAULT_WINDOW_SECS: i64 = 300;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ServerFnSummaryParams {
    /// Absolute path to the Dioxus project root. Defaults to the path the MCP server was
    /// started in.
    pub project_root: Option<String>,
    /// RFC 3339 cutoff; only events with `ts >= since` are considered. Defaults to the
    /// last 5 minutes.
    pub since: Option<String>,
    /// If set, only return the summary row for this server-fn name.
    pub server_fn: Option<String>,
    /// Override the log location. Default: `<project_root>/target/dioxus-mcp/events.jsonl`.
    pub log_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LatencyStats {
    pub count: usize,
    pub ok: usize,
    pub err: usize,
    pub min_us: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub max_us: u64,
    pub total_ms: f64,
}

#[derive(Debug, Serialize)]
pub struct ServerFnSummary {
    pub name: String,
    pub completed: LatencyStats,
    /// Calls observed with phase=start but no matching phase=end within the window.
    /// Usually means in-flight at the time of the query, or dropped.
    pub pending: usize,
}

#[derive(Debug, Serialize)]
pub struct ServerFnSummaryReport {
    pub summaries: Vec<ServerFnSummary>,
    pub log_files_scanned: Vec<PathBuf>,
    pub notes: Vec<String>,
}

pub async fn server_fn_summary(
    state: &Arc<State>,
    p: ServerFnSummaryParams,
) -> Result<ServerFnSummaryReport, String> {
    let live_path = match p.log_path.as_deref() {
        Some(path) if Path::new(path).is_absolute() => PathBuf::from(path),
        _ => {
            let root = crate_root(state, p.project_root.as_deref()).await?;
            match p.log_path.as_deref() {
                Some(rel) => root.join(rel),
                None => root.join("target").join("dioxus-mcp").join("events.jsonl"),
            }
        }
    };

    let since = p.since.clone().unwrap_or_else(default_since_iso);
    let mut notes: Vec<String> = Vec::new();
    let mut scanned: Vec<PathBuf> = Vec::new();

    if !live_path.exists() {
        notes.push(crate::tools::runtime::runtime_events::probe_missing_note(
            &live_path,
        ));
        return Ok(ServerFnSummaryReport {
            summaries: Vec::new(),
            log_files_scanned: scanned,
            notes,
        });
    }

    scanned.push(live_path.clone());

    // Two-pass over the file:
    //  * collect start events keyed by call_id
    //  * each end event pulls its start, computes a duration, records it
    let f = File::open(&live_path).map_err(|e| format!("open {}: {e}", live_path.display()))?;
    let reader = BufReader::new(f);

    // call_id -> (server_fn_name, start_ts_str)
    let mut starts: HashMap<String, (String, String)> = HashMap::new();
    // server_fn_name -> Vec<(duration_us, ok)>
    let mut completed: BTreeMap<String, Vec<(u64, bool)>> = BTreeMap::new();
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
        let Some(obj) = v.as_object() else { continue };
        if obj.get("kind").and_then(|x| x.as_str()) != Some("server_fn") {
            continue;
        }
        let ts = obj.get("ts").and_then(|x| x.as_str()).unwrap_or("");
        if ts < since.as_str() {
            continue;
        }
        let name = match obj.get("name").and_then(|x| x.as_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if let Some(only) = &p.server_fn
            && &name != only
        {
            continue;
        }
        let phase = obj.get("phase").and_then(|x| x.as_str()).unwrap_or("");
        let call_id = obj
            .get("call_id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();

        match phase {
            "start" if !call_id.is_empty() => {
                starts.insert(call_id, (name, ts.to_string()));
            }
            "end" => {
                let duration_us = obj.get("duration_us").and_then(|x| x.as_u64()).or_else(|| {
                    // Fall back to computing duration from timestamps.
                    let start = starts.get(&call_id).map(|(_, t)| t.as_str())?;
                    duration_from_iso(start, ts)
                });
                let ok = obj.get("ok").and_then(|x| x.as_bool()).unwrap_or(true);
                starts.remove(&call_id);
                if let Some(d) = duration_us {
                    completed.entry(name).or_default().push((d, ok));
                }
            }
            _ => {}
        }
    }

    if parse_errors > 0 {
        notes.push(format!(
            "skipped {parse_errors} malformed line(s) in {}",
            live_path.display()
        ));
    }

    // Compute pending: starts that never matched an end (after the filter).
    let mut pending_by_name: HashMap<String, usize> = HashMap::new();
    for (_, (name, _)) in starts {
        *pending_by_name.entry(name).or_default() += 1;
    }

    // Stitch the result. Include any name that appears in either bucket.
    let mut names: BTreeMap<String, ()> = BTreeMap::new();
    for k in completed.keys() {
        names.insert(k.clone(), ());
    }
    for k in pending_by_name.keys() {
        names.insert(k.clone(), ());
    }

    let mut summaries: Vec<ServerFnSummary> = names
        .into_keys()
        .map(|name| {
            let durations: Vec<(u64, bool)> = completed.get(&name).cloned().unwrap_or_default();
            let pending = pending_by_name.get(&name).copied().unwrap_or(0);
            ServerFnSummary {
                name,
                completed: stats_from(&durations),
                pending,
            }
        })
        .collect();

    summaries.sort_by(|a, b| {
        b.completed
            .count
            .cmp(&a.completed.count)
            .then_with(|| a.name.cmp(&b.name))
    });

    Ok(ServerFnSummaryReport {
        summaries,
        log_files_scanned: scanned,
        notes,
    })
}

fn stats_from(samples: &[(u64, bool)]) -> LatencyStats {
    if samples.is_empty() {
        return LatencyStats {
            count: 0,
            ok: 0,
            err: 0,
            min_us: 0,
            p50_us: 0,
            p95_us: 0,
            max_us: 0,
            total_ms: 0.0,
        };
    }
    let mut sorted: Vec<u64> = samples.iter().map(|(d, _)| *d).collect();
    sorted.sort_unstable();
    let n = sorted.len();
    let pick = |q: f64| -> u64 {
        let idx = ((q * (n - 1) as f64).round() as usize).min(n - 1);
        sorted[idx]
    };
    let ok = samples.iter().filter(|(_, ok)| *ok).count();
    let total_us: u128 = sorted.iter().copied().map(u128::from).sum();
    LatencyStats {
        count: n,
        ok,
        err: n - ok,
        min_us: *sorted.first().unwrap(),
        p50_us: pick(0.50),
        p95_us: pick(0.95),
        max_us: *sorted.last().unwrap(),
        total_ms: total_us as f64 / 1000.0,
    }
}

fn default_since_iso() -> String {
    let cutoff = time::OffsetDateTime::now_utc() - time::Duration::seconds(DEFAULT_WINDOW_SECS);
    cutoff
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"))
}

fn duration_from_iso(start: &str, end: &str) -> Option<u64> {
    let s =
        time::OffsetDateTime::parse(start, &time::format_description::well_known::Rfc3339).ok()?;
    let e =
        time::OffsetDateTime::parse(end, &time::format_description::well_known::Rfc3339).ok()?;
    let dur = e - s;
    if dur.is_negative() {
        return None;
    }
    Some(dur.whole_microseconds() as u64)
}
