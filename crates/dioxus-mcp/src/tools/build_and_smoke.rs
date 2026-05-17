//! `build_and_smoke`: invoke `cargo check` against the project with whatever
//! feature combo the caller asks for (or the canonical fullstack one when
//! they don't), parse the JSON diagnostic stream, and return structured
//! per-error / per-warning entries plus a top-line pass/fail.
//!
//! Why this exists: every other tool in this crate is static analysis. After
//! `execute_code` writes files, the next obvious question — "does this still
//! compile?" — is one a static analyzer can't answer. Callers were shelling
//! out and pasting cargo output back into the conversation. This tool
//! captures that loop with a structured shape so the caller can spot the
//! first error without scrolling 200 lines of cargo progress noise.
//!
//! Out of scope: starting `dx serve` or hitting endpoints. The MCP avoids
//! wrapping `dx` (it should add capabilities `dx` doesn't have, not duplicate
//! ones it already does), and the timing/output complexity of a serve probe
//! belongs in a dedicated end-to-end harness.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::project::ProjectInfo;
use crate::state::State;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct BuildAndSmokeParams {
    /// Absolute path to the Dioxus project root. Required when the MCP server
    /// was not started in the target project directory.
    #[serde(default)]
    pub project_root: Option<String>,
    /// Cargo `--features` list. When omitted, the tool picks a canonical
    /// fullstack combo: `["server"]` for the standard 0.7 layout that already
    /// has `default = ["web"]` + a sibling `server = ["dioxus/server"]`
    /// feature, and `["web", "server"]` otherwise. Pass an explicit list to
    /// override (e.g. `["fullstack"]`).
    #[serde(default)]
    pub features: Option<Vec<String>>,
    /// Pass `--no-default-features` to cargo. Default: false.
    #[serde(default)]
    pub no_default_features: Option<bool>,
    /// Pass `--target wasm32-unknown-unknown`. Default: false. Useful for
    /// catching client-only compile errors that the host-target check misses.
    #[serde(default)]
    pub target_wasm: Option<bool>,
    /// Max wall-clock seconds the cargo invocation may run before this tool
    /// gives up and reports `status: "timed_out"`. Default: 300.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Max diagnostics returned per category (errors + warnings).
    /// Default: 20. Excess entries are dropped and `truncated: true` is set
    /// so callers know the report isn't exhaustive.
    #[serde(default)]
    pub max_messages: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub level: String,
    pub message: String,
    /// Compiler error code (`E0432`, `unused_imports`, …) when present.
    pub code: Option<String>,
    /// Path of the primary span — relative to the project root when possible.
    pub file: Option<String>,
    pub line: Option<usize>,
    pub column: Option<usize>,
    /// Cargo's pre-rendered diagnostic text including source context. Useful
    /// when the caller wants the exact compiler output for a single failure.
    pub rendered: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BuildAndSmokeResult {
    /// The full cargo invocation that ran, for reproducibility.
    pub invocation: String,
    pub project_root: PathBuf,
    pub duration_ms: u128,
    /// `"passed"` | `"failed"` | `"timed_out"` | `"spawn_failed"`.
    pub status: &'static str,
    pub errors_count: usize,
    pub warnings_count: usize,
    pub errors: Vec<Diagnostic>,
    pub warnings: Vec<Diagnostic>,
    /// True when the returned `errors` / `warnings` lists were capped under
    /// `max_messages`.
    pub truncated: bool,
    /// Top-line reason cargo itself couldn't even start (executable missing,
    /// not a Cargo project, etc.). When set, `errors` / `warnings` are empty.
    pub fatal: Option<String>,
    /// Next-step hints, e.g. "re-run with `target_wasm: true` to also catch
    /// wasm-only errors" when the host-target check passed.
    pub next_steps: Vec<String>,
}

pub async fn build_and_smoke(
    state: &Arc<State>,
    p: BuildAndSmokeParams,
) -> Result<BuildAndSmokeResult, String> {
    let project = match p.project_root.as_deref() {
        Some(root) => ProjectInfo::detect(Path::new(root)),
        None => state.project.lock().await.clone(),
    };
    let crate_root = project.manifest_dir().ok_or_else(|| {
        let hint = if p.project_root.is_none() {
            " — pass `project_root` so the tool knows which crate to check"
        } else {
            ""
        };
        format!("no Cargo.toml found from the project's cwd{hint}")
    })?;

    let features = resolve_features(&project, p.features.as_deref());
    let timeout_secs = p.timeout_secs.unwrap_or(300);
    let max_messages = p.max_messages.unwrap_or(20);
    let no_default_features = p.no_default_features.unwrap_or(false);
    let target_wasm = p.target_wasm.unwrap_or(false);

    let mut args: Vec<String> = vec![
        "check".into(),
        "--message-format=json".into(),
        "--quiet".into(),
    ];
    if no_default_features {
        args.push("--no-default-features".into());
    }
    if !features.is_empty() {
        args.push("--features".into());
        args.push(features.join(","));
    }
    if target_wasm {
        args.push("--target".into());
        args.push("wasm32-unknown-unknown".into());
    }

    let invocation = format!("cargo {}", args.join(" "));

    let started = Instant::now();
    let (status, fatal, errors, warnings, truncated) =
        run_and_parse(&crate_root, &args, timeout_secs, max_messages).await;
    let duration_ms = started.elapsed().as_millis();

    let mut next_steps: Vec<String> = Vec::new();
    if status == "passed" {
        if !target_wasm {
            next_steps.push(
                "host-target check is clean — re-run with `target_wasm: true` \
                 to also catch wasm-only compile errors before `dx serve`"
                    .into(),
            );
        }
    } else if status == "timed_out" {
        next_steps.push(format!(
            "cargo check exceeded the {timeout_secs}s budget; re-run with a higher \
             `timeout_secs:` on a cold build, or invoke `cargo {}` manually",
            args.join(" ")
        ));
    }

    Ok(BuildAndSmokeResult {
        invocation,
        project_root: crate_root,
        duration_ms,
        status,
        errors_count: errors.len(),
        warnings_count: warnings.len(),
        errors,
        warnings,
        truncated,
        fatal,
        next_steps,
    })
}

/// Default feature combo for a Dioxus project when the caller doesn't pin one.
///
/// Order of preference:
/// 1. Project already opts into `fullstack` directly on the `dioxus` dep, so
///    a feature-less `cargo check` already covers it — no `--features` needed.
/// 2. Canonical 0.7 layout: `default = ["web"]` + opt-in `server` sibling
///    feature — server-side errors only show up under `--features server`.
/// 3. Older `web` + `server` layout — pass both explicitly.
fn resolve_features(project: &ProjectInfo, override_list: Option<&[String]>) -> Vec<String> {
    if let Some(list) = override_list {
        return list.to_vec();
    }
    let eff: Vec<&str> = project
        .effective_dioxus_features
        .iter()
        .map(|s| s.as_str())
        .collect();
    if project.dioxus_features.iter().any(|f| f == "fullstack") {
        // Already on — feature-less check covers the server-side compile path.
        return Vec::new();
    }
    if eff.contains(&"fullstack") {
        return Vec::new();
    }
    if eff.contains(&"web") {
        // Canonical 0.7 layout: opt-in `server` sibling feature exercises the
        // server compile path on top of the already-active `web` default.
        return vec!["server".into()];
    }
    // Older / non-default layout.
    vec!["web".into(), "server".into()]
}

async fn run_and_parse(
    crate_root: &Path,
    args: &[String],
    timeout_secs: u64,
    max_messages: usize,
) -> (
    &'static str,
    Option<String>,
    Vec<Diagnostic>,
    Vec<Diagnostic>,
    bool,
) {
    use tokio::process::Command;
    use tokio::time::{Duration, timeout};

    let mut cmd = Command::new("cargo");
    cmd.args(args).current_dir(crate_root);
    cmd.env("CARGO_TERM_COLOR", "never");

    let fut = cmd.output();
    let out = match timeout(Duration::from_secs(timeout_secs), fut).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            return (
                "spawn_failed",
                Some(format!("failed to spawn `cargo`: {e} — is cargo on PATH?")),
                Vec::new(),
                Vec::new(),
                false,
            );
        }
        Err(_) => {
            return (
                "timed_out",
                Some(format!("cargo check exceeded the {timeout_secs}s budget")),
                Vec::new(),
                Vec::new(),
                false,
            );
        }
    };

    let stdout = String::from_utf8_lossy(&out.stdout);
    let (errors, warnings, truncated) = parse_cargo_messages(&stdout, crate_root, max_messages);

    // Cargo's exit status determines pass/fail. We don't try to be cleverer
    // than that — `errors.is_empty() && exit_ok` could be true after a build-
    // script failure that doesn't produce a compiler message we can capture.
    let status = if out.status.success() {
        "passed"
    } else {
        "failed"
    };

    let fatal = if !out.status.success() && errors.is_empty() {
        // Failed but no compiler-message JSON — surface stderr so the caller
        // gets some signal (manifest parse error, missing dep, etc.).
        let stderr = String::from_utf8_lossy(&out.stderr);
        let snippet: String = stderr.lines().take(20).collect::<Vec<_>>().join("\n");
        if snippet.trim().is_empty() {
            Some(format!(
                "cargo exited with status {:?} but produced no diagnostics",
                out.status.code()
            ))
        } else {
            Some(snippet)
        }
    } else {
        None
    };

    (status, fatal, errors, warnings, truncated)
}

