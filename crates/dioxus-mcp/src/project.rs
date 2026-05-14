use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ProjectInfo {
    pub manifest_path: Option<PathBuf>,
    pub package_name: Option<String>,
    pub dioxus_version: Option<String>,
    pub dioxus_features: Vec<String>,
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
        info
    }

    pub fn manifest_dir(&self) -> Option<PathBuf> {
        self.manifest_path.as_ref().and_then(|p| p.parent().map(PathBuf::from))
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

fn find_manifest_with_dioxus(start: &Path) -> Option<PathBuf> {
    let mut cur = if start.is_file() { start.parent()? } else { start };
    loop {
        let candidate = cur.join("Cargo.toml");
        if candidate.exists() {
            if let Ok(m) = cargo_toml::Manifest::from_path(&candidate) {
                if m.dependencies.contains_key("dioxus") {
                    return Some(candidate);
                }
            }
            // if we hit a non-dioxus manifest at the workspace root, still note it
            if cur.parent().is_none() {
                return Some(candidate);
            }
        }
        cur = cur.parent()?;
    }
}
