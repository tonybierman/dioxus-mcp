use std::collections::HashSet;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::State;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ProjectTourParams {
    /// Sections to include. Defaults to all: ["audit", "routes", "index", "assets"].
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
    pub truncated: TruncationFlags,
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
    let included: HashSet<String> = match p.include {
        Some(v) => v.into_iter().collect(),
        None => all_sections.iter().map(|s| s.to_string()).collect(),
    };
    let excluded: HashSet<String> = p.exclude.unwrap_or_default().into_iter().collect();
    let want = |s: &str| included.contains(s) && !excluded.contains(s);

    let max = p.max_items_per_section.unwrap_or(50);

    let audit_fut = async {
        if want("audit") {
            Some(
                crate::tools::analysis::audit_feature_flags(
                    state,
                    crate::tools::analysis::AuditFeatureFlagsParams {
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
            crate::tools::route_map::route_map(
                state,
                crate::tools::route_map::RouteMapParams {
                    router_file: None,
                    project_root: p.project_root.clone(),
                },
            )
            .await
            .ok()
        } else {
            None
        }
    };

    let index_fut = async {
        if want("index") {
            crate::tools::project_index::project_index(
                state,
                crate::tools::project_index::ProjectIndexParams {
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
            crate::tools::asset_audit::asset_audit(
                state,
                crate::tools::asset_audit::AssetAuditParams {
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

    let (audit, mut routes, mut index, mut assets) =
        tokio::join!(audit_fut, routes_fut, index_fut, assets_fut);

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

    let summary = render_summary(&audit, &routes, &index, &assets, &trunc);

    Ok(ProjectTourReport {
        summary,
        audit: audit.map(|a| serde_json::to_value(a).unwrap_or(Value::Null)),
        routes: routes.map(|r| serde_json::to_value(r).unwrap_or(Value::Null)),
        index: index.map(|i| serde_json::to_value(i).unwrap_or(Value::Null)),
        assets: assets.map(|a| serde_json::to_value(a).unwrap_or(Value::Null)),
        truncated: trunc,
    })
}

fn render_summary(
    audit: &Option<crate::tools::analysis::AuditReport>,
    routes: &Option<crate::tools::route_map::RouteMapReport>,
    index: &Option<crate::tools::project_index::ProjectIndexReport>,
    assets: &Option<crate::tools::asset_audit::AssetAuditReport>,
    trunc: &TruncationFlags,
) -> String {
    let mut out = String::new();
    out.push_str("# Project tour\n\n");

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

    out
}
