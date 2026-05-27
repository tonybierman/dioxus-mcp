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
    /// Single-leg override for the target axis. When unset, the tool runs
    /// BOTH legs (host check + `--target wasm32-unknown-unknown`) and
    /// reports the union of diagnostics — `dx serve` cares about wasm-only
    /// errors and the host-only check misses them. Set to `false` to run
    /// only the host leg (faster, fine for purely-server changes); set to
    /// `true` to run only the wasm leg.
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
    /// When true AND `target_wasm` is not set, run the host leg first and
    /// only run the wasm leg if host passed. Keeps fast-fail behaviour
    /// without forcing the caller to flip `target_wasm: false` (which
    /// silently drops wasm coverage on the next run). Default: false —
    /// both legs run unconditionally.
    #[serde(default)]
    pub quick: Option<bool>,
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
pub struct BuildLegResult {
    /// `"host"` or `"wasm"` — the axis this leg covered.
    pub target: &'static str,
    /// The cargo invocation that ran for this leg.
    pub invocation: String,
    pub duration_ms: u128,
    /// `"passed"` | `"failed"` | `"timed_out"` | `"spawn_failed"`.
    pub status: &'static str,
    pub errors_count: usize,
    pub warnings_count: usize,
    pub errors: Vec<Diagnostic>,
    pub warnings: Vec<Diagnostic>,
    pub truncated: bool,
    pub fatal: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BuildAndSmokeResult {
    /// First leg's invocation, retained for backward compatibility with the
    /// single-leg shape callers used to assert on. Real callers should walk
    /// `legs` instead — it carries the cargo invocation per axis.
    pub invocation: String,
    pub project_root: PathBuf,
    /// Wall-clock total for the whole run — the sum of every leg's
    /// `duration_ms`. Renamed from `duration_ms` to `total_ms` because the
    /// per-leg numbers also live under `legs[].duration_ms`; a top-level
    /// `duration_ms` was easy to misread as a single timing. `duration_ms`
    /// remains as a synonym for backward compatibility — it always carries
    /// the same value as `total_ms`.
    pub total_ms: u128,
    /// @deprecated alias for `total_ms`. Kept so existing callers that
    /// asserted on `duration_ms` keep working. New code should read
    /// `total_ms` (or the per-leg `legs[].duration_ms` for the slowest leg).
    pub duration_ms: u128,
    /// Aggregated status across every leg. `"passed"` only when every leg
    /// passed; otherwise the first non-passing status wins (`"failed"`
    /// > `"timed_out"` > `"spawn_failed"`).
    pub status: &'static str,
    /// Sum of errors / warnings across every leg.
    pub errors_count: usize,
    pub warnings_count: usize,
    /// Flat merge of every leg's diagnostics. Each entry is annotated via
    /// `rendered` (cargo's text) so the source axis stays visible.
    pub errors: Vec<Diagnostic>,
    pub warnings: Vec<Diagnostic>,
    /// True when any leg's `errors` / `warnings` were capped.
    pub truncated: bool,
    /// First leg's fatal reason (executable missing, not a Cargo project,
    /// etc.). When set on leg 1, the subsequent leg is skipped.
    pub fatal: Option<String>,
    /// Next-step hints. Empty when both legs ran clean.
    pub next_steps: Vec<String>,
    /// Per-leg breakdown. Single-leg mode (caller passed `target_wasm`)
    /// has one entry; default both-legs mode has two.
    pub legs: Vec<BuildLegResult>,
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
    let quick = p.quick.unwrap_or(false);

    // Default to running both legs: host check + wasm check. The host leg
    // catches `cargo build` errors fast (no `dx serve` round-trip) while the
    // wasm leg catches client-only breakage that `cargo check` for the host
    // target misses. Callers who explicitly pass `target_wasm: true|false`
    // get a single leg.
    let legs_to_run: Vec<&'static str> = match p.target_wasm {
        None => vec!["host", "wasm"],
        Some(true) => vec!["wasm"],
        Some(false) => vec!["host"],
    };

    let outer_start = Instant::now();
    let mut legs: Vec<BuildLegResult> = Vec::new();
    let mut aggregate_errors: Vec<Diagnostic> = Vec::new();
    let mut aggregate_warnings: Vec<Diagnostic> = Vec::new();
    let mut aggregate_truncated = false;
    let mut first_fatal: Option<String> = None;
    let mut first_invocation: Option<String> = None;
    let mut quick_skipped_wasm = false;

