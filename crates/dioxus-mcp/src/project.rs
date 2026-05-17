use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ProjectInfo {
    pub manifest_path: Option<PathBuf>,
    pub package_name: Option<String>,
    pub dioxus_version: Option<String>,
    /// Features listed directly on the `dioxus` dep line.
    pub dioxus_features: Vec<String>,
    /// `dioxus_features` plus anything reachable through the project's own
    /// `[features]` table (e.g. `web = ["dioxus/web"]` activated by
    /// `default = ["web"]`). Use this for "is the project fullstack-capable"
    /// preflight checks so they line up with audit_feature_flags.
    #[serde(default)]
    pub effective_dioxus_features: Vec<String>,
    pub has_dioxus_toml: bool,
    pub is_dioxus_project: bool,
}

impl ProjectInfo {
    pub fn detect(start: &Path) -> Self {
        let manifest = find_manifest_with_dioxus(start);
        let mut info = ProjectInfo {
            manifest_path: manifest.clone(),
            package_name: None,
            dioxus_version: None,
            dioxus_features: Vec::new(),
            effective_dioxus_features: Vec::new(),
            has_dioxus_toml: false,
            is_dioxus_project: false,
        };

        let Some(path) = manifest else { return info };

        info.has_dioxus_toml = path
            .parent()
            .map(|p| p.join("dioxus.toml").exists())
            .unwrap_or(false);

        if let Ok(manifest) = cargo_toml::Manifest::from_path(&path) {
            info.package_name = manifest.package.as_ref().map(|p| p.name.clone());
            if let Some(dep) = manifest.dependencies.get("dioxus") {
                info.is_dioxus_project = true;
                info.dioxus_version = dep.req().to_string().into();
                info.dioxus_features = dep.req_features().to_vec();
            }
        }
        let manifest_text = std::fs::read_to_string(&path).ok();
        info.effective_dioxus_features =
            effective_dioxus_features(&info.dioxus_features, manifest_text.as_deref());
        info
    }

    /// True when the project is configured to compile server-side code —
    /// either via `fullstack` directly, or via `web` + `server` together,
    /// or via an opt-in `server = ["dioxus/server"]` sibling feature (the
    /// canonical 0.7 layout where the server binary builds with
    /// `--features server`).
    pub fn fullstack_capable(&self) -> bool {
        let eff: Vec<&str> = self
            .effective_dioxus_features
            .iter()
            .map(|s| s.as_str())
            .collect();
        if eff.contains(&"fullstack") {
            return true;
        }
        if eff.contains(&"server") && eff.contains(&"web") {
            return true;
        }
        // Canonical 0.7 fullstack opt-in: fullstack on the dioxus dep,
        // `default = ["web"]`, and a sibling `server = ["dioxus/server"]`
        // feature the server binary turns on at build time. The effective
        // graph for `default` doesn't include `server`, but the wiring is
        // there.
        let fullstack_anywhere =
            eff.contains(&"fullstack") || self.dioxus_features.iter().any(|f| f == "fullstack");
        if fullstack_anywhere
            && let Some(path) = &self.manifest_path
            && let Ok(text) = std::fs::read_to_string(path)
            && manifest_has_optin_server_feature(&text)
        {
            return true;
        }
        false
    }

    pub fn manifest_dir(&self) -> Option<PathBuf> {
        self.manifest_path
            .as_ref()
            .and_then(|p| p.parent().map(PathBuf::from))
    }

    pub fn version_major_minor(&self) -> Option<(u64, u64)> {
        let v = self.dioxus_version.as_deref()?;
        let cleaned = v.trim_start_matches(|c: char| !c.is_ascii_digit());
        let mut parts = cleaned.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        Some((major, minor))
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
pub fn effective_dioxus_features(
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

/// True when the project's `[features]` table has any feature whose array
/// includes `dioxus/server` (or `dioxus?/server`). That's the canonical 0.7
/// fullstack opt-in shape: the server binary is launched with
/// `--features server`, so the feature isn't in `default` but the wiring is
/// there.
pub fn manifest_has_optin_server_feature(manifest_text: &str) -> bool {
    let Ok(parsed) = manifest_text.parse::<toml::Table>() else {
        return false;
    };
    let Some(features) = parsed.get("features").and_then(|v| v.as_table()) else {
        return false;
    };
    features.iter().any(|(_name, value)| {
        let Some(arr) = value.as_array() else {
            return false;
        };
        arr.iter().any(|v| {
            let s = v.as_str().unwrap_or("");
            s == "dioxus/server" || s == "dioxus?/server"
        })
    })
}

fn find_manifest_with_dioxus(start: &Path) -> Option<PathBuf> {
    let mut cur = if start.is_file() {
        start.parent()?
    } else {
        start
    };
    loop {
        let candidate = cur.join("Cargo.toml");
        if candidate.exists() {
            if let Ok(m) = cargo_toml::Manifest::from_path(&candidate)
                && m.dependencies.contains_key("dioxus")
            {
                return Some(candidate);
            }
            // if we hit a non-dioxus manifest at the workspace root, still note it
            if cur.parent().is_none() {
                return Some(candidate);
            }
        }
        cur = cur.parent()?;
    }
}