fn parse_cargo_messages(
    stdout: &str,
    crate_root: &Path,
    max_messages: usize,
) -> (Vec<Diagnostic>, Vec<Diagnostic>, bool) {
    let mut errors: Vec<Diagnostic> = Vec::new();
    let mut warnings: Vec<Diagnostic> = Vec::new();
    let mut truncated = false;

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("reason").and_then(|x| x.as_str()) != Some("compiler-message") {
            continue;
        }
        let msg = match v.get("message") {
            Some(m) => m,
            None => continue,
        };
        let level = msg
            .get("level")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        // Ignore the "X warnings emitted" / "aborting due to" sub-reports.
        if level == "failure-note" {
            continue;
        }
        let message = msg
            .get("message")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let code = msg
            .get("code")
            .and_then(|c| c.get("code"))
            .and_then(|x| x.as_str())
            .map(|s| s.to_string());
        let rendered = msg
            .get("rendered")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string());
        let (file, line_no, column) = primary_span(msg, crate_root);
        let diag = Diagnostic {
            level: level.clone(),
            message,
            code,
            file,
            line: line_no,
            column,
            rendered,
        };
        match level.as_str() {
            "error" | "error: internal compiler error" => {
                if errors.len() >= max_messages {
                    truncated = true;
                    continue;
                }
                errors.push(diag);
            }
            "warning" => {
                if warnings.len() >= max_messages {
                    truncated = true;
                    continue;
                }
                warnings.push(diag);
            }
            // help / note land here — drop them; they're attached to a primary
            // diagnostic via `children:` upstream.
            _ => {}
        }
    }

    (errors, warnings, truncated)
}

