use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

use crate::state::State;

const READY_PATTERNS: &[&str] = &[
    "Server listening",
    "Application running",
    "running on",
    "App listening",
    "Local:",
];
const READY_TIMEOUT: Duration = Duration::from_secs(60);
const LOG_TAIL_LINES: usize = 80;

fn project_dir(state: &Arc<State>) -> PathBuf {
    state.project_root.clone()
}

// ---------- dx_serve ----------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct DxServeParams {
    /// Target platform: web, desktop, mobile, fullstack, server.
    pub platform: String,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub release: bool,
}

#[derive(Debug, Serialize)]
pub struct DxServeResult {
    pub session_id: Option<String>,
    pub ready: bool,
    pub timed_out: bool,
    pub exit_code: Option<i32>,
    pub url: Option<String>,
    pub log_tail: Vec<String>,
}

pub async fn dx_serve(state: &Arc<State>, p: DxServeParams) -> Result<DxServeResult, String> {
    let mut cmd = Command::new("dx");
    cmd.arg("serve").arg("--platform").arg(&p.platform);
    if !p.features.is_empty() {
        cmd.arg("--features").arg(p.features.join(","));
    }
    if p.release {
        cmd.arg("--release");
    }
    cmd.current_dir(project_dir(state))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn `dx serve`: {e}"))?;
    let stdout = child.stdout.take().ok_or("dx had no stdout")?;
    let stderr = child.stderr.take().ok_or("dx had no stderr")?;

    let session_id = format!("dx-{}", std::process::id());
    let mut log: Vec<String> = Vec::with_capacity(LOG_TAIL_LINES * 2);
    let mut url: Option<String> = None;
    let mut ready = false;

    let mut out_reader = BufReader::new(stdout).lines();
    let mut err_reader = BufReader::new(stderr).lines();

    let outcome = timeout(READY_TIMEOUT, async {
        loop {
            tokio::select! {
                line = out_reader.next_line() => match line {
                    Ok(Some(l)) => {
                        push_capped(&mut log, format!("out: {l}"));
                        if let Some(u) = extract_url(&l) { url = Some(u); }
                        if READY_PATTERNS.iter().any(|p| l.contains(p)) {
                            ready = true; break;
                        }
                    }
                    Ok(None) => break,
                    Err(_) => break,
                },
                line = err_reader.next_line() => match line {
                    Ok(Some(l)) => {
                        push_capped(&mut log, format!("err: {l}"));
                    }
                    Ok(None) => {},
                    Err(_) => break,
                },
                status = child.wait() => {
                    return Some(status.ok().and_then(|s| s.code()));
                }
            }
        }
        None
    })
    .await;

    match outcome {
        Ok(Some(code)) => Ok(DxServeResult {
            session_id: None,
            ready: false,
            timed_out: false,
            exit_code: Some(code.unwrap_or(-1)),
            url,
            log_tail: log,
        }),
        Ok(None) => {
            // ready or stream ended; keep child if still running
            let still_running = child.try_wait().map(|s| s.is_none()).unwrap_or(false);
            let session = if still_running {
                state
                    .dx_children
                    .lock()
                    .await
                    .insert(session_id.clone(), child);
                Some(session_id)
            } else {
                None
            };
            Ok(DxServeResult {
                session_id: session,
                ready,
                timed_out: false,
                exit_code: None,
                url,
                log_tail: log,
            })
        }
        Err(_) => {
            // timeout — keep the process alive so the caller can dx_stop later
            state
                .dx_children
                .lock()
                .await
                .insert(session_id.clone(), child);
            Ok(DxServeResult {
                session_id: Some(session_id),
                ready: false,
                timed_out: true,
                exit_code: None,
                url,
                log_tail: log,
            })
        }
    }
}

fn push_capped(buf: &mut Vec<String>, s: String) {
    if buf.len() >= LOG_TAIL_LINES * 2 {
        buf.remove(0);
    }
    buf.push(s);
}

fn extract_url(line: &str) -> Option<String> {
    line.split_whitespace()
        .find(|tok| tok.starts_with("http://") || tok.starts_with("https://"))
        .map(|s| s.trim_end_matches(|c: char| matches!(c, '.' | ',' | ')')).to_string())
}

// ---------- dx_stop ----------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct DxStopParams {
    pub session_id: String,
}

#[derive(Debug, Serialize)]
pub struct DxStopResult {
    pub stopped: bool,
}

