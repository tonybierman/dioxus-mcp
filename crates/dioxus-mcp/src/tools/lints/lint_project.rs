//! Whole-project lint composite. Runs every lint endpoint
//! (`check_rsx`, `dead_components`, `prop_drill`, `signal_lint`, `props_lint`)
//! over the crate's `src/` tree and merges the results into one report.
//!
//! Designed as a single entry point so callers don't have to discover the
//! individual lints. Use `include` / `exclude` to scope the run.

use std::collections::HashSet;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::State;
use crate::tools::ast::walk_rs_files;
use crate::tools::scaffold::crate_root;

const ALL_LINTS: &[&str] = &[
    "check_rsx",
    "dead_components",
    "prop_drill",
    "signal_lint",
    "props_lint",
    "reinvented_widget",
];

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct LintProjectParams {
    /// Subset of lints to run. Defaults to every lint:
    /// `check_rsx`, `dead_components`, `prop_drill`, `signal_lint`, `props_lint`.
    #[serde(default)]
    pub include: Option<Vec<String>>,
    /// Lints to skip (applied after `include`).
    #[serde(default)]
    pub exclude: Option<Vec<String>>,
    /// Optional roots forwarded to `dead_components` (extra component names to
    /// treat as alive on top of the Routable enum + `App`).
    #[serde(default)]
    pub dead_component_roots: Option<Vec<String>>,
    /// Absolute path to the Dioxus project root. Defaults to the cwd the MCP
    /// server was started in.
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize, Default)]
pub struct LintProjectReport {
    /// Pre-rendered markdown digest of the run.
    pub summary: String,
    /// Lints that actually ran (after include/exclude resolution).
    pub lints_run: Vec<String>,
    /// Sum of issues across every lint. Parse errors are not counted.
    pub total_issues: usize,
    /// Per-lint issue counts (in `lints_run` order).
    pub issues_by_lint: Vec<LintCount>,
    /// Files that failed to parse during any lint pass. Deduplicated.
    pub parse_errors: Vec<Value>,
    /// Raw report from each lint, present iff that lint ran. Shape matches the
    /// underlying tool's response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_rsx: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dead_components: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prop_drill: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal_lint: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub props_lint: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reinvented_widget: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct LintCount {
    pub lint: String,
    pub issues: usize,
}

