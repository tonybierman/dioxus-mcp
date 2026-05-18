use super::super::*;
use super::cargo_toml_with_fullstack;

#[test]
fn plan_dsl_classifies_files() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src/components")).unwrap();
    std::fs::write(root.join("src/components/existing.rs"), "// existing\n").unwrap();
    std::fs::write(
        root.join("src/components/mod.rs"),
        "pub mod existing;\npub use existing::*;\n",
    )
    .unwrap();

    let doc: DslDoc = serde_yml::from_str(
        r#"version: "1"
components:
  - name: Existing
  - name: New
"#,
    )
    .unwrap();
    let plan = plan_dsl(&doc, &[], root);
    assert!(plan.dry_run);
    assert!(
        plan.collisions.iter().any(|p| p.ends_with("existing.rs")),
        "expected existing.rs in collisions, got {:?}",
        plan.collisions
    );
    assert!(
        plan.would_create.iter().any(|p| p.ends_with("new.rs")),
        "expected new.rs in would_create, got {:?}",
        plan.would_create
    );
    assert!(
        plan.would_modify.iter().any(|p| p.ends_with("mod.rs")),
        "expected mod.rs in would_modify, got {:?}",
        plan.would_modify
    );
}

#[test]
fn skip_set_collects_existing_leaf_files() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src/components")).unwrap();
    std::fs::write(root.join("src/components/existing.rs"), "").unwrap();

    let doc: DslDoc = serde_yml::from_str(
        r#"version: "1"
components:
  - name: Existing
  - name: New
"#,
    )
    .unwrap();
    let skip = skip_set(&doc, &[], root);
    assert_eq!(skip.len(), 1);
    assert!(skip.iter().any(|p| p.ends_with("existing.rs")));
}

#[tokio::test]
async fn if_missing_skips_existing_and_reports_collisions() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "if_missing_test"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = { version = "0.7", features = ["fullstack"] }
"#,
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src/components")).unwrap();
    std::fs::write(
        root.join("src/components/existing.rs"),
        "// hand-written; do not touch\n",
    )
    .unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    let result = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
components:
  - name: Existing
  - name: NewOne
"#
            .into(),
            project_root: Some(root.to_string_lossy().into_owned()),
            if_missing: true,
            dry_run: false,
            cargo_check: false,
            format_after: false,
        },
    )
    .await
    .expect("execute_code should succeed in if_missing mode");

    assert!(
        result.collisions.iter().any(|p| p.ends_with("existing.rs")),
        "expected existing.rs in collisions, got {:?}",
        result.collisions
    );
    let existing_body = std::fs::read_to_string(root.join("src/components/existing.rs")).unwrap();
    assert_eq!(
        existing_body, "// hand-written; do not touch\n",
        "if_missing must not overwrite the existing file"
    );
    assert!(
        root.join("src/components/new_one.rs").exists(),
        "non-conflicting components should still be created"
    );
}

#[tokio::test]
async fn if_missing_skips_existing_model_server_fn_signal_session() {
    // The skip-set machinery covers every primitive — confirm it applies
    // uniformly so iterative re-runs (add one field, re-run) don't force
    // the user to manually delete pre-existing files.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("if_missing_all_primitives"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src/model")).unwrap();
    std::fs::create_dir_all(root.join("src/server")).unwrap();
    std::fs::create_dir_all(root.join("src/signals")).unwrap();
    std::fs::create_dir_all(root.join("src/auth")).unwrap();
    // Pre-seed each with hand-written content.
    std::fs::write(root.join("src/model/widget.rs"), "// hand model\n").unwrap();
    std::fs::write(
        root.join("src/server/fetch_widgets.rs"),
        "// hand server fn\n",
    )
    .unwrap();
    std::fs::write(root.join("src/signals/counter.rs"), "// hand signal\n").unwrap();
    std::fs::write(root.join("src/auth/session.rs"), "// hand session\n").unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    let result = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
models:
  - name: Widget
    fields:
      - {name: id, type: i64}
server_fns:
  - name: fetch_widgets
    return_type: String
signals:
  - name: counter
    type: i32
    initial: "0"
session_states:
  - name: session
    user_type: String
"#
            .into(),
            project_root: Some(root.to_string_lossy().into_owned()),
            if_missing: true,
            dry_run: false,
            cargo_check: false,
            format_after: false,
        },
    )
    .await
    .expect("if_missing should skip pre-existing primitives, not error");

    for stub in [
        "src/model/widget.rs",
        "src/server/fetch_widgets.rs",
        "src/signals/counter.rs",
        "src/auth/session.rs",
    ] {
        assert!(
            result.collisions.iter().any(|p| p.ends_with(stub)),
            "expected {stub} in collisions, got {:?}",
            result.collisions
        );
        let body = std::fs::read_to_string(root.join(stub)).unwrap();
        assert!(
            body.starts_with("// hand"),
            "if_missing must not overwrite {stub}, got: {body}"
        );
    }
}

