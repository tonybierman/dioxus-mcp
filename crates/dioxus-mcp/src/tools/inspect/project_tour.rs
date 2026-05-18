use std::collections::HashSet;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::State;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ProjectTourParams {
    /// Sections to include. Default = the cheap set:
    /// `["audit", "routes", "index", "assets"]`.
    /// `"lints"` is an opt-in extra (runs every lint over `src/`) — pass it
    /// explicitly via `include` to add the one-line lint summary to the
    /// tour. Keep it off when you just want the quick architecture readout.
    #[serde(default)]
    pub include: Option<Vec<String>>,
    /// Sections to exclude (applied after `include`).
    #[serde(default)]
    pub exclude: Option<Vec<String>>,
    /// Truncate each section to this many items (e.g. routes, components, server fns).
    /// Defaults to 50.
    #[serde(default)]
    pub max_items_per_section: Option<usize>,
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProjectTourReport {
    pub summary: String,
    pub audit: Option<Value>,
    pub routes: Option<Value>,
    pub index: Option<Value>,
    pub assets: Option<Value>,
    /// Full `lint_project` report when `"lints"` was opted-in via the
    /// `include` param. Skipped from the JSON when absent so the default
    /// tour stays compact.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lints: Option<Value>,
    pub truncated: TruncationFlags,
    /// Concrete follow-up actions derived from audit findings — each entry
    /// pairs a short human-readable description with an executable hint
    /// (usually a small `execute_code` DSL snippet) the caller can paste
    /// directly. Empty when nothing actionable is detected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<NextAction>,
}

#[derive(Debug, Serialize)]
pub struct NextAction {
    pub title: String,
    /// One-line description of why this action is suggested.
    pub reason: String,
    /// Hint: either a Cargo.toml patch line, a tool call name, or a YAML
    /// `execute_code` snippet the caller can run.
    pub hint: String,
}

#[derive(Debug, Serialize, Default)]
pub struct TruncationFlags {
    pub routes: bool,
    pub components: bool,
    pub server_fns: bool,
    pub unreferenced_assets: bool,
}

pub async fn project_tour(
    state: &Arc<State>,
    p: ProjectTourParams,
) -> Result<ProjectTourReport, String> {
    let all_sections: Vec<&str> = vec!["audit", "routes", "index", "assets"];
    let explicit_include = p.include.is_some();
    let included: HashSet<String> = match p.include {
        Some(v) => v.into_iter().collect(),
        None => all_sections.iter().map(|s| s.to_string()).collect(),
    };
    let excluded: HashSet<String> = p.exclude.unwrap_or_default().into_iter().collect();
    let want = |s: &str| included.contains(s) && !excluded.contains(s);
    // Opt-in only: the default tour skips lints to keep the call cheap.
    // A caller that explicitly passes `include: ["..."]` and lists "lints"
    // turns it on; the absence of an `include` param leaves it off.
    let want_lints = explicit_include && want("lints");

    let max = p.max_items_per_section.unwrap_or(50);

    let audit_fut = async {
        if want("audit") {
            Some(
                crate::tools::audit::audit_feature_flags::audit_feature_flags(
                    state,
                    crate::tools::audit::audit_feature_flags::AuditFeatureFlagsParams {
                        project_root: p.project_root.clone(),
                    },
                )
                .await,
            )
        } else {
            None
        }
    };

    let routes_fut = async {
        if want("routes") {
            match crate::tools::inspect::route_map::route_map(
                state,
                crate::tools::inspect::route_map::RouteMapParams {
                    router_file: None,
                    project_root: p.project_root.clone(),
                },
            )
            .await
            {
                Ok(rm) => (Some(rm), None),
                Err(e) => (None, Some(e)),
            }
        } else {
            (None, None)
        }
    };

    let index_fut = async {
        if want("index") {
            crate::tools::inspect::project_index::project_index(
                state,
                crate::tools::inspect::project_index::ProjectIndexParams {
                    path: None,
                    kind: None,
                    project_root: p.project_root.clone(),
                },
            )
            .await
            .ok()
        } else {
            None
        }
    };

    let assets_fut = async {
        if want("assets") {
            crate::tools::audit::asset_audit::asset_audit(
                state,
                crate::tools::audit::asset_audit::AssetAuditParams {
                    assets_dirs: None,
                    project_root: p.project_root.clone(),
                },
            )
            .await
            .ok()
        } else {
            None
        }
    };

    // Lint run is opt-in (multi-pass static analysis is the most expensive
    // section). When skipped we don't even resolve the future, so the cost
    // is zero. `LintProjectParams { include: None, .. }` runs every lint.
    let lints_fut = async {
        if want_lints {
            crate::tools::lints::lint_project::lint_project(
                state,
                crate::tools::lints::lint_project::LintProjectParams {
                    include: None,
                    exclude: None,
                    dead_component_roots: None,
                    project_root: p.project_root.clone(),
                },
            )
            .await
            .ok()
        } else {
            None
        }
    };

    let (audit, (mut routes, routes_err), mut index, mut assets, lints) =
        tokio::join!(audit_fut, routes_fut, index_fut, assets_fut, lints_fut);

    let mut trunc = TruncationFlags::default();
    if let Some(rm) = routes.as_mut()
        && rm.routes.len() > max
    {
        rm.routes.truncate(max);
        trunc.routes = true;
    }
    if let Some(idx) = index.as_mut() {
        if idx.components.len() > max {
            idx.components.truncate(max);
            trunc.components = true;
        }
        if idx.server_fns.len() > max {
            idx.server_fns.truncate(max);
            trunc.server_fns = true;
        }
    }
    if let Some(aa) = assets.as_mut()
        && aa.unreferenced_files.len() > max
    {
        aa.unreferenced_files.truncate(max);
        trunc.unreferenced_assets = true;
    }

    // A caller that asked for `include: ["lints"]` and nothing else gets a
    // markdown header that names the narrower scope, so they don't have to
    // second-guess whether the rest of the report was dropped on the floor
    // or intentionally skipped.
    let only_lints = explicit_include
        && want("lints")
        && !["audit", "routes", "index", "assets"]
            .iter()
            .any(|s| want(s));
    let summary = render_summary(&audit, &routes, &index, &assets, &lints, &trunc, only_lints);
    let next_actions = derive_next_actions(&audit, &routes, routes_err.as_deref(), &index);

    Ok(ProjectTourReport {
        summary,
        audit: audit.map(|a| serde_json::to_value(a).unwrap_or(Value::Null)),
        routes: routes.map(|r| serde_json::to_value(r).unwrap_or(Value::Null)),
        index: index.map(|i| serde_json::to_value(i).unwrap_or(Value::Null)),
        assets: assets.map(|a| serde_json::to_value(a).unwrap_or(Value::Null)),
        lints: lints.map(|l| serde_json::to_value(l).unwrap_or(Value::Null)),
        truncated: trunc,
        next_actions,
    })
}

/// Convert audit findings (and a couple of "empty project" cases) into
/// concrete `next_actions`. Intentionally conservative — only the high-signal,
/// fix-is-obvious cases get surfaced. Anything that needs human judgment (e.g.
/// "pick a render target") stays in `audit.findings` for the caller to read.
fn derive_next_actions(
    audit: &Option<crate::tools::audit::audit_feature_flags::AuditReport>,
    routes: &Option<crate::tools::inspect::route_map::RouteMapReport>,
    routes_err: Option<&str>,
    index: &Option<crate::tools::inspect::project_index::ProjectIndexReport>,
) -> Vec<NextAction> {
    let mut out: Vec<NextAction> = Vec::new();

    // Tool-level partial failure: route_map errored. Surface the error so the
    // caller knows the `routes` section is missing for a reason other than
    // "no router".
    if let Some(err) = routes_err {
        out.push(NextAction {
            title: "route_map failed".into(),
            reason: err.to_string(),
            hint:
                "re-run `route_map` directly with `router_file: \"...\"` to point at the right file"
                    .into(),
        });
    }

    // Degraded result: tool succeeded but reported a note (e.g. no Routable
    // enum). Pass the note through as a next-action so the caller doesn't have
    // to inspect the nested struct to spot it.
    if let Some(r) = routes
        && let Some(note) = &r.note
    {
        out.push(NextAction {
            title: "routes section is empty".into(),
            reason: note.clone(),
            hint: "if the app uses server-side routing, ignore; otherwise scaffold a `Routable` enum with `execute_code`"
                .into(),
        });
    }

    if let Some(a) = audit {
        for f in &a.findings {
            let msg = f.message.as_str();
            if msg.contains("`fullstack` is enabled but `web` is not") {
                out.push(NextAction {
                    title: "Enable `web` on the dioxus dep".into(),
                    reason: msg.to_string(),
                    hint: "Cargo.toml: add `\"web\"` to the dioxus dep's features array".into(),
                });
            } else if msg.contains("`fullstack` is enabled but `server` is not") {
                out.push(NextAction {
                    title: "Enable `server` on the dioxus dep".into(),
                    reason: msg.to_string(),
                    hint: "Cargo.toml: add `\"server\"` to the dioxus dep's features array".into(),
                });
            } else if msg.contains("no platform feature enabled on the `dioxus` dep") {
                out.push(NextAction {
                    title: "Pick a render target".into(),
                    reason: msg.to_string(),
                    hint: "Cargo.toml: set `features = [\"web\"]` (or desktop/mobile/fullstack) on the dioxus dep".into(),
                });
            } else if msg
                .contains("multiple render targets enabled simultaneously without `fullstack`")
            {
                out.push(NextAction {
                    title: "Resolve render-target conflict".into(),
                    reason: msg.to_string(),
                    hint: "Cargo.toml: keep exactly one of web/desktop/mobile on the dioxus dep, or switch to `\"fullstack\"`".into(),
                });
            } else if msg.contains("activates both render targets at once") {
                out.push(NextAction {
                    title: "Trim the [features] default".into(),
                    reason: msg.to_string(),
                    hint: "Cargo.toml: set `default = [\"web\"]` (or `[\"server\"]`) and pass the other via `--features` when needed".into(),
                });
            }
        }
    }
    // Empty-project hook: when neither components nor server fns exist, suggest
    // the canonical scaffolding entry point.
    if let Some(i) = index
        && i.components.is_empty()
        && i.server_fns.is_empty()
    {
        out.push(NextAction {
            title: "Scaffold a starting slice".into(),
            reason: "no components or server fns yet".into(),
            hint: "call `get_dsl_spec { index_only: true }`, pick the primitives you need (model + client_store + screen, or a resource bundle), then call `execute_code` with the YAML".into(),
        });
    }
    out
}

/// Maps a lint id (`signal_lint`, `prop_drill`, …) to a short, human-friendly
/// label used in the one-line tour summary. Kept tiny on purpose — when a
/// caller wants the full count breakdown they read the `lints` field
/// directly. The label drops the `_lint` suffix for brevity.
fn short_lint_label(lint: &str) -> &'static str {
    match lint {
        "check_rsx" => "rsx",
        "dead_components" => "dead",
        "prop_drill" => "prop drill",
        "signal_lint" => "signal",
        "props_lint" => "props",
        "reinvented_widget" => "reinvented",
        "optimistic_lock_gate" => "opt-lock",
        _ => "other",
    }
}

