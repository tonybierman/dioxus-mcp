use super::super::*;
use super::cargo_toml_with_fullstack;

#[tokio::test]
async fn dx_components_attempts_install_and_validates_names() {
    // TODO15: top-level `dx_components: [...]` shells out to `dx components
    // add <name>` for each catalog-valid entry. On failure (e.g. `dx` not on
    // PATH in CI / sandboxed test env) it falls back to surfacing the
    // install command. Either way the typo entry is rejected up front.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "dx_components_test"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = { version = "0.7" }
"#,
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    let result = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
dx_components: [button, dialog, somethingunknown]
"#
            .into(),
            project_root: Some(root.to_string_lossy().into_owned()),
            if_missing: false,
            dry_run: false,
            cargo_check: false,
            format_after: false,
        },
    )
    .await
    .expect("dx_components run must not fail on a typo — it surfaces it as a hint");

    let steps = result.next_steps.join("\n");
    // The unknown name should NOT yield an install attempt; instead a
    // validation hint calls it out.
    assert!(
        steps.contains("\"somethingunknown\"") && steps.contains("not in the official"),
        "expected catalog-validation hint for the typo, got:\n{steps}"
    );
    // Either we successfully installed both (line: "installed via ... button, dialog")
    // OR fell back to printing the install commands. Both shapes are valid.
    let installed_ok =
        steps.contains("installed via `dx components add`") && steps.contains("button");
    let fallback_printed =
        steps.contains("dx components add button") && steps.contains("dx components add dialog");
    assert!(
        installed_ok || fallback_printed,
        "expected either successful install or fallback install commands, got:\n{steps}"
    );
    // First-time setup reminder fires either way.
    assert!(
        steps.contains("mod components;"),
        "expected one-time mod components reminder, got:\n{steps}"
    );
    // Import hint mentions both valid items with `crate::components::...::Pascal`.
    assert!(
        steps.contains("crate::components::button::Button"),
        "expected import hint for Button, got:\n{steps}"
    );
    assert!(
        steps.contains("crate::components::dialog::Dialog"),
        "expected import hint for Dialog, got:\n{steps}"
    );
}

#[tokio::test]
async fn dx_components_hints_surface_in_dry_run() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "dx_components_dry_run_test"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = { version = "0.7" }
"#,
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    let result = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
dx_components: [tooltip]
"#
            .into(),
            project_root: Some(root.to_string_lossy().into_owned()),
            if_missing: false,
            dry_run: true,
            cargo_check: false,
            format_after: false,
        },
    )
    .await
    .expect("dry_run with dx_components must succeed");
    assert!(result.dry_run);
    let steps = result.next_steps.join("\n");
    // Dry-run path uses the "would run …" shape so callers can preview the
    // install plan without anything happening on disk.
    assert!(
        steps.contains("would run `dx components add tooltip`"),
        "dry_run must surface dx_components install hints, got:\n{steps}"
    );
}

#[test]
fn suppress_dead_code_prepends_attribute_on_pub_enums() {
    // Mirrors the upstream button component.rs: two pub enums (`ButtonVariant`,
    // `ButtonSize`) sit at the top level alongside the `pub fn Button`. The
    // helper must prepend `#[allow(dead_code)]` to each enum without
    // touching the fn.
    use super::dx_components::suppress_dead_code_on_enums;
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("component.rs");
    let original = r#"use dioxus::prelude::*;

#[derive(Copy, Clone, PartialEq, Default)]
#[non_exhaustive]
pub enum ButtonVariant {
    #[default]
    Primary,
    Secondary,
}

#[derive(Copy, Clone, PartialEq, Default)]
#[non_exhaustive]
pub enum ButtonSize {
    Xs,
    #[default]
    Default,
}

#[component]
pub fn Button() -> Element {
    rsx! { button {} }
}
"#;
    std::fs::write(&path, original).unwrap();
    let r = suppress_dead_code_on_enums(&path);
    assert_eq!(r, Some(true), "first run should modify the file");
    let patched = std::fs::read_to_string(&path).unwrap();
    // Both pub enums got an `#[allow(dead_code)]` prepended.
    assert_eq!(
        patched.matches("#[allow(dead_code)]\npub enum").count(),
        2,
        "both pub enums must carry #[allow(dead_code)], got:\n{patched}"
    );
    // The fn is untouched.
    assert!(
        !patched.contains("#[allow(dead_code)]\n#[component]"),
        "must not annotate the component fn, got:\n{patched}"
    );
    // Existing attributes (`#[derive(…)]`, `#[non_exhaustive]`) are preserved.
    assert!(patched.contains("#[derive(Copy, Clone, PartialEq, Default)]"));
    assert!(patched.contains("#[non_exhaustive]"));

    // Idempotent: re-running on the patched file should NOT modify it again.
    let r2 = suppress_dead_code_on_enums(&path);
    assert_eq!(
        r2,
        Some(false),
        "second run should be a no-op when the attribute is already present"
    );
    let patched2 = std::fs::read_to_string(&path).unwrap();
    assert_eq!(patched, patched2, "second run must not change the file");
}

#[test]
fn record_dx_component_files_lists_each_file_individually() {
    // After `dx components add button` lays out
    // src/components/button/{mod.rs, component.rs, docs.md}, the recorder
    // must surface each file separately in `files_created` / `files_modified`
    // instead of just the dir — callers asked to skip the ls + verify_install
    // round-trip.
    use super::dx_components::record_dx_component_files;
    use crate::tools::scaffold::ScaffoldResult;
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let button_dir = root.join("src/components/button");
    std::fs::create_dir_all(&button_dir).unwrap();
    std::fs::write(
        button_dir.join("mod.rs"),
        "pub mod component;\npub use component::*;\n",
    )
    .unwrap();
    std::fs::write(
        button_dir.join("component.rs"),
        "use dioxus::prelude::*;\n\npub enum ButtonVariant { Primary, Secondary }\n\n#[component]\npub fn Button() -> Element { rsx! { button {} } }\n",
    )
    .unwrap();
    std::fs::write(button_dir.join("docs.md"), "# Button\n").unwrap();

    let mut result = ScaffoldResult::default();
    record_dx_component_files(root, "button", &mut result);

    let created: Vec<String> = result
        .files_created
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    let modified: Vec<String> = result
        .files_modified
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    assert!(
        created.iter().any(|p| p.ends_with("button/mod.rs")),
        "expected mod.rs in files_created, got {created:?}"
    );
    assert!(
        created.iter().any(|p| p.ends_with("button/docs.md")),
        "expected docs.md in files_created, got {created:?}"
    );
    // `pub enum ButtonVariant` triggered the dead-code touch-up; component.rs
    // moves to files_modified.
    assert!(
        modified.iter().any(|p| p.ends_with("button/component.rs")),
        "expected component.rs in files_modified after dead-code touch-up, got {modified:?}"
    );
    assert!(
        !created.iter().any(|p| p.ends_with("button/component.rs")),
        "component.rs must not appear in both lists, got created={created:?}"
    );
    // The dir itself is NOT recorded — only individual files.
    assert!(
        !created.iter().any(|p| p.ends_with("src/components/button")
            && !p.ends_with(".rs")
            && !p.ends_with(".md")),
        "dir path should not be recorded when files are enumerated"
    );
}