#[tokio::test]
async fn dry_run_returns_plan_without_writing() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "dry_run_test"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = { version = "0.7", features = ["fullstack"] }
"#,
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    let result = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
components:
  - name: Widget
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
    .expect("dry_run should succeed");

    assert!(result.dry_run);
    assert!(
        result.would_create.iter().any(|p| p.ends_with("widget.rs")),
        "expected widget.rs in would_create, got {:?}",
        result.would_create
    );
    assert!(
        !root.join("src/components/widget.rs").exists(),
        "dry_run must not write the file"
    );
}

#[tokio::test]
async fn dry_run_treats_existing_leaf_as_collision_not_error() {
    // TODO10: dry_run must never hard-error on an existing leaf — collisions
    // belong in the plan output, not as a preflight abort.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "dry_run_collision_test"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = { version = "0.7" }
"#,
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src/state")).unwrap();
    let existing = root.join("src/state/todo_store.rs");
    std::fs::write(&existing, "// pre-existing file\n").unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    let result = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
client_stores:
  - name: TodoStore
    item_type: String
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
    .expect("dry_run on an existing leaf must not error");

    assert!(result.dry_run);
    assert!(
        result.collisions.iter().any(|p| p == &existing),
        "expected the existing leaf in `collisions`, got: {:?}",
        result.collisions
    );
    assert_eq!(
        std::fs::read_to_string(&existing).unwrap(),
        "// pre-existing file\n",
        "dry_run must not modify the existing file"
    );
}

#[tokio::test]
async fn dry_run_resolves_cross_refs_from_disk() {
    // TODO11: callers should be able to dry-run a Screen that targets an
    // already-scaffolded client_store / model without redeclaring those
    // primitives in the YAML. preflight relaxes cross-ref checks to look
    // on disk in dry_run mode.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "dry_run_xref_test"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = { version = "0.7" }
"#,
    )
    .unwrap();
    // Pre-scaffold the leaf files the Screen will cross-reference: a model
    // file (Todo) and a client_store file (TodoStore). The bodies don't
    // matter for the dry-run preflight — only their on-disk presence.
    std::fs::create_dir_all(root.join("src/model")).unwrap();
    std::fs::write(
        root.join("src/model/todo.rs"),
        "pub struct Todo { pub id: i64, pub title: String }\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src/state")).unwrap();
    std::fs::write(
        root.join("src/state/todo_store.rs"),
        "// pre-existing client store\n",
    )
    .unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    // The YAML omits `models:` and `client_stores:` entirely — both live on
    // disk. Without the disk-aware relaxation this would error with
    // "screen references unknown client_store ...".
    let result = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
screens:
  - name: TodoScreen
    route: /
    template:
      kind: client_crud
      store: TodoStore
      item_type: Todo
      label_field: title
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
    .expect("dry_run must accept on-disk cross-references");

    assert!(result.dry_run);
    // The Screen leaf is fresh, so it lands in `would_create`.
    assert!(
        result
            .would_create
            .iter()
            .any(|p| p.ends_with("todo_screen.rs")),
        "expected todo_screen.rs in would_create, got {:?}",
        result.would_create
    );
    // The pre-existing files must still be on disk after the dry-run.
    assert_eq!(
        std::fs::read_to_string(root.join("src/state/todo_store.rs")).unwrap(),
        "// pre-existing client store\n",
        "dry_run must not modify pre-existing leaves"
    );
}

#[tokio::test]
async fn dry_run_emits_screen_body_preview() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "preview_test"
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
screens:
  - name: HomeScreen
    route: /
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
    .expect("dry_run should succeed");

    assert!(result.dry_run);
    let leaf = root.join("src/components/home_screen.rs");
    let body = result.previews.get(&leaf).unwrap_or_else(|| {
        panic!(
            "expected preview for {}; got keys: {:?}",
            leaf.display(),
            result.previews.keys().collect::<Vec<_>>()
        )
    });
    // The default Screen template renders an `rsx!` block with the
    // screen-class root div — make sure the preview surface is the
    // actual generated body, not a path placeholder.
    assert!(
        body.contains("rsx!"),
        "preview should include rsx! macro, got:\n{body}"
    );
    assert!(
        body.contains("HomeScreen"),
        "preview should mention the component name, got:\n{body}"
    );
    // Sanity: dry_run must still not write anything to disk.
    assert!(!leaf.exists(), "dry_run must not write the screen file");
}