fn render_summary(
    audit: &Option<crate::tools::audit::audit_feature_flags::AuditReport>,
    routes: &Option<crate::tools::inspect::route_map::RouteMapReport>,
    index: &Option<crate::tools::inspect::project_index::ProjectIndexReport>,
    assets: &Option<crate::tools::audit::asset_audit::AssetAuditReport>,
    lints: &Option<crate::tools::lints::lint_project::LintProjectReport>,
    trunc: &TruncationFlags,
    only_lints: bool,
) -> String {
    let mut out = String::new();
    // Narrower header when the caller asked for lints-only — otherwise the
    // "# Project tour" title with an otherwise empty body is misleading
    // (looks like other sections silently failed).
    if only_lints {
        out.push_str("# Project lints\n\n");
    } else {
        out.push_str("# Project tour\n\n");
    }

    if let Some(a) = audit {
        out.push_str(&format!(
            "**Dioxus**: {} | platform features: {}\n",
            a.dioxus_version.clone().unwrap_or_else(|| "?".into()),
            if a.dioxus_features.is_empty() {
                "(none)".to_string()
            } else {
                a.dioxus_features.join(", ")
            }
        ));
        if !a.findings.is_empty() {
            out.push_str(&format!("- audit findings: {}\n", a.findings.len()));
        }
    }

    if let Some(r) = routes {
        let suffix = if trunc.routes { " (truncated)" } else { "" };
        if let Some(note) = &r.note {
            out.push_str(&format!("**Routes** (0): {note}\n"));
        } else {
            out.push_str(&format!(
                "**Routes** ({}{}): enum `{}`\n",
                r.routes.len(),
                suffix,
                r.enum_name
            ));
            for route in r.routes.iter().take(10) {
                out.push_str(&format!(
                    "  - `{}` → `{}`\n",
                    route.full_path, route.component
                ));
            }
        }
    }

    if let Some(i) = index {
        let cs = if trunc.components { " (truncated)" } else { "" };
        let fs = if trunc.server_fns { " (truncated)" } else { "" };
        out.push_str(&format!(
            "**Components**: {}{} | **Server fns**: {}{}\n",
            i.components.len(),
            cs,
            i.server_fns.len(),
            fs
        ));
    }

    if let Some(a) = assets {
        out.push_str(&format!(
            "**Assets**: {} files, {} referenced, {} unreferenced{}, {} missing\n",
            a.total_files,
            a.referenced_count,
            a.unreferenced_files.len(),
            if trunc.unreferenced_assets {
                " (truncated)"
            } else {
                ""
            },
            a.missing_assets.len()
        ));
    }

    if let Some(l) = lints {
        // Per-lint breakdown of only the lints that found at least one
        // issue — keeps the summary line short on clean projects ("Lints:
        // 0 issues") and dense on dirty ones ("Lints: 6 issues (1 signal,
        // 4 prop drill, 1 reinvented)"). Short labels match the lint name.
        let parts: Vec<String> = l
            .issues_by_lint
            .iter()
            .filter(|c| c.issues > 0)
            .map(|c| format!("{} {}", c.issues, short_lint_label(&c.lint)))
            .collect();
        let detail = if parts.is_empty() {
            String::new()
        } else {
            format!(" ({})", parts.join(", "))
        };
        out.push_str(&format!("**Lints**: {} issues{detail}\n", l.total_issues));
    }

    if let Some(i) = index
        && i.components.is_empty()
        && i.server_fns.is_empty()
    {
        out.push_str(
            "\n> This project has no components or server fns yet. To scaffold \
             anything in a Dioxus 0.7 project — a model, a screen, a server fn, \
             or a full CRUD slice (model + store + server fns + screens) — call \
             `get_dsl_spec` then `execute_code`.\n",
        );
    }

    // Cross-reference: the structured `next_actions` field carries the
    // executable hints; the summary just points at them so a human-only
    // reader knows they exist.
    let actions = derive_next_actions(audit, routes, None, index);
    if !actions.is_empty() {
        out.push_str(&format!(
            "\n**Next actions** ({}): see `next_actions` for paste-ready hints.\n",
            actions.len()
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::lints::lint_project::{LintCount, LintProjectReport};

    /// Standup's actual lint counts (from the TODO header):
    /// `check_rsx 0 + dead_components 0 + prop_drill 4 + signal_lint 1 +
    /// props_lint 0 + reinvented_widget 1` = 6 total. The tour summary
    /// must surface the breakdown ("4 prop drill, 1 signal, 1 reinvented")
    /// and keep the headline number consistent with `total_issues`.
    #[test]
    fn lint_summary_one_liner_matches_per_lint_breakdown() {
        let lint_report = LintProjectReport {
            summary: String::new(),
            lints_run: vec![
                "check_rsx".into(),
                "dead_components".into(),
                "prop_drill".into(),
                "signal_lint".into(),
                "props_lint".into(),
                "reinvented_widget".into(),
            ],
            total_issues: 6,
            headline: Vec::new(),
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
                    lint: "props_lint".into(),
                    issues: 0,
                },
                LintCount {
                    lint: "reinvented_widget".into(),
                    issues: 1,
                },
            ],
            parse_errors: Vec::new(),
            check_rsx: None,
            dead_components: None,
            prop_drill: None,
            signal_lint: None,
            signal_drilled_2_levels: None,
            props_lint: None,
            reinvented_widget: None,
            optimistic_lock_gate: None,
            server_state_blocking_locks: None,
            presence_map_unbounded: None,
            insecure_set_cookie: None,
            components_audit: None,
        };
        let trunc = TruncationFlags::default();
        let summary = render_summary(
            &None,
            &None,
            &None,
            &None,
            &Some(lint_report),
            &trunc,
            false,
        );
        assert!(
            summary.contains("**Lints**: 6 issues"),
            "headline should match total_issues: {summary}"
        );
        // Per-lint detail uses the short labels and includes only lints
        // that found at least one issue.
        assert!(
            summary.contains("4 prop drill"),
            "should include prop drill count: {summary}"
        );
        assert!(
            summary.contains("1 signal"),
            "should include signal count: {summary}"
        );
        assert!(
            summary.contains("1 reinvented"),
            "should include reinvented count: {summary}"
        );
        // Zero-issue lints stay OUT of the breakdown so the line stays
        // readable on a clean project.
        assert!(
            !summary.contains("0 rsx") && !summary.contains("0 props"),
            "zero-issue lints must not appear in the breakdown: {summary}"
        );
    }

    /// Clean project (no findings) renders a single "0 issues" line with no
    /// trailing `(…)` — keeps the tour markdown compact when there's
    /// nothing to call out.
    #[test]
    fn lint_summary_clean_project_has_no_trailing_paren() {
        let lint_report = LintProjectReport {
            summary: String::new(),
            lints_run: vec!["signal_lint".into()],
            total_issues: 0,
            headline: Vec::new(),
            issues_by_lint: vec![LintCount {
                lint: "signal_lint".into(),
                issues: 0,
            }],
            parse_errors: Vec::new(),
            check_rsx: None,
            dead_components: None,
            prop_drill: None,
            signal_lint: None,
            signal_drilled_2_levels: None,
            props_lint: None,
            reinvented_widget: None,
            optimistic_lock_gate: None,
            server_state_blocking_locks: None,
            presence_map_unbounded: None,
            insecure_set_cookie: None,
            components_audit: None,
        };
        let trunc = TruncationFlags::default();
        let summary = render_summary(
            &None,
            &None,
            &None,
            &None,
            &Some(lint_report),
            &trunc,
            false,
        );
        assert!(
            summary.contains("**Lints**: 0 issues\n"),
            "clean project line should be bare: {summary}"
        );
        assert!(
            !summary.contains("**Lints**: 0 issues ("),
            "no trailing breakdown when nothing fired: {summary}"
        );
    }

    /// When no lint report is provided (the default tour, with lints
    /// opt-out), the summary must not emit a `**Lints**:` line at all.
    #[test]
    fn lint_summary_omitted_when_lints_not_run() {
        let trunc = TruncationFlags::default();
        let summary = render_summary(&None, &None, &None, &None, &None, &trunc, false);
        assert!(
            !summary.contains("**Lints**"),
            "no lints section when the tour didn't run them: {summary}"
        );
    }

    /// When the caller scoped the tour to `include: ["lints"]` only, the
    /// markdown header should reflect the narrower scope so a reader
    /// doesn't think the empty audit/routes/index/assets sections were
    /// silently dropped on the floor.
    #[test]
    fn lints_only_summary_renames_header() {
        let lint_report = LintProjectReport {
            summary: String::new(),
            lints_run: vec!["signal_lint".into()],
            total_issues: 0,
            headline: Vec::new(),
            issues_by_lint: vec![LintCount {
                lint: "signal_lint".into(),
                issues: 0,
            }],
            parse_errors: Vec::new(),
            check_rsx: None,
            dead_components: None,
            prop_drill: None,
            signal_lint: None,
            signal_drilled_2_levels: None,
            props_lint: None,
            reinvented_widget: None,
            optimistic_lock_gate: None,
            server_state_blocking_locks: None,
            presence_map_unbounded: None,
            insecure_set_cookie: None,
            components_audit: None,
        };
        let trunc = TruncationFlags::default();
        let summary = render_summary(&None, &None, &None, &None, &Some(lint_report), &trunc, true);
        assert!(
            summary.starts_with("# Project lints\n"),
            "lints-only scope should rename the header: {summary}"
        );
        assert!(
            !summary.contains("# Project tour"),
            "should not also include the full-tour header: {summary}"
        );
    }
}
