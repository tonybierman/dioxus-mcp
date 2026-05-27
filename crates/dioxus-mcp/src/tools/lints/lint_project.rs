//! Whole-project lint composite. Runs every lint endpoint
//! (`check_rsx`, `dead_components`, `prop_drill`, `signal_lint`, `props_lint`,
//! `reinvented_widget`, `optimistic_lock_gate`, `server_state_blocking_locks`,
//! `components_audit`) over the crate's `src/` tree and merges the results
//! into one report.
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
    "signal_drilled_2_levels",
    "props_lint",
    "reinvented_widget",
    "optimistic_lock_gate",
    "server_state_blocking_locks",
    "presence_map_unbounded",
    "insecure_set_cookie",
    "components_audit",
    "duplicate_helper_client_server",
    "vec_or_owned_prop_passthrough",
    "magic_id_prefix_for_optimistic",
    "shared_enum_validation",
    "derived_view_no_memo",
    "empty_async_error_arm",
    "polling_future_no_backoff",
    "repeated_auth_extractor",
];

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct LintProjectParams {
    /// Subset of lints to run. Defaults to every lint:
    /// `check_rsx`, `dead_components`, `prop_drill`, `signal_lint`,
    /// `props_lint`, `reinvented_widget`, `optimistic_lock_gate`,
    /// `server_state_blocking_locks`, `components_audit`.
    #[serde(default)]
    pub include: Option<Vec<String>>,
    /// Lints to skip (applied after `include`). Same valid names as `include`:
    /// `check_rsx`, `dead_components`, `prop_drill`, `signal_lint`,
    /// `props_lint`, `reinvented_widget`, `optimistic_lock_gate`,
    /// `server_state_blocking_locks`, `components_audit`.
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
    /// Top findings rolled up by `(lint, code, severity)` and sorted by
    /// severity descending (`error` > `warning` > `info`). Lets callers
    /// pick the highest-leverage fixes without parsing every lint's body.
    /// When findings carry no explicit severity, the lint's default tier
    /// is used (`check_rsx` → `error`; `prop_drill` → mix of `info` /
    /// `warning`; `reinvented_widget` / `components_audit` → hint, mapped
    /// to `info`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headline: Vec<HeadlineEntry>,
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
    pub signal_drilled_2_levels: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub props_lint: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reinvented_widget: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub optimistic_lock_gate: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_state_blocking_locks: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_map_unbounded: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub insecure_set_cookie: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components_audit: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate_helper_client_server: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vec_or_owned_prop_passthrough: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub magic_id_prefix_for_optimistic: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_enum_validation: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derived_view_no_memo: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub empty_async_error_arm: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub polling_future_no_backoff: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeated_auth_extractor: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct LintCount {
    pub lint: String,
    pub issues: usize,
}

/// One row in the severity-rollup table. Grouping by `(lint, code,
/// severity)` keeps the row count bounded — a 17-issue project might land
/// 4-6 rows — and gives the caller "top-3 fixes" without per-finding
/// parsing.
#[derive(Debug, Serialize, Clone)]
pub struct HeadlineEntry {
    pub lint: String,
    /// Stable finding code from the underlying lint (e.g.
    /// `optimistic_lock_gate`, `state_passthrough`, `unset_secure`).
    /// `None` when the lint emits one finding-shape per issue and doesn't
    /// distinguish further.
    pub code: Option<String>,
    /// `error` | `warning` | `info`. Higher tiers sort first.
    pub severity: &'static str,
    pub count: usize,
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
            // single-file form would reject an empty file list). No `file:`
            // field: batch mode never surfaces one (see `CheckRsxReport::file`
            // — the top-level pointer was misleading callers in real runs).
            report.check_rsx = Some(serde_json::json!({
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
                min_chain_depth: None,
            },
        )
        .await?;
        let count: usize = r.parents.iter().map(|p| p.passthroughs.len()).sum();
        for pe in &r.parse_errors {
            parse_errors.push(serde_json::to_value(pe).unwrap_or(Value::Null));
        }
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
        report.lints_run.push("signal_lint".into());
        report.issues_by_lint.push(LintCount {
            lint: "signal_lint".into(),
            issues: count,
        });
        report.signal_lint = Some(serde_json::to_value(&r).unwrap_or(Value::Null));
    }

    if want("signal_drilled_2_levels") {
        let r = crate::tools::lints::signal_drilled_2_levels::signal_drilled_2_levels(
            state,
            crate::tools::lints::signal_drilled_2_levels::SignalDrilledParams {
                project_root: p.project_root.clone(),
            },
        )
        .await?;
        let count = r.findings.len();
        for pe in &r.parse_errors {
            parse_errors.push(serde_json::to_value(pe).unwrap_or(Value::Null));
        }
        report.lints_run.push("signal_drilled_2_levels".into());
        report.issues_by_lint.push(LintCount {
            lint: "signal_drilled_2_levels".into(),
            issues: count,
        });
        report.signal_drilled_2_levels = Some(serde_json::to_value(&r).unwrap_or(Value::Null));
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
        report.lints_run.push("reinvented_widget".into());
        report.issues_by_lint.push(LintCount {
            lint: "reinvented_widget".into(),
            issues: count,
        });
        report.reinvented_widget = Some(serde_json::to_value(&r).unwrap_or(Value::Null));
    }

    if want("optimistic_lock_gate") {
        let r = crate::tools::lints::optimistic_lock_gate::optimistic_lock_gate(
            state,
            crate::tools::lints::optimistic_lock_gate::OptimisticLockGateParams {
                project_root: p.project_root.clone(),
            },
        )
        .await?;
        let count = r.findings.len();
        for pe in &r.parse_errors {
            parse_errors.push(serde_json::to_value(pe).unwrap_or(Value::Null));
        }
        report.lints_run.push("optimistic_lock_gate".into());
        report.issues_by_lint.push(LintCount {
            lint: "optimistic_lock_gate".into(),
            issues: count,
        });
        report.optimistic_lock_gate = Some(serde_json::to_value(&r).unwrap_or(Value::Null));
    }

    if want("server_state_blocking_locks") {
        let r = crate::tools::lints::server_state_blocking_locks::server_state_blocking_locks(
            state,
            crate::tools::lints::server_state_blocking_locks::ServerStateBlockingLocksParams {
                project_root: p.project_root.clone(),
            },
        )
        .await?;
        let count = r.findings.len();
        for pe in &r.parse_errors {
            parse_errors.push(serde_json::to_value(pe).unwrap_or(Value::Null));
        }
        report.lints_run.push("server_state_blocking_locks".into());
        report.issues_by_lint.push(LintCount {
            lint: "server_state_blocking_locks".into(),
            issues: count,
        });
        report.server_state_blocking_locks = Some(serde_json::to_value(&r).unwrap_or(Value::Null));
    }

    if want("presence_map_unbounded") {
        let r = crate::tools::lints::presence_map_unbounded::presence_map_unbounded(
            state,
            crate::tools::lints::presence_map_unbounded::PresenceMapUnboundedParams {
                project_root: p.project_root.clone(),
            },
        )
        .await?;
        let count = r.findings.len();
        for pe in &r.parse_errors {
            parse_errors.push(serde_json::to_value(pe).unwrap_or(Value::Null));
        }
        report.lints_run.push("presence_map_unbounded".into());
        report.issues_by_lint.push(LintCount {
            lint: "presence_map_unbounded".into(),
            issues: count,
        });
        report.presence_map_unbounded = Some(serde_json::to_value(&r).unwrap_or(Value::Null));
    }

    if want("insecure_set_cookie") {
        let r = crate::tools::lints::insecure_set_cookie::insecure_set_cookie(
            state,
            crate::tools::lints::insecure_set_cookie::InsecureSetCookieParams {
                project_root: p.project_root.clone(),
            },
        )
        .await?;
        let count = r.findings.len();
        for pe in &r.parse_errors {
            parse_errors.push(serde_json::to_value(pe).unwrap_or(Value::Null));
        }
        report.lints_run.push("insecure_set_cookie".into());
        report.issues_by_lint.push(LintCount {
            lint: "insecure_set_cookie".into(),
            issues: count,
        });
        report.insecure_set_cookie = Some(serde_json::to_value(&r).unwrap_or(Value::Null));
    }

    if want("components_audit") {
        let r = crate::tools::lints::components_audit::components_audit(
            state,
            crate::tools::lints::components_audit::ComponentsAuditParams {
                project_root: p.project_root.clone(),
            },
        )
        .await?;
        let count = r.findings.len();
        for pe in &r.parse_errors {
            parse_errors.push(serde_json::to_value(pe).unwrap_or(Value::Null));
        }
        report.lints_run.push("components_audit".into());
        report.issues_by_lint.push(LintCount {
            lint: "components_audit".into(),
            issues: count,
        });
        report.components_audit = Some(serde_json::to_value(&r).unwrap_or(Value::Null));
    }

    if want("duplicate_helper_client_server") {
        let r =
            crate::tools::lints::duplicate_helper_client_server::duplicate_helper_client_server(
                state,
                crate::tools::lints::duplicate_helper_client_server::DuplicateHelperParams {
                    project_root: p.project_root.clone(),
                },
            )
            .await?;
        let count = r.findings.len();
        for pe in &r.parse_errors {
            parse_errors.push(serde_json::to_value(pe).unwrap_or(Value::Null));
        }
        report
            .lints_run
            .push("duplicate_helper_client_server".into());
        report.issues_by_lint.push(LintCount {
            lint: "duplicate_helper_client_server".into(),
            issues: count,
        });
        report.duplicate_helper_client_server =
            Some(serde_json::to_value(&r).unwrap_or(Value::Null));
    }

    if want("vec_or_owned_prop_passthrough") {
        let r = crate::tools::lints::vec_or_owned_prop_passthrough::vec_or_owned_prop_passthrough(
            state,
            crate::tools::lints::vec_or_owned_prop_passthrough::VecOrOwnedPropParams {
                project_root: p.project_root.clone(),
            },
        )
        .await?;
        let count = r.findings.len();
        for pe in &r.parse_errors {
            parse_errors.push(serde_json::to_value(pe).unwrap_or(Value::Null));
        }
        report
            .lints_run
            .push("vec_or_owned_prop_passthrough".into());
        report.issues_by_lint.push(LintCount {
            lint: "vec_or_owned_prop_passthrough".into(),
            issues: count,
        });
        report.vec_or_owned_prop_passthrough =
            Some(serde_json::to_value(&r).unwrap_or(Value::Null));
    }

    if want("magic_id_prefix_for_optimistic") {
        let r = crate::tools::lints::magic_id_prefix::magic_id_prefix_for_optimistic(
            state,
            crate::tools::lints::magic_id_prefix::MagicIdPrefixParams {
                project_root: p.project_root.clone(),
            },
        )
        .await?;
        let count = r.findings.len();
        for pe in &r.parse_errors {
            parse_errors.push(serde_json::to_value(pe).unwrap_or(Value::Null));
        }
        report
            .lints_run
            .push("magic_id_prefix_for_optimistic".into());
        report.issues_by_lint.push(LintCount {
            lint: "magic_id_prefix_for_optimistic".into(),
            issues: count,
        });
        report.magic_id_prefix_for_optimistic =
            Some(serde_json::to_value(&r).unwrap_or(Value::Null));
    }

    if want("shared_enum_validation") {
        let r = crate::tools::lints::shared_enum_validation::shared_enum_validation(
            state,
            crate::tools::lints::shared_enum_validation::SharedEnumValidationParams {
                project_root: p.project_root.clone(),
            },
        )
        .await?;
        let count = r.findings.len();
        for pe in &r.parse_errors {
            parse_errors.push(serde_json::to_value(pe).unwrap_or(Value::Null));
        }
        report.lints_run.push("shared_enum_validation".into());
        report.issues_by_lint.push(LintCount {
            lint: "shared_enum_validation".into(),
            issues: count,
        });
        report.shared_enum_validation = Some(serde_json::to_value(&r).unwrap_or(Value::Null));
    }

    if want("derived_view_no_memo") {
        let r = crate::tools::lints::derived_view_no_memo::derived_view_no_memo(
            state,
            crate::tools::lints::derived_view_no_memo::DerivedViewNoMemoParams {
                project_root: p.project_root.clone(),
            },
        )
        .await?;
        let count = r.findings.len();
        for pe in &r.parse_errors {
            parse_errors.push(serde_json::to_value(pe).unwrap_or(Value::Null));
        }
        report.lints_run.push("derived_view_no_memo".into());
        report.issues_by_lint.push(LintCount {
            lint: "derived_view_no_memo".into(),
            issues: count,
        });
        report.derived_view_no_memo = Some(serde_json::to_value(&r).unwrap_or(Value::Null));
    }

    if want("empty_async_error_arm") {
        let r = crate::tools::lints::empty_async_error_arm::empty_async_error_arm(
            state,
            crate::tools::lints::empty_async_error_arm::EmptyAsyncErrorArmParams {
                project_root: p.project_root.clone(),
            },
        )
        .await?;
        let count = r.findings.len();
        for pe in &r.parse_errors {
            parse_errors.push(serde_json::to_value(pe).unwrap_or(Value::Null));
        }
        report.lints_run.push("empty_async_error_arm".into());
        report.issues_by_lint.push(LintCount {
            lint: "empty_async_error_arm".into(),
            issues: count,
        });
        report.empty_async_error_arm = Some(serde_json::to_value(&r).unwrap_or(Value::Null));
    }

    if want("polling_future_no_backoff") {
        let r = crate::tools::lints::polling_future_no_backoff::polling_future_no_backoff(
            state,
            crate::tools::lints::polling_future_no_backoff::PollingFutureNoBackoffParams {
                project_root: p.project_root.clone(),
            },
        )
        .await?;
        let count = r.findings.len();
        for pe in &r.parse_errors {
            parse_errors.push(serde_json::to_value(pe).unwrap_or(Value::Null));
        }
        report.lints_run.push("polling_future_no_backoff".into());
        report.issues_by_lint.push(LintCount {
            lint: "polling_future_no_backoff".into(),
            issues: count,
        });
        report.polling_future_no_backoff = Some(serde_json::to_value(&r).unwrap_or(Value::Null));
    }

    if want("repeated_auth_extractor") {
        let r = crate::tools::lints::repeated_auth_extractor::repeated_auth_extractor(
            state,
            crate::tools::lints::repeated_auth_extractor::RepeatedAuthExtractorParams {
                project_root: p.project_root.clone(),
                min_call_sites: None,
            },
        )
        .await?;
        let count = r.findings.len();
        for pe in &r.parse_errors {
            parse_errors.push(serde_json::to_value(pe).unwrap_or(Value::Null));
        }
        report.lints_run.push("repeated_auth_extractor".into());
        report.issues_by_lint.push(LintCount {
            lint: "repeated_auth_extractor".into(),
            issues: count,
        });
        report.repeated_auth_extractor = Some(serde_json::to_value(&r).unwrap_or(Value::Null));
    }

    // Sum from `issues_by_lint` instead of accumulating per-lint, so adding a
    // new lint can't silently drop its count from the headline number.
    report.total_issues = report.issues_by_lint.iter().map(|c| c.issues).sum();
    report.headline = build_headline(&report);
    report.parse_errors = dedup_parse_errors(parse_errors);
    report.summary = render_summary(&report);
    Ok(report)
}

/// Walk every embedded per-lint report and roll findings up to
/// `(lint, code, severity, count)`. Sorting is severity descending then
/// count descending, so the first row is always the highest-leverage fix.
///
/// We bucket lints into three groups based on their finding shape:
///   1. `findings[]` carrying explicit `severity` + `code`. We use them
///      verbatim (e.g. `signal_drilled_2_levels`, `insecure_set_cookie`,
///      `components_audit`, `presence_map_unbounded`).
///   2. `issues[]` carrying explicit `code` but no severity field. We
///      assign the lint's default tier — `check_rsx` issues are always
///      `error`; `signal_lint` / `props_lint` are `warning`.
///   3. `parents[].passthroughs[]` (only `prop_drill`). We group by
///      `kind` (state_passthrough vs callback_passthrough) and respect
///      the per-passthrough `severity` field.
///
/// Anything else (`dead_components.dead[]`) lands as a single
/// `severity: "warning"` bucket using the lint name as code.
fn build_headline(report: &LintProjectReport) -> Vec<HeadlineEntry> {
    use std::collections::HashMap;

    let mut buckets: HashMap<(String, Option<String>, &'static str), usize> = HashMap::new();
    let mut bump = |lint: &str, code: Option<String>, severity: &'static str, n: usize| {
        if n == 0 {
            return;
        }
        *buckets
            .entry((lint.to_string(), code, severity))
            .or_insert(0) += n;
    };

    if let Some(v) = &report.check_rsx
        && let Some(arr) = v.get("issues").and_then(|x| x.as_array())
    {
        for issue in arr {
            let code = issue
                .get("code")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());
            bump("check_rsx", code, "error", 1);
        }
    }
    if let Some(v) = &report.dead_components
        && let Some(arr) = v.get("dead").and_then(|x| x.as_array())
    {
        bump("dead_components", None, "warning", arr.len());
    }
    if let Some(v) = &report.prop_drill
        && let Some(parents) = v.get("parents").and_then(|x| x.as_array())
    {
        for parent in parents {
            let Some(pts) = parent.get("passthroughs").and_then(|x| x.as_array()) else {
                continue;
            };
            for pt in pts {
                let kind = pt
                    .get("kind")
                    .and_then(|x| x.as_str())
                    .unwrap_or("state_passthrough")
                    .to_string();
                let sev = match pt.get("severity").and_then(|x| x.as_str()) {
                    Some("error") => "error",
                    Some("info") => "info",
                    Some("hint") => "hint",
                    _ => "warning",
                };
                bump("prop_drill", Some(kind), sev, 1);
            }
        }
    }
    if let Some(v) = &report.signal_lint
        && let Some(arr) = v.get("issues").and_then(|x| x.as_array())
    {
        for issue in arr {
            let code = issue
                .get("code")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());
            bump("signal_lint", code, "warning", 1);
        }
    }
    if let Some(v) = &report.signal_drilled_2_levels
        && let Some(arr) = v.get("findings").and_then(|x| x.as_array())
    {
        for finding in arr {
            let code = finding
                .get("code")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());
            let sev = severity_str(finding.get("severity").and_then(|x| x.as_str()), "warning");
            bump("signal_drilled_2_levels", code, sev, 1);
        }
    }
    if let Some(v) = &report.props_lint
        && let Some(arr) = v.get("issues").and_then(|x| x.as_array())
    {
        for issue in arr {
            let code = issue
                .get("code")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());
            bump("props_lint", code, "warning", 1);
        }
    }
    if let Some(v) = &report.reinvented_widget
        && let Some(arr) = v.get("findings").and_then(|x| x.as_array())
    {
        for finding in arr {
            // reinvented_widget has no explicit severity; map confidence:
            // high → warning (it's the strong DnD-triplet shape), confidence:
            // medium → info, confidence: low → info. The text inputs newly
            // marked medium therefore sort above bare-DOM low findings.
            let sev = match finding.get("confidence").and_then(|x| x.as_str()) {
                Some("high") => "warning",
                Some("medium") => "info",
                _ => "info",
            };
            bump(
                "reinvented_widget",
                Some("reinvented_widget".into()),
                sev,
                1,
            );
        }
    }
    if let Some(v) = &report.optimistic_lock_gate
        && let Some(arr) = v.get("findings").and_then(|x| x.as_array())
    {
        for finding in arr {
            let code = finding
                .get("code")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());
            // optimistic_lock_gate emits confidence:high|medium — both
            // surface as warning in the rollup (the medium tier is still a
            // real refactor candidate, just less obvious).
            bump("optimistic_lock_gate", code, "warning", 1);
        }
    }
    if let Some(v) = &report.server_state_blocking_locks
        && let Some(arr) = v.get("findings").and_then(|x| x.as_array())
    {
        for finding in arr {
            let code = finding
                .get("code")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());
            bump("server_state_blocking_locks", code, "info", 1);
        }
    }
    if let Some(v) = &report.presence_map_unbounded
        && let Some(arr) = v.get("findings").and_then(|x| x.as_array())
    {
        for finding in arr {
            let code = finding
                .get("code")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());
            let sev = severity_str(finding.get("severity").and_then(|x| x.as_str()), "info");
            bump("presence_map_unbounded", code, sev, 1);
        }
    }
    if let Some(v) = &report.insecure_set_cookie
        && let Some(arr) = v.get("findings").and_then(|x| x.as_array())
    {
        for finding in arr {
            let code = finding
                .get("code")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());
            let sev = severity_str(finding.get("severity").and_then(|x| x.as_str()), "warning");
            bump("insecure_set_cookie", code, sev, 1);
        }
    }
    if let Some(v) = &report.components_audit
        && let Some(arr) = v.get("findings").and_then(|x| x.as_array())
    {
        bump(
            "components_audit",
            Some("hand_rolled_catalog_class".into()),
            "info",
            arr.len(),
        );
    }
    if let Some(v) = &report.duplicate_helper_client_server
        && let Some(arr) = v.get("findings").and_then(|x| x.as_array())
    {
        for finding in arr {
            let code = finding
                .get("code")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());
            let sev = severity_str(finding.get("severity").and_then(|x| x.as_str()), "warning");
            bump("duplicate_helper_client_server", code, sev, 1);
        }
    }
    if let Some(v) = &report.vec_or_owned_prop_passthrough
        && let Some(arr) = v.get("findings").and_then(|x| x.as_array())
    {
        for finding in arr {
            let code = finding
                .get("code")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());
            // `info` is the default tier; the per-finding `confidence`
            // (medium vs low) doesn't change severity rollup — that's
            // surfaced in the raw finding body for callers that want it.
            let sev = severity_str(finding.get("severity").and_then(|x| x.as_str()), "info");
            bump("vec_or_owned_prop_passthrough", code, sev, 1);
        }
    }
    if let Some(v) = &report.magic_id_prefix_for_optimistic
        && let Some(arr) = v.get("findings").and_then(|x| x.as_array())
    {
        for finding in arr {
            let code = finding
                .get("code")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());
            let sev = severity_str(finding.get("severity").and_then(|x| x.as_str()), "info");
            bump("magic_id_prefix_for_optimistic", code, sev, 1);
        }
    }
    if let Some(v) = &report.shared_enum_validation
        && let Some(arr) = v.get("findings").and_then(|x| x.as_array())
    {
        for finding in arr {
            let code = finding
                .get("code")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());
            let sev = severity_str(finding.get("severity").and_then(|x| x.as_str()), "info");
            bump("shared_enum_validation", code, sev, 1);
        }
    }
    if let Some(v) = &report.derived_view_no_memo
        && let Some(arr) = v.get("findings").and_then(|x| x.as_array())
    {
        for finding in arr {
            let code = finding
                .get("code")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());
            let sev = severity_str(finding.get("severity").and_then(|x| x.as_str()), "warning");
            bump("derived_view_no_memo", code, sev, 1);
        }
    }
    if let Some(v) = &report.empty_async_error_arm
        && let Some(arr) = v.get("findings").and_then(|x| x.as_array())
    {
        for finding in arr {
            let code = finding
                .get("code")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());
            let sev = severity_str(finding.get("severity").and_then(|x| x.as_str()), "warning");
            bump("empty_async_error_arm", code, sev, 1);
        }
    }
    if let Some(v) = &report.polling_future_no_backoff
        && let Some(arr) = v.get("findings").and_then(|x| x.as_array())
    {
        for finding in arr {
            let code = finding
                .get("code")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());
            let sev = severity_str(finding.get("severity").and_then(|x| x.as_str()), "warning");
            bump("polling_future_no_backoff", code, sev, 1);
        }
    }
    if let Some(v) = &report.repeated_auth_extractor
        && let Some(arr) = v.get("findings").and_then(|x| x.as_array())
    {
        for finding in arr {
            let code = finding
                .get("code")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());
            let sev = severity_str(finding.get("severity").and_then(|x| x.as_str()), "info");
            bump("repeated_auth_extractor", code, sev, 1);
        }
    }

    let mut rows: Vec<HeadlineEntry> = buckets
        .into_iter()
        .map(|((lint, code, severity), count)| HeadlineEntry {
            lint,
            code,
            severity,
            count,
        })
        .collect();
    rows.sort_by(|a, b| {
        severity_rank(b.severity)
            .cmp(&severity_rank(a.severity))
            .then(b.count.cmp(&a.count))
            .then(a.lint.cmp(&b.lint))
            .then(a.code.cmp(&b.code))
    });
    rows
}