#[test]
fn preflight_rejects_duplicate_model_name_and_duplicate_fields() {
    let dir = tempfile::TempDir::new().unwrap();
    let doc: DslDoc = serde_yml::from_str(
        r#"version: "1"
models:
  - name: Product
    fields:
      - {name: id, type: i64}
  - name: product
    fields:
      - {name: id, type: i64}
"#,
    )
    .unwrap();
    let err = preflight(&doc, &[], dir.path(), false).unwrap_err();
    assert!(err.contains("duplicate model"), "got {err}");

    let doc: DslDoc = serde_yml::from_str(
        r#"version: "1"
models:
  - name: Product
    fields:
      - {name: id, type: i64}
      - {name: ID, type: i64}
"#,
    )
    .unwrap();
    let err = preflight(&doc, &[], dir.path(), false).unwrap_err();
    assert!(err.contains("duplicate field"), "got {err}");
}

#[test]
fn preflight_rejects_store_referencing_unknown_model() {
    let dir = tempfile::TempDir::new().unwrap();
    let doc: DslDoc = serde_yml::from_str(
        r#"version: "1"
stores:
  - name: WidgetStore
    resource: Widget
"#,
    )
    .unwrap();
    let err = preflight(&doc, &[], dir.path(), false).unwrap_err();
    assert!(err.contains("unknown model"), "got {err}");
}

#[test]
fn preflight_rejects_client_crud_screen_with_unknown_store() {
    let dir = tempfile::TempDir::new().unwrap();
    let doc: DslDoc = serde_yml::from_str(
        r#"version: "1"
screens:
  - name: TodoScreen
    route: /
    template:
      kind: client_crud
      store: NopeStore
      item_type: Todo
      label_field: title
"#,
    )
    .unwrap();
    let err = preflight(&doc, &[], dir.path(), false).unwrap_err();
    assert!(err.contains("unknown client_store"), "got: {err}");
}

#[tokio::test]
async fn modify_dry_run_classifies_targets() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("modify_dry_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src/model")).unwrap();
    std::fs::write(
        root.join("src/model/product.rs"),
        "pub struct Product { pub id: i64, }\n",
    )
    .unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    let result = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
modify:
  - kind: add_model_field
    model: Product
    fields:
      - {name: sku, type: String}
  - kind: add_model_field
    model: Ghost
    fields:
      - {name: x, type: i32}
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
    .expect("dry_run should succeed even with missing target");
    assert!(result.dry_run);
    assert!(
        result
            .would_modify
            .iter()
            .any(|p| p.ends_with("product.rs")),
        "expected product.rs in would_modify, got {:?}",
        result.would_modify
    );
    assert!(
        result.collisions.iter().any(|p| p.ends_with("ghost.rs")),
        "expected ghost.rs in collisions, got {:?}",
        result.collisions
    );
    // Source file must be untouched.
    let body = std::fs::read_to_string(root.join("src/model/product.rs")).unwrap();
    assert!(!body.contains("sku"));
}

#[test]
fn preflight_rejects_empty_or_duplicate_modify_entry() {
    let dir = tempfile::TempDir::new().unwrap();
    let doc: DslDoc = serde_yml::from_str(
        r#"version: "1"
modify:
  - kind: add_model_field
    model: Product
    fields: []
"#,
    )
    .unwrap();
    let err = preflight(&doc, &[], dir.path(), false).unwrap_err();
    assert!(err.contains("is empty"), "got {err}");

    let doc: DslDoc = serde_yml::from_str(
        r#"version: "1"
modify:
  - kind: add_server_fn_arg
    server_fn: fetch
    args:
      - {name: page, type: u32}
      - {name: page, type: u64}
"#,
    )
    .unwrap();
    let err = preflight(&doc, &[], dir.path(), false).unwrap_err();
    assert!(err.contains("duplicate name"), "got {err}");
}

#[tokio::test]
async fn dry_run_classifies_model_files() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "models_dry"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = { version = "0.7", features = ["fullstack"] }
"#,
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    let result = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
models:
  - name: Product
    fields:
      - {name: id, type: i64}
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
    .expect("dry_run should succeed");
    assert!(result.dry_run);
    assert!(
        result
            .would_create
            .iter()
            .any(|p| p.ends_with("product.rs")),
        "expected product.rs in would_create, got {:?}",
        result.would_create
    );
    assert!(
        !root.join("src/model/product.rs").exists(),
        "dry_run must not write the file"
    );
}