pub async fn lint_project(
    state: &Arc<State>,
    p: LintProjectParams,
) -> Result<LintProjectReport, String> {
    let included: HashSet<String> = match p.include {
        Some(v) => {
            for name in &v {
                if !ALL_LINTS.contains(&name.as_str()) {
                    return Err(format!(
                        "lint_project: unknown lint {name:?} in include; valid: {ALL_LINTS:?}"
                    ));
                }
            }
            v.into_iter().collect()
        }
        None => ALL_LINTS.iter().map(|s| s.to_string()).collect(),
    };
    let excluded: HashSet<String> = p.exclude.unwrap_or_default().into_iter().collect();
    for name in &excluded {
        if !ALL_LINTS.contains(&name.as_str()) {
            return Err(format!(
                "lint_project: unknown lint {name:?} in exclude; valid: {ALL_LINTS:?}"
            ));
        }
    }
    let want = |s: &str| included.contains(s) && !excluded.contains(s);

    let mut report = LintProjectReport::default();
    let mut parse_errors: Vec<Value> = Vec::new();

    if want("check_rsx") {
        let crate_root = crate_root(state, p.project_root.as_deref()).await?;
        let src_root = crate_root.join("src");
        let files: Vec<String> = walk_rs_files(&src_root)
            .into_iter()
            .map(|sf| sf.path.to_string_lossy().into_owned())
            .collect();

        if files.is_empty() {
            // Empty src/ — record an empty report rather than erroring (the
            // single-file form would reject an empty file list).
            report.check_rsx = Some(serde_json::json!({
                "file": src_root,
                "rsx_block_count": 0,
                "issues": [],
                "per_file": [],
            }));
            report.lints_run.push("check_rsx".into());
            report.issues_by_lint.push(LintCount {
                lint: "check_rsx".into(),
                issues: 0,
            });
        } else {
            let r = crate::tools::lints::check_rsx::check_rsx(
                state,
                crate::tools::lints::check_rsx::CheckRsxParams {
                    file: None,
                    files: Some(files),
                    project_root: p.project_root.clone(),
                },
            )
            .await?;
            let count = r.issues.len();
            report.total_issues += count;
            report.lints_run.push("check_rsx".into());
            report.issues_by_lint.push(LintCount {
                lint: "check_rsx".into(),
                issues: count,
            });
            report.check_rsx = Some(serde_json::to_value(&r).unwrap_or(Value::Null));
        }
    }

    if want("dead_components") {
        let r = crate::tools::inspect::dead_components::dead_components(
            state,
            crate::tools::inspect::dead_components::DeadComponentsParams {
                roots: p.dead_component_roots.clone(),
                project_root: p.project_root.clone(),
            },
        )
        .await?;
        let count = r.dead.len();
        for pe in &r.parse_errors {
            parse_errors.push(serde_json::to_value(pe).unwrap_or(Value::Null));
        }
        report.total_issues += count;
        report.lints_run.push("dead_components".into());
        report.issues_by_lint.push(LintCount {
            lint: "dead_components".into(),
            issues: count,
        });
        report.dead_components = Some(serde_json::to_value(&r).unwrap_or(Value::Null));
    }

    if want("prop_drill") {
        let r = crate::tools::inspect::prop_drill::prop_drill(
            state,
            crate::tools::inspect::prop_drill::PropDrillParams {
                project_root: p.project_root.clone(),
                ignore_callbacks: false,
                kinds: None,
            },
        )
        .await?;
        let count: usize = r.parents.iter().map(|p| p.passthroughs.len()).sum();
        for pe in &r.parse_errors {
            parse_errors.push(serde_json::to_value(pe).unwrap_or(Value::Null));
        }
        report.total_issues += count;
        report.lints_run.push("prop_drill".into());
        report.issues_by_lint.push(LintCount {
            lint: "prop_drill".into(),
            issues: count,
        });
        report.prop_drill = Some(serde_json::to_value(&r).unwrap_or(Value::Null));
    }

    if want("signal_lint") {
        let r = crate::tools::lints::signal_lint::signal_lint(
            state,
            crate::tools::lints::signal_lint::SignalLintParams {
                project_root: p.project_root.clone(),
            },
        )
        .await?;
        let count = r.issues.len();
        for pe in &r.parse_errors {
            parse_errors.push(serde_json::to_value(pe).unwrap_or(Value::Null));
        }
        report.total_issues += count;
        report.lints_run.push("signal_lint".into());
        report.issues_by_lint.push(LintCount {
            lint: "signal_lint".into(),
            issues: count,
        });
        report.signal_lint = Some(serde_json::to_value(&r).unwrap_or(Value::Null));
    }

    if want("props_lint") {
        let r = crate::tools::lints::props_lint::props_lint(
            state,
            crate::tools::lints::props_lint::PropsLintParams {
                project_root: p.project_root.clone(),
            },
        )
        .await?;
        let count = r.issues.len();
        for pe in &r.parse_errors {
            parse_errors.push(serde_json::to_value(pe).unwrap_or(Value::Null));
        }
        report.total_issues += count;
        report.lints_run.push("props_lint".into());
        report.issues_by_lint.push(LintCount {
            lint: "props_lint".into(),
            issues: count,
        });
        report.props_lint = Some(serde_json::to_value(&r).unwrap_or(Value::Null));
    }

    if want("reinvented_widget") {
        let r = crate::tools::lints::reinvented_widget::reinvented_widget(
            state,
            crate::tools::lints::reinvented_widget::ReinventedWidgetParams {
                project_root: p.project_root.clone(),
            },
        )
        .await?;
        let count = r.findings.len();
        for pe in &r.parse_errors {
            parse_errors.push(serde_json::to_value(pe).unwrap_or(Value::Null));
        }
        // reinvented_widget findings are hints, not errors — keep them out
        // of `total_issues` so the headline counter still reflects "fix me"
        // signal only. The per-lint count below still surfaces them.
        report.lints_run.push("reinvented_widget".into());
        report.issues_by_lint.push(LintCount {
            lint: "reinvented_widget".into(),
            issues: count,
        });
        report.reinvented_widget = Some(serde_json::to_value(&r).unwrap_or(Value::Null));
    }

    report.parse_errors = dedup_parse_errors(parse_errors);
    report.summary = render_summary(&report);
    Ok(report)
}

fn dedup_parse_errors(errs: Vec<Value>) -> Vec<Value> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for v in errs {
        let key = v.to_string();
        if seen.insert(key) {
            out.push(v);
        }
    }
    out
}

fn render_summary(report: &LintProjectReport) -> String {
    let mut out = String::new();
    out.push_str("# Project lint\n\n");
    out.push_str(&format!(
        "**Lints run**: {} | **Total issues**: {}\n",
        report.lints_run.len(),
        report.total_issues,
    ));
    if !report.parse_errors.is_empty() {
        out.push_str(&format!(
            "**Parse errors**: {} files failed to parse (results may be incomplete)\n",
            report.parse_errors.len()
        ));
    }
    out.push('\n');
    for c in &report.issues_by_lint {
        let badge = if c.issues == 0 { "ok" } else { "issues" };
        out.push_str(&format!("- `{}`: {} ({})\n", c.lint, c.issues, badge));
    }
    out
}