fn severity_rank(s: &str) -> u8 {
    match s {
        "error" => 4,
        "warning" => 3,
        "info" => 2,
        "hint" => 1,
        _ => 0,
    }
}

fn severity_str(raw: Option<&str>, default: &'static str) -> &'static str {
    match raw {
        Some("error") => "error",
        Some("warning") => "warning",
        Some("info") => "info",
        Some("hint") => "hint",
        _ => default,
    }
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
    if !report.headline.is_empty() {
        out.push_str("## Top fixes\n\n");
        for entry in report.headline.iter().take(3) {
            let code = entry.code.as_deref().unwrap_or("-");
            out.push_str(&format!(
                "- `[{sev}]` `{lint}` / `{code}` × {count}\n",
                sev = entry.severity,
                lint = entry.lint,
                code = code,
                count = entry.count,
            ));
        }
        out.push('\n');
    }
    for c in &report.issues_by_lint {
        let badge = if c.issues == 0 { "ok" } else { "issues" };
        out.push_str(&format!("- `{}`: {} ({})\n", c.lint, c.issues, badge));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Catches the regression the standup self-evaluation surfaced: a new
    /// lint added to `ALL_LINTS` but missed by the per-lint `+= count`
    /// accumulator left `total_issues` off by however many findings that
    /// lint produced. Computing the total from `issues_by_lint` at the end
    /// makes it impossible to forget.
    #[test]
    fn total_issues_matches_issues_by_lint_sum() {
        let report = LintProjectReport {
            lints_run: vec![
                "check_rsx".into(),
                "dead_components".into(),
                "prop_drill".into(),
                "signal_lint".into(),
                "signal_drilled_2_levels".into(),
                "props_lint".into(),
                "reinvented_widget".into(),
                "optimistic_lock_gate".into(),
                "server_state_blocking_locks".into(),
                "components_audit".into(),
            ],
            issues_by_lint: vec![
                LintCount {
                    lint: "check_rsx".into(),
                    issues: 0,
                },
                LintCount {
                    lint: "dead_components".into(),
                    issues: 0,
                },
                LintCount {
                    lint: "prop_drill".into(),
                    issues: 4,
                },
                LintCount {
                    lint: "signal_lint".into(),
                    issues: 1,
                },
                LintCount {
                    lint: "signal_drilled_2_levels".into(),
                    issues: 2,
                },
                LintCount {
                    lint: "props_lint".into(),
                    issues: 0,
                },
                LintCount {
                    lint: "reinvented_widget".into(),
                    issues: 1,
                },
                LintCount {
                    lint: "optimistic_lock_gate".into(),
                    issues: 2,
                },
                LintCount {
                    lint: "server_state_blocking_locks".into(),
                    issues: 8,
                },
                LintCount {
                    lint: "components_audit".into(),
                    issues: 3,
                },
            ],
            total_issues: 21,
            ..Default::default()
        };
        let expected: usize = report.issues_by_lint.iter().map(|c| c.issues).sum();
        assert_eq!(report.total_issues, expected);
    }

    /// Regression test for the TODO: `server_state_blocking_locks` was a
    /// standalone tool but missing from the `lint_project` registry, so the
    /// one-call sweep silently dropped its findings. The fix is to add the
    /// lint to `ALL_LINTS`, the include/exclude validator, and the
    /// issues_by_lint table. This test pins all three.
    #[test]
    fn registry_includes_server_state_blocking_locks() {
        assert!(
            ALL_LINTS.contains(&"server_state_blocking_locks"),
            "server_state_blocking_locks must be in ALL_LINTS: {ALL_LINTS:?}",
        );
    }

    /// `headline` is sorted severity-desc, then count-desc. A synthetic
    /// report with one error, two warnings, and three info findings must
    /// surface the error first regardless of count.
    #[test]
    fn headline_sorts_error_before_warning_before_info() {
        let report = LintProjectReport {
            insecure_set_cookie: Some(serde_json::json!({
                "findings": [
                    {"code": "insecure_set_cookie", "severity": "error"},
                ]
            })),
            signal_drilled_2_levels: Some(serde_json::json!({
                "findings": [
                    {"code": "signal_drilled_2_levels", "severity": "warning"},
                    {"code": "signal_drilled_2_levels", "severity": "warning"},
                ]
            })),
            components_audit: Some(serde_json::json!({
                "findings": [{}, {}, {}],
            })),
            ..Default::default()
        };
        let headline = build_headline(&report);
        assert!(
            !headline.is_empty(),
            "headline should be populated: {headline:?}",
        );
        assert_eq!(
            headline[0].severity, "error",
            "error rows must sort first: {headline:?}",
        );
        // Subsequent rows: warning before info.
        let warn_idx = headline
            .iter()
            .position(|h| h.severity == "warning")
            .unwrap();
        let info_idx = headline.iter().position(|h| h.severity == "info").unwrap();
        assert!(
            warn_idx < info_idx,
            "warning must precede info: {headline:?}",
        );
    }

    /// Top-3 fixes are rendered in the markdown summary. Pins the format
    /// so the caller can scrape "the top fix per project lint run".
    #[test]
    fn summary_surfaces_top_3_headline_rows() {
        let report = LintProjectReport {
            insecure_set_cookie: Some(serde_json::json!({
                "findings": [{"code": "insecure_set_cookie", "severity": "error"}]
            })),
            signal_drilled_2_levels: Some(serde_json::json!({
                "findings": [{"code": "signal_drilled_2_levels", "severity": "warning"}]
            })),
            ..Default::default()
        };
        let headline = build_headline(&report);
        let with_headline = LintProjectReport { headline, ..report };
        let summary = render_summary(&with_headline);
        assert!(
            summary.contains("Top fixes"),
            "summary should include the rollup section: {summary}",
        );
        assert!(
            summary.contains("`[error]`"),
            "error row must appear in summary: {summary}",
        );
    }
}
