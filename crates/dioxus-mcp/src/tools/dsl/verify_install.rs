//! `verify_install`: inspect a project's wiring for `dx components add` output.
//!
//! After `dx components add <name>` runs, two one-time edits must land in the
//! user's crate before the new module compiles cleanly:
//!
//!   1. `mod components;` in `src/main.rs` (or `src/lib.rs`).
//!   2. The catalog theme stylesheet mounted via
//!      `asset!("/assets/dx-components-theme.css")` — typically in the `App`
//!      component body.
//!
//! `dx` prints these reminders to stdout but agents that don't capture CLI
//! output (or that only inspect the file system) miss them. This tool reads
//! the project and reports which steps are still missing, with the exact
//! lines to add — so the agent can finish wiring without re-running `dx`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::state::State;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct VerifyInstallParams {
    /// Optional project root override. Defaults to the detected manifest dir.
    #[serde(default)]
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct VerifyInstallStep {
    /// Stable short id (`mod_components` / `theme_asset` / `components_dir`).
    /// Lets callers branch on a specific step without string-matching titles.
    pub id: &'static str,
    /// Human-readable title.
    pub title: &'static str,
    /// `true` when the step is wired correctly, `false` when action is needed.
    pub ok: bool,
    /// Where the check looked (the file or directory it inspected). When the
    /// step is `ok: true`, this is the path where the wiring was found.
    pub looked_in: Vec<PathBuf>,
    /// The line(s) to add when `ok: false`, ready to paste. Empty when `ok`.
    pub fix: Option<String>,
    /// Free-form hint about *where* to paste the fix (e.g. "top of src/main.rs",
    /// "inside the rsx! body of `App`"). Empty when `ok`.
    pub fix_location: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct VerifyInstallReport {
    /// Absolute crate root the checks ran against.
    pub project_root: PathBuf,
    /// `true` when every step is `ok` — the catalog is fully wired.
    pub fully_wired: bool,
    /// Per-step results in stable order.
    pub steps: Vec<VerifyInstallStep>,
    /// Convenience: short titles of every step where `ok: false`. Lets a
    /// caller scan the report without iterating `steps`.
    pub missing: Vec<&'static str>,
}

pub async fn verify_install(
    state: &Arc<State>,
    p: VerifyInstallParams,
) -> Result<VerifyInstallReport, String> {
    let crate_root = resolve_crate_root(state, p.project_root.as_deref()).await?;
    let src = crate_root.join("src");
    let main_rs = src.join("main.rs");
    let lib_rs = src.join("lib.rs");

    let mod_step = check_mod_components(&main_rs, &lib_rs);
    let theme_step = check_theme_asset(&src);
    let dir_step = check_components_dir(&src.join("components"));

    let steps = vec![mod_step, theme_step, dir_step];
    let missing: Vec<&'static str> = steps.iter().filter(|s| !s.ok).map(|s| s.id).collect();
    let fully_wired = missing.is_empty();
    Ok(VerifyInstallReport {
        project_root: crate_root,
        fully_wired,
        steps,
        missing,
    })
}

async fn resolve_crate_root(
    state: &Arc<State>,
    override_: Option<&str>,
) -> Result<PathBuf, String> {
    if let Some(root) = override_ {
        let info = crate::project::ProjectInfo::detect(Path::new(root));
        return info
            .manifest_dir()
            .ok_or_else(|| format!("no Cargo.toml found at or above `{root}`"));
    }
    let info = state.project.lock().await;
    info.manifest_dir()
        .ok_or_else(|| "no Cargo.toml found in the detected project root".into())
}

fn check_mod_components(main_rs: &Path, lib_rs: &Path) -> VerifyInstallStep {
    // Either main.rs or lib.rs satisfies the check — bin and lib crates wire
    // the module declaration in different files. We look in both so a lib-only
    // crate doesn't fail just because main.rs is absent.
    let candidates = [main_rs, lib_rs];
    let hits: Vec<PathBuf> = candidates
        .iter()
        .filter(|p| p.exists())
        .filter(|p| {
            std::fs::read_to_string(p)
                .map(|s| has_mod_components_decl(&s))
                .unwrap_or(false)
        })
        .map(|p| p.to_path_buf())
        .collect();
    let looked_in: Vec<PathBuf> = candidates
        .iter()
        .filter(|p| p.exists())
        .map(|p| p.to_path_buf())
        .collect();
    if !hits.is_empty() {
        VerifyInstallStep {
            id: "mod_components",
            title: "`mod components;` declared in crate root",
            ok: true,
            looked_in: hits,
            fix: None,
            fix_location: None,
        }
    } else {
        VerifyInstallStep {
            id: "mod_components",
            title: "`mod components;` declared in crate root",
            ok: false,
            looked_in,
            fix: Some("mod components;".into()),
            fix_location: Some(
                "near the top of src/main.rs (or src/lib.rs), alongside the other `mod` lines"
                    .into(),
            ),
        }
    }
}

fn has_mod_components_decl(src: &str) -> bool {
    // Tolerate leading whitespace, attributes on the previous line, and `pub`.
    // Reject only the trivial cases (commented out, no semicolon).
    for line in src.lines() {
        let t = line.trim();
        if t.starts_with("//") {
            continue;
        }
        // `mod components;` or `pub mod components;`. We don't care about
        // `mod components { ... }` — that's an inline module body which won't
        // resolve `src/components/<name>/component.rs` files anyway.
        let stripped = t.trim_start_matches("pub ").trim_start();
        if stripped == "mod components;" {
            return true;
        }
    }
    false
}

fn check_theme_asset(src_dir: &Path) -> VerifyInstallStep {
    // We don't know which file the user mounted the asset in — projects pick
    // their own App location. Scan the whole src/ tree (cheap; src is small)
    // for any `asset!("/assets/dx-components-theme.css")` reference. The
    // catalog template ships exactly that filename so a substring match is
    // accurate enough to use as a wiring proxy.
    let mut hits: Vec<PathBuf> = Vec::new();
    let mut walked: Vec<PathBuf> = Vec::new();
    walk_rs_files(src_dir, &mut walked, 6);
    for path in &walked {
        if let Ok(text) = std::fs::read_to_string(path)
            && text.contains("dx-components-theme.css")
        {
            hits.push(path.clone());
        }
    }
    if !hits.is_empty() {
        VerifyInstallStep {
            id: "theme_asset",
            title: "catalog theme stylesheet mounted via `asset!`",
            ok: true,
            looked_in: hits,
            fix: None,
            fix_location: None,
        }
    } else {
        VerifyInstallStep {
            id: "theme_asset",
            title: "catalog theme stylesheet mounted via `asset!`",
            ok: false,
            // Surface a small slice of paths the scan touched so the caller
            // can see we actually looked, without dumping the whole src tree.
            looked_in: walked.into_iter().take(8).collect(),
            fix: Some(
                r#"document::Link { rel: "stylesheet", href: asset!("/assets/dx-components-theme.css") }"#
                    .into(),
            ),
            fix_location: Some(
                "at the top of the rsx! body in your `App` component (sibling to the rest of \
                 your route / layout content)"
                    .into(),
            ),
        }
    }
}

fn check_components_dir(dir: &Path) -> VerifyInstallStep {
    // A `src/components/` directory should exist *after* the first
    // `dx components add` call. If it doesn't, the catalog hasn't been
    // installed yet — surfaced as a separate step so the caller can
    // distinguish "wiring missing" from "nothing installed yet".
    let exists = dir.exists() && dir.is_dir();
    if exists {
        VerifyInstallStep {
            id: "components_dir",
            title: "`src/components/` exists",
            ok: true,
            looked_in: vec![dir.to_path_buf()],
            fix: None,
            fix_location: None,
        }
    } else {
        VerifyInstallStep {
            id: "components_dir",
            title: "`src/components/` exists",
            ok: false,
            looked_in: vec![dir.to_path_buf()],
            fix: Some("dx components add <name>".into()),
            fix_location: Some(
                "run from the crate root for any catalog widget (e.g. `dx components add button`)"
                    .into(),
            ),
        }
    }
}

fn walk_rs_files(root: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && depth > 0 {
            walk_rs_files(&path, out, depth - 1);
        } else if path.extension().and_then(|x| x.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(p: &Path, body: &str) {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn reports_all_three_missing_on_a_fresh_crate() {
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("src/main.rs"),
            "fn main() { dioxus::launch(App); }\n#[component]\nfn App() -> Element { rsx!{} }\n",
        );
        let r = run(dir.path());
        assert!(!r.fully_wired);
        let by_id: std::collections::HashMap<&str, &VerifyInstallStep> =
            r.steps.iter().map(|s| (s.id, s)).collect();
        assert!(!by_id["mod_components"].ok);
        assert!(!by_id["theme_asset"].ok);
        assert!(!by_id["components_dir"].ok);
        assert!(r.missing.contains(&"mod_components"));
        assert!(r.missing.contains(&"theme_asset"));
        assert!(r.missing.contains(&"components_dir"));
    }

    #[test]
    fn picks_up_mod_components_in_main_rs() {
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("src/main.rs"),
            "mod components;\nfn main() {}\n",
        );
        let r = run(dir.path());
        let step = step_by_id(&r, "mod_components");
        assert!(step.ok);
        assert_eq!(step.looked_in.len(), 1);
        assert!(step.looked_in[0].ends_with("src/main.rs"));
    }

    #[test]
    fn picks_up_mod_components_in_lib_rs_when_no_main() {
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("src/lib.rs"),
            "pub mod components;\npub fn run() {}\n",
        );
        let r = run(dir.path());
        let step = step_by_id(&r, "mod_components");
        assert!(step.ok);
        assert!(step.looked_in[0].ends_with("src/lib.rs"));
    }

    #[test]
    fn ignores_commented_out_mod_decl() {
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("src/main.rs"),
            "// mod components;\nfn main() {}\n",
        );
        let r = run(dir.path());
        assert!(!step_by_id(&r, "mod_components").ok);
    }

    #[test]
    fn picks_up_theme_asset_anywhere_under_src() {
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("src/main.rs"),
            "mod components;\nfn main() {}\n",
        );
        write(
            &dir.path().join("src/components/app.rs"),
            r#"#[component]
fn App() -> Element {
    rsx! {
        document::Link { rel: "stylesheet", href: asset!("/assets/dx-components-theme.css") }
    }
}
"#,
        );
        let r = run(dir.path());
        let step = step_by_id(&r, "theme_asset");
        assert!(step.ok);
        assert!(
            step.looked_in
                .iter()
                .any(|p| p.ends_with("src/components/app.rs"))
        );
    }

    #[test]
    fn detects_components_dir_present() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/components")).unwrap();
        write(&dir.path().join("src/main.rs"), "fn main() {}\n");
        let r = run(dir.path());
        assert!(step_by_id(&r, "components_dir").ok);
    }

    #[test]
    fn fully_wired_when_everything_present() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/components")).unwrap();
        write(
            &dir.path().join("src/main.rs"),
            r#"mod components;
fn main() {}
#[component]
fn App() -> Element {
    rsx! {
        document::Link { rel: "stylesheet", href: asset!("/assets/dx-components-theme.css") }
    }
}
"#,
        );
        let r = run(dir.path());
        assert!(r.fully_wired);
        assert!(r.missing.is_empty());
    }

    fn step_by_id<'a>(r: &'a VerifyInstallReport, id: &str) -> &'a VerifyInstallStep {
        r.steps.iter().find(|s| s.id == id).expect("step exists")
    }

    fn run(crate_root: &Path) -> VerifyInstallReport {
        let src = crate_root.join("src");
        let main_rs = src.join("main.rs");
        let lib_rs = src.join("lib.rs");
        let steps = vec![
            check_mod_components(&main_rs, &lib_rs),
            check_theme_asset(&src),
            check_components_dir(&src.join("components")),
        ];
        let missing: Vec<&'static str> = steps.iter().filter(|s| !s.ok).map(|s| s.id).collect();
        let fully_wired = missing.is_empty();
        VerifyInstallReport {
            project_root: crate_root.to_path_buf(),
            fully_wired,
            steps,
            missing,
        }
    }
}
