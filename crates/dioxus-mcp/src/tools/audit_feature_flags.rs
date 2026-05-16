use std::path::PathBuf;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::state::State;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AuditFeatureFlagsParams {
    /// Absolute path to the Dioxus project root to inspect.
    /// Defaults to the path the MCP server was started in when omitted.
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Finding {
    pub level: &'static str, // "error" | "warning" | "info"
    pub message: String,
    pub fix: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AuditReport {
    pub ok: bool,
    pub manifest: Option<PathBuf>,
    pub dioxus_version: Option<String>,
    pub dioxus_features: Vec<String>,
    pub has_dioxus_toml: bool,
    pub findings: Vec<Finding>,
}

const PLATFORM_FEATURES: &[&str] = &[
    "web",
    "desktop",
    "mobile",
    "fullstack",
    "server",
    "static-generation",
    "ssr",
];

pub async fn audit_feature_flags(state: &Arc<State>, p: AuditFeatureFlagsParams) -> AuditReport {
    let project = match p.project_root.as_deref() {
        Some(root) => crate::project::ProjectInfo::detect(std::path::Path::new(root)),
        None => state.project.lock().await.clone(),
    };
    let mut findings = Vec::new();

    let Some(manifest) = project.manifest_path.clone() else {
        findings.push(Finding {
            level: "error",
            message: "no Cargo.toml with a `dioxus` dependency was found from the project root"
                .into(),
            fix: Some("run dioxus-mcp with --project-root pointing at a Dioxus crate".into()),
        });
        return AuditReport {
            ok: false,
            manifest: None,
            dioxus_version: None,
            dioxus_features: vec![],
            has_dioxus_toml: false,
            findings,
        };
    };

    if !project.is_dioxus_project {
        findings.push(Finding {
            level: "error",
            message: "manifest does not list `dioxus` as a dependency".into(),
            fix: None,
        });
    }

    // Version sanity
    match project.version_major_minor() {
        Some((0, 7)) => {}
        Some((maj, min)) => findings.push(Finding {
            level: "warning",
            message: format!(
                "detected Dioxus {maj}.{min}; this MCP ships templates and rules for 0.7"
            ),
            fix: Some("upgrade Dioxus to 0.7.x for best results".into()),
        }),
        None => findings.push(Finding {
            level: "warning",
            message: "could not parse the Dioxus version from Cargo.toml".into(),
            fix: None,
        }),
    }

    // Active platform features on the dioxus dep — both directly via
    // `features = [...]` on the dep line AND transitively via the project's
    // own `[features]` table (e.g. `web = ["dioxus/web"]` activated by
    // `default = ["web"]`). Without the [features] walk, projects using
    // cargo feature unification flag false positives like "fullstack
    // enabled but web is not" when web *is* enabled through default.
    let manifest_text = std::fs::read_to_string(&manifest).ok();
    let effective_dioxus_features =
        collect_effective_dioxus_features(&project.dioxus_features, manifest_text.as_deref());
    let active: Vec<&str> = effective_dioxus_features
        .iter()
        .map(|s| s.as_str())
        .filter(|f| PLATFORM_FEATURES.contains(f))
        .collect();

    let has_fullstack = active.contains(&"fullstack");
    let render_targets: Vec<&str> = active
        .iter()
        .copied()
        .filter(|f| matches!(*f, "web" | "desktop" | "mobile"))
        .collect();

    if has_fullstack {
        if !render_targets.contains(&"web") {
            findings.push(Finding {
                level: "warning",
                message: "`fullstack` is enabled but `web` is not — fullstack typically pairs `web` (client) with `server`".into(),
                fix: Some("add `web` to the dioxus features".into()),
            });
        }
        if !active.contains(&"server") {
            findings.push(Finding {
                level: "warning",
                message: "`fullstack` is enabled but `server` is not".into(),
                fix: Some("add `server` to the dioxus features".into()),
            });
        }
    } else if render_targets.len() > 1 {
        findings.push(Finding {
            level: "error",
            message: format!(
                "multiple render targets enabled simultaneously without `fullstack`: {render_targets:?}"
            ),
            fix: Some(
                "pick exactly one of web/desktop/mobile, or enable `fullstack` instead"
                    .into(),
            ),
        });
    } else if render_targets.is_empty() && !active.contains(&"server") {
        findings.push(Finding {
            level: "warning",
            message: "no platform feature enabled on the `dioxus` dep".into(),
            fix: Some(
                "select a platform: features = [\"web\"] (or desktop/mobile/fullstack)".into(),
            ),
        });
    }

    // [features] default = ["web", "server"] footgun
    if let Some(text) = manifest_text.as_deref()
        && let Ok(parsed) = text.parse::<toml::Table>()
        && let Some(features) = parsed.get("features").and_then(|v| v.as_table())
        && let Some(default) = features.get("default").and_then(|v| v.as_array())
    {
        let names: Vec<String> = default
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        let has_web = names.iter().any(|n| n == "web");
        let has_server = names.iter().any(|n| n == "server");
        if has_web && has_server {
            findings.push(Finding {
                            level: "warning",
                            message: "[features] default = [\"web\", \"server\"] activates both render targets at once — a common source of build confusion".into(),
                            fix: Some("set default = [\"web\"] (or [\"server\"]) and pass the other via --features when needed".into()),
                        });
        }
    }

    let ok = !findings.iter().any(|f| f.level == "error");
    AuditReport {
        ok,
        manifest: Some(manifest),
        dioxus_version: project.dioxus_version,
        dioxus_features: effective_dioxus_features,
        has_dioxus_toml: project.has_dioxus_toml,
        findings,
    }
}

/// Compute the effective set of dioxus features for the project, starting
/// from features set directly on the `dioxus` dep line and walking the
/// project's own `[features]` table to follow `dioxus/<name>` indirections.
///
/// Cargo activates `default` automatically (unless `default-features = false`
/// on a downstream crate — but we're inspecting the project itself, so default
/// is in scope). Any named feature reachable from `default` whose value list
/// contains `dioxus/X` contributes `X` to the effective set.
fn collect_effective_dioxus_features(
    direct_dep_features: &[String],
    manifest_text: Option<&str>,
) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut effective: BTreeSet<String> = direct_dep_features.iter().cloned().collect();
    let Some(text) = manifest_text else {
        return effective.into_iter().collect();
    };
    let Ok(parsed) = text.parse::<toml::Table>() else {
        return effective.into_iter().collect();
    };
    let Some(features) = parsed.get("features").and_then(|v| v.as_table()) else {
        return effective.into_iter().collect();
    };

    // Seed the work queue with `default` (cargo activates it by default for
    // path-level builds, which is what `dx serve` and `cargo build` do).
    let mut work: Vec<String> = Vec::new();
    if let Some(default) = features.get("default").and_then(|v| v.as_array()) {
        for v in default {
            if let Some(s) = v.as_str() {
                work.push(s.to_string());
            }
        }
    }

    let mut seen: BTreeSet<String> = BTreeSet::new();
    while let Some(name) = work.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        // Accept both `dioxus/web` and the weak-dep form `dioxus?/web`.
        if let Some(stripped) = name
            .strip_prefix("dioxus/")
            .or_else(|| name.strip_prefix("dioxus?/"))
        {
            effective.insert(stripped.to_string());
            continue;
        }
        if let Some(arr) = features.get(name.as_str()).and_then(|v| v.as_array()) {
            for v in arr {
                if let Some(s) = v.as_str() {
                    work.push(s.to_string());
                }
            }
        }
    }

    effective.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::collect_effective_dioxus_features;

    #[test]
    fn picks_up_features_routed_through_project_features_table() {
        // A typical `dx new` fullstack starter: dioxus dep declares only
        // `fullstack`, with web/server enabled indirectly through the
        // project's own [features] table. The audit must follow the chain so
        // it doesn't falsely warn that web/server are missing.
        let manifest = r#"[package]
name = "starter"
version = "0.1.0"
edition = "2024"

[features]
default = ["web"]
web = ["dioxus/web"]
server = ["dioxus/server"]

[dependencies]
dioxus = { version = "0.7", features = ["fullstack"] }
"#;
        let effective =
            collect_effective_dioxus_features(&["fullstack".to_string()], Some(manifest));
        assert!(
            effective.contains(&"fullstack".to_string()),
            "{effective:?}"
        );
        assert!(effective.contains(&"web".to_string()), "{effective:?}");
        // `server` should NOT appear: cargo only activates default features
        // and `server` isn't in default. The walk is correct in skipping it.
        assert!(!effective.contains(&"server".to_string()), "{effective:?}");
    }

    #[test]
    fn handles_default_chains_with_intermediate_features() {
        // `default → ssr → dioxus/server` (intermediate feature without a
        // direct dioxus/* entry) — the walker should still resolve it.
        let manifest = r#"[features]
default = ["ssr"]
ssr = ["with_server"]
with_server = ["dioxus/server"]

[dependencies]
dioxus = "0.7"
"#;
        let effective = collect_effective_dioxus_features(&[], Some(manifest));
        assert!(effective.contains(&"server".to_string()), "{effective:?}");
    }

    #[test]
    fn handles_weak_dep_marker() {
        // `dioxus?/web` weak-feature syntax should still contribute `web`.
        let manifest = r#"[features]
default = ["web"]
web = ["dioxus?/web"]

[dependencies]
dioxus = { version = "0.7" }
"#;
        let effective = collect_effective_dioxus_features(&[], Some(manifest));
        assert!(effective.contains(&"web".to_string()), "{effective:?}");
    }

    #[test]
    fn passes_through_direct_dep_features_when_no_features_table() {
        let manifest = r#"[dependencies]
dioxus = { version = "0.7", features = ["fullstack", "web"] }
"#;
        let effective = collect_effective_dioxus_features(
            &["fullstack".to_string(), "web".to_string()],
            Some(manifest),
        );
        assert!(
            effective.contains(&"fullstack".to_string()),
            "{effective:?}"
        );
        assert!(effective.contains(&"web".to_string()), "{effective:?}");
    }
}