    for leg in &legs_to_run {
        // Quick mode: when both legs are scheduled and the previous leg
        // failed, skip the remaining ones. The whole point of `quick: true`
        // is that the caller doesn't pay 30s of wasm compile to find out
        // host already broke.
        if quick && legs_to_run.len() > 1 && legs.iter().any(|l| l.status != "passed") {
            quick_skipped_wasm = true;
            break;
        }
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
        if *leg == "wasm" {
            args.push("--target".into());
            args.push("wasm32-unknown-unknown".into());
        }
        let invocation = format!("cargo {}", args.join(" "));
        if first_invocation.is_none() {
            first_invocation = Some(invocation.clone());
        }

        let started = Instant::now();
        let (status, fatal, errors, warnings, truncated) =
            run_and_parse(&crate_root, &args, timeout_secs, max_messages).await;
        let leg_ms = started.elapsed().as_millis();

        aggregate_errors.extend(errors.iter().cloned());
        aggregate_warnings.extend(warnings.iter().cloned());
        aggregate_truncated |= truncated;
        if first_fatal.is_none() {
            first_fatal = fatal.clone();
        }

        let target_label: &'static str = if *leg == "wasm" { "wasm" } else { "host" };
        legs.push(BuildLegResult {
            target: target_label,
            invocation,
            duration_ms: leg_ms,
            status,
            errors_count: errors.len(),
            warnings_count: warnings.len(),
            errors,
            warnings,
            truncated,
            fatal,
        });

        // If the first leg's cargo couldn't even start, skip the second —
        // it would hit the same failure (e.g. cargo not on PATH).
        if first_fatal.is_some() {
            break;
        }
    }

    // Aggregate status: prefer the worst status across legs so a passing
    // host + failing wasm reports as failed (the user can't `dx serve` it).
    let aggregate_status: &'static str =
        legs.iter()
            .map(|l| l.status)
            .fold("passed", |acc, s| match (acc, s) {
                (_, "failed") | ("failed", _) => "failed",
                (_, "timed_out") | ("timed_out", _) => "timed_out",
                (_, "spawn_failed") | ("spawn_failed", _) => "spawn_failed",
                _ => acc,
            });

    let mut next_steps: Vec<String> = Vec::new();
    if aggregate_status == "timed_out" {
        next_steps.push(format!(
            "one or more legs of cargo check exceeded the {timeout_secs}s budget; \
             re-run with a higher `timeout_secs:` on a cold build"
        ));
    }
    if aggregate_status == "failed" {
        // Surface which leg failed so the caller doesn't have to walk
        // `legs[].status` themselves to know whether the host or wasm side
        // is broken.
        let failed: Vec<&str> = legs
            .iter()
            .filter(|l| l.status == "failed")
            .map(|l| l.target)
            .collect();
        if !failed.is_empty() {
            next_steps.push(format!(
                "compile errors on leg(s): {} — fix the listed errors before `dx serve`",
                failed.join(", ")
            ));
        }
    }
    if legs_to_run.len() == 1 && aggregate_status == "passed" {
        let only = legs_to_run[0];
        let other = if only == "host" { "wasm" } else { "host" };
        next_steps.push(format!(
            "only the {only} leg ran (`target_wasm:` was passed explicitly); the {other} \
             leg may still have compile errors — omit `target_wasm` to run both"
        ));
    }
    if quick_skipped_wasm {
        next_steps.push(
            "host leg failed under `quick: true` — wasm leg skipped to fail fast. \
             Fix the host errors and re-run; once host passes, the wasm leg will \
             run automatically (no flag flip needed)."
                .into(),
        );
    }

    let total_ms = outer_start.elapsed().as_millis();
    Ok(BuildAndSmokeResult {
        invocation: first_invocation.unwrap_or_default(),
        project_root: crate_root,
        total_ms,
        duration_ms: total_ms,
        status: aggregate_status,
        errors_count: aggregate_errors.len(),
        warnings_count: aggregate_warnings.len(),
        errors: aggregate_errors,
        warnings: aggregate_warnings,
        truncated: aggregate_truncated,
        fatal: first_fatal,
        next_steps,
        legs,
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

    /// `quick: true` is a per-run knob; this test pins the helper that
    /// decides whether to short-circuit. The check is: if any earlier leg
    /// failed AND there are more legs to run AND quick is set, skip.
    #[test]
    fn quick_short_circuits_on_failed_first_leg() {
        let legs = [BuildLegResult {
            target: "host",
            invocation: "cargo check".into(),
            duration_ms: 1,
            status: "failed",
            errors_count: 1,
            warnings_count: 0,
            errors: Vec::new(),
            warnings: Vec::new(),
            truncated: false,
            fatal: None,
        }];
        let quick = true;
        let legs_remaining = 2;
        let should_skip = quick && legs_remaining > 1 && legs.iter().any(|l| l.status != "passed");
        assert!(
            should_skip,
            "quick + remaining legs + failed prior should short-circuit"
        );
    }

    /// Quick mode is a no-op when only one leg is scheduled (the caller
    /// already opted into a single-leg run via `target_wasm`).
    #[test]
    fn quick_does_not_apply_when_single_leg() {
        let legs = [BuildLegResult {
            target: "host",
            invocation: "cargo check".into(),
            duration_ms: 1,
            status: "failed",
            errors_count: 1,
            warnings_count: 0,
            errors: Vec::new(),
            warnings: Vec::new(),
            truncated: false,
            fatal: None,
        }];
        let quick = true;
        let legs_remaining = 1;
        let should_skip = quick && legs_remaining > 1 && legs.iter().any(|l| l.status != "passed");
        assert!(
            !should_skip,
            "single-leg run shouldn't trigger the short-circuit"
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