pub async fn dx_stop(state: &Arc<State>, p: DxStopParams) -> Result<DxStopResult, String> {
    let mut guard = state.dx_children.lock().await;
    match guard.remove(&p.session_id) {
        Some(mut c) => {
            let _ = c.kill().await;
            Ok(DxStopResult { stopped: true })
        }
        None => Err(format!("no running session named {}", p.session_id)),
    }
}

// ---------- dx_bundle ----------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct DxBundleParams {
    pub platform: String,
    #[serde(default)]
    pub release: bool,
}

#[derive(Debug, Serialize)]
pub struct DxBundleResult {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub artifacts: Vec<String>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    pub log_tail: Vec<String>,
}

pub async fn dx_bundle(state: &Arc<State>, p: DxBundleParams) -> Result<DxBundleResult, String> {
    let mut cmd = Command::new("dx");
    cmd.arg("bundle").arg("--platform").arg(&p.platform);
    if p.release {
        cmd.arg("--release");
    }
    cmd.current_dir(project_dir(state))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = cmd
        .output()
        .await
        .map_err(|e| format!("failed to spawn `dx bundle`: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let mut warnings = Vec::new();
    let mut errors = Vec::new();
    let mut artifacts = Vec::new();
    for line in stdout.lines().chain(stderr.lines()) {
        let l = line.trim();
        if l.starts_with("warning:") || l.starts_with("warn:") {
            warnings.push(l.to_string());
        } else if l.starts_with("error:") || l.starts_with("Error:") {
            errors.push(l.to_string());
        }
        if let Some(rest) = l.strip_prefix("Bundled at ").or_else(|| l.strip_prefix("Output: ")) {
            artifacts.push(rest.to_string());
        }
    }
    let mut log_tail: Vec<String> = stdout
        .lines()
        .chain(stderr.lines())
        .map(|s| s.to_string())
        .collect();
    if log_tail.len() > LOG_TAIL_LINES {
        log_tail = log_tail.split_off(log_tail.len() - LOG_TAIL_LINES);
    }

    Ok(DxBundleResult {
        success: output.status.success(),
        exit_code: output.status.code(),
        artifacts,
        warnings,
        errors,
        log_tail,
    })
}

// ---------- dx_check ----------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct DxCheckParams {
    /// Platform feature to enable (web, desktop, mobile, fullstack, server). Optional.
    #[serde(default)]
    pub platform: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DxCheckDiagnostic {
    pub level: String,
    pub message: String,
    pub file: Option<String>,
    pub line: Option<u64>,
    pub column: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct DxCheckResult {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub errors: Vec<DxCheckDiagnostic>,
    pub warnings: Vec<DxCheckDiagnostic>,
    pub notes: Vec<DxCheckDiagnostic>,
}

pub async fn dx_check(state: &Arc<State>, p: DxCheckParams) -> Result<DxCheckResult, String> {
    let mut cmd = Command::new("cargo");
    cmd.arg("check").arg("--message-format=json");
    if let Some(plat) = p.platform.as_deref() {
        cmd.arg("--features").arg(plat);
    }
    cmd.current_dir(project_dir(state))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = cmd
        .output()
        .await
        .map_err(|e| format!("failed to run `cargo check`: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut notes = Vec::new();

    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v.get("reason").and_then(|r| r.as_str()) != Some("compiler-message") {
            continue;
        }
        let Some(msg) = v.get("message") else { continue };
        let level = msg.get("level").and_then(|l| l.as_str()).unwrap_or("");
        let text = msg
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let span = msg
            .get("spans")
            .and_then(|s| s.as_array())
            .and_then(|a| a.iter().find(|s| s.get("is_primary") == Some(&true.into())));
        let (file, line, col) = match span {
            Some(s) => (
                s.get("file_name").and_then(|v| v.as_str()).map(String::from),
                s.get("line_start").and_then(|v| v.as_u64()),
                s.get("column_start").and_then(|v| v.as_u64()),
            ),
            None => (None, None, None),
        };
        let diag = DxCheckDiagnostic {
            level: level.to_string(),
            message: text,
            file,
            line,
            column: col,
        };
        match level {
            "error" | "error: internal compiler error" => errors.push(diag),
            "warning" => warnings.push(diag),
            "note" | "help" => notes.push(diag),
            _ => {}
        }
    }

    Ok(DxCheckResult {
        success: output.status.success(),
        exit_code: output.status.code(),
        errors,
        warnings,
        notes,
    })
}