/// Extract `(file, line, column)` from the primary span of a cargo
/// `compiler-message`. Files are reported relative to the crate root when
/// possible (cargo gives absolute paths when invoked with --message-format=json).
fn primary_span(
    msg: &serde_json::Value,
    crate_root: &Path,
) -> (Option<String>, Option<usize>, Option<usize>) {
    let spans = match msg.get("spans").and_then(|x| x.as_array()) {
        Some(s) => s,
        None => return (None, None, None),
    };
    let primary = spans
        .iter()
        .find(|s| {
            s.get("is_primary")
                .and_then(|x| x.as_bool())
                .unwrap_or(false)
        })
        .or_else(|| spans.first());
    let span = match primary {
        Some(s) => s,
        None => return (None, None, None),
    };
    let file = span.get("file_name").and_then(|x| x.as_str()).map(|raw| {
        let p = Path::new(raw);
        p.strip_prefix(crate_root)
            .map(|x| x.to_string_lossy().into_owned())
            .unwrap_or_else(|_| raw.to_string())
    });
    let line = span
        .get("line_start")
        .and_then(|x| x.as_u64())
        .map(|n| n as usize);
    let column = span
        .get("column_start")
        .and_then(|x| x.as_u64())
        .map(|n| n as usize);
    (file, line, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span_json(file: &str, line: u64, col: u64, primary: bool) -> serde_json::Value {
        serde_json::json!({
            "file_name": file,
            "line_start": line,
            "line_end": line,
            "column_start": col,
            "column_end": col + 1,
            "is_primary": primary,
        })
    }

    fn compiler_message(level: &str, message: &str, code: Option<&str>) -> String {
        serde_json::json!({
            "reason": "compiler-message",
            "message": {
                "level": level,
                "message": message,
                "code": code.map(|c| serde_json::json!({"code": c, "explanation": null})),
                "spans": [span_json("/abs/proj/src/main.rs", 12, 5, true)],
                "rendered": format!("{level}: {message}\n"),
                "children": [],
            }
        })
        .to_string()
    }

    #[test]
    fn parses_errors_and_warnings_and_caps_each_category() {
        let mut lines: Vec<String> = Vec::new();
        for i in 0..3 {
            lines.push(compiler_message(
                "error",
                &format!("err {i}"),
                Some("E0432"),
            ));
        }
        for i in 0..5 {
            lines.push(compiler_message(
                "warning",
                &format!("warn {i}"),
                Some("unused_imports"),
            ));
        }
        // unrelated cargo line — should be ignored.
        lines.push(serde_json::json!({"reason": "build-script-executed"}).to_string());
        let blob = lines.join("\n");

        let (errors, warnings, truncated) = parse_cargo_messages(&blob, Path::new("/abs/proj"), 2);
        assert_eq!(errors.len(), 2, "errors should be capped at max_messages=2");
        assert_eq!(
            warnings.len(),
            2,
            "warnings should be capped at max_messages=2"
        );
        assert!(truncated, "cap should set truncated=true");
        assert_eq!(errors[0].code.as_deref(), Some("E0432"));
        assert_eq!(errors[0].file.as_deref(), Some("src/main.rs"));
        assert_eq!(errors[0].line, Some(12));
        assert_eq!(errors[0].column, Some(5));
    }

    #[test]
    fn skips_non_compiler_message_lines_and_garbage() {
        let blob = r#"
not-json garbage
{"reason":"compiler-artifact","package_id":"x"}
{"reason":"compiler-message","message":{"level":"warning","message":"hi","spans":[]}}
"#;
        let (errors, warnings, truncated) = parse_cargo_messages(blob, Path::new("/abs/proj"), 10);
        assert!(errors.is_empty());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].message, "hi");
        assert!(!truncated);
    }

    #[test]
    fn resolve_features_picks_fullstack_when_already_active() {
        let project = ProjectInfo {
            manifest_path: None,
            package_name: None,
            dioxus_version: None,
            dioxus_features: vec!["fullstack".into()],
            effective_dioxus_features: vec!["fullstack".into()],
            has_dioxus_toml: false,
            is_dioxus_project: true,
        };
        assert!(
            resolve_features(&project, None).is_empty(),
            "fullstack already active — no extra --features needed"
        );
    }

    #[test]
    fn resolve_features_picks_server_for_canonical_0_7_layout() {
        // default = ["web"] + opt-in server sibling. Effective features
        // include `web` but not `server`.
        let project = ProjectInfo {
            manifest_path: None,
            package_name: None,
            dioxus_version: None,
            dioxus_features: vec!["fullstack".into()],
            effective_dioxus_features: vec!["fullstack".into(), "web".into()],
            has_dioxus_toml: false,
            is_dioxus_project: true,
        };
        // fullstack already active → no extras.
        assert!(resolve_features(&project, None).is_empty());

        let project = ProjectInfo {
            manifest_path: None,
            package_name: None,
            dioxus_version: None,
            dioxus_features: vec![],
            effective_dioxus_features: vec!["web".into()],
            has_dioxus_toml: false,
            is_dioxus_project: true,
        };
        assert_eq!(resolve_features(&project, None), vec!["server".to_string()]);
    }

    #[test]
    fn resolve_features_falls_back_to_web_and_server_for_legacy_layout() {
        let project = ProjectInfo {
            manifest_path: None,
            package_name: None,
            dioxus_version: None,
            dioxus_features: vec![],
            effective_dioxus_features: vec![],
            has_dioxus_toml: false,
            is_dioxus_project: true,
        };
        assert_eq!(
            resolve_features(&project, None),
            vec!["web".to_string(), "server".to_string()]
        );
    }

    #[test]
    fn override_features_take_precedence() {
        let project = ProjectInfo {
            manifest_path: None,
            package_name: None,
            dioxus_version: None,
            dioxus_features: vec!["fullstack".into()],
            effective_dioxus_features: vec!["fullstack".into()],
            has_dioxus_toml: false,
            is_dioxus_project: true,
        };
        assert_eq!(
            resolve_features(&project, Some(&["desktop".into(), "server".into()])),
            vec!["desktop".to_string(), "server".to_string()]
        );
    }
}
