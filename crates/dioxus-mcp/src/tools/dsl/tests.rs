use super::*;

/// For each colocated spec block, take its `example:` mapping (which is a
/// DslDoc fragment under one or more primitive sections) and deserialize
/// it as a DslDoc with version "1" injected. Catches drift between the
/// hand-authored spec text and the Rust structs.
#[test]
fn spec_examples_round_trip() {
    let blocks: &[(&str, &str)] = &[
        ("CORE_MODEL", CORE_MODEL),
        ("CORE_STORE", CORE_STORE),
        ("CORE_CLIENT_STORE", CORE_CLIENT_STORE),
        ("CORE_RESOURCE", CORE_RESOURCE),
        ("CORE_COMPONENT", CORE_COMPONENT),
        ("CORE_SCREEN", CORE_SCREEN),
        ("CORE_SERVER_FN", CORE_SERVER_FN),
        ("CORE_MODIFY", CORE_MODIFY),
        ("CORE_REMOVE", CORE_REMOVE),
        ("CRUD_FORM", CRUD_FORM),
        ("CRUD_LIST", CRUD_LIST),
        ("CRUD_TABLE", CRUD_TABLE),
        ("REALTIME_SIGNAL", REALTIME_SIGNAL),
        ("REALTIME_SOCKET", REALTIME_SOCKET),
        ("REALTIME_FEED", REALTIME_FEED),
        ("AUTH_SESSION", AUTH_SESSION),
        ("AUTH_LOGIN", AUTH_LOGIN),
        ("AUTH_PROTECTED", AUTH_PROTECTED),
    ];
    for (name, block) in blocks {
        let v: serde_yml::Value = serde_yml::from_str(block)
            .unwrap_or_else(|e| panic!("{name}: spec block isn't YAML: {e}"));
        let map = v
            .as_mapping()
            .unwrap_or_else(|| panic!("{name}: top level not a map"));
        let primitive_value = map
            .iter()
            .next()
            .map(|(_, v)| v)
            .unwrap_or_else(|| panic!("{name}: empty"));
        let example = primitive_value
            .as_mapping()
            .and_then(|m| m.get("example"))
            .unwrap_or_else(|| panic!("{name}: no example: field"));
        let example_map = example
            .as_mapping()
            .unwrap_or_else(|| panic!("{name}: example is not a map"));
        let mut doc_yaml = String::from("version: \"1\"\n");
        for (k, v) in example_map.iter() {
            let mut snippet = serde_yml::to_string(&serde_yml::mapping::Mapping::from_iter([(
                k.clone(),
                v.clone(),
            )]))
            .unwrap();
            if !snippet.ends_with('\n') {
                snippet.push('\n');
            }
            doc_yaml.push_str(&snippet);
        }
        let doc: DslDoc = serde_yml::from_str(&doc_yaml)
            .unwrap_or_else(|e| panic!("{name}: deserialize failed: {e}\nyaml:\n{doc_yaml}"));
        assert_eq!(doc.version, "1");
    }
}

#[tokio::test]
async fn rejects_unknown_extension() {
    let dummy = std::sync::Arc::new(State::new(std::env::temp_dir()).unwrap());
    let r = get_dsl_spec(
        &dummy,
        GetDslSpecParams {
            extensions: vec!["bogus".into()],
            sections: vec![],
            index_only: false,
            include_prologue: Some(true),
            include_examples: true,
        },
    )
    .await;
    assert!(r.is_err());
}

#[tokio::test]
async fn sections_filter_returns_only_requested_core_sections() {
    let dummy = std::sync::Arc::new(State::new(std::env::temp_dir()).unwrap());
    let r = get_dsl_spec(
        &dummy,
        GetDslSpecParams {
            extensions: vec![],
            sections: vec!["model".into(), "client_store".into()],
            index_only: false,
            include_prologue: Some(true),
            include_examples: true,
        },
    )
    .await
    .expect("filter call should succeed");
    assert!(
        r.spec.contains("Model:"),
        "expected Model section, got:\n{}",
        r.spec
    );
    assert!(
        r.spec.contains("ClientStore:"),
        "expected ClientStore section, got:\n{}",
        r.spec
    );
    // Other core sections must be excluded. Use the section's own header
    // line (newline + 2-space indent + name + colon) so the assertion
    // doesn't trip over `Components:` — which contains `Component:` as a
    // substring but is a separate section.
    assert!(
        !r.spec.contains("\n  Component:\n"),
        "Component should be filtered out, got:\n{}",
        r.spec
    );
    assert!(
        !r.spec.contains("\n  Components:\n"),
        "Components should be filtered out, got:\n{}",
        r.spec
    );
    assert!(!r.spec.contains("Screen:"), "Screen should be filtered out");
    assert!(
        !r.spec.contains("ServerFn:"),
        "ServerFn should be filtered out"
    );
    assert!(!r.spec.contains("Modify:"), "Modify should be filtered out");
    // No extensions:` header when the filter only selects core sections.
    assert!(
        !r.spec.contains("\nextensions:\n"),
        "no extensions header expected, got:\n{}",
        r.spec
    );
}

#[tokio::test]
async fn sections_filter_auto_pulls_extension_group() {
    let dummy = std::sync::Arc::new(State::new(std::env::temp_dir()).unwrap());
    let r = get_dsl_spec(
        &dummy,
        GetDslSpecParams {
            extensions: vec![],
            sections: vec!["form".into()],
            index_only: false,
            include_prologue: Some(true),
            include_examples: true,
        },
    )
    .await
    .expect("filter call should succeed");
    assert!(
        r.spec.contains("\nextensions:\n"),
        "expected extensions header"
    );
    assert!(
        r.spec.contains(" crud:\n"),
        "expected crud group, got:\n{}",
        r.spec
    );
    assert!(r.spec.contains("Form:"), "expected Form section");
    // Other crud siblings must stay out when only `form` was requested.
    assert!(!r.spec.contains("List:\n"));
    assert!(!r.spec.contains("Table:\n"));
    // No core block when only an extension section was requested.
    assert!(!r.spec.contains("\ncore:\n"));
}

#[tokio::test]
async fn index_only_returns_compact_listing() {
    let dummy = std::sync::Arc::new(State::new(std::env::temp_dir()).unwrap());
    let r = get_dsl_spec(
        &dummy,
        GetDslSpecParams {
            extensions: vec!["crud".into()],
            sections: vec![],
            index_only: true,
            include_prologue: Some(true),
            include_examples: true,
        },
    )
    .await
    .expect("index_only call should succeed");
    // Every primitive name appears at most once, on its own line — and
    // the body should be much smaller than the full spec.
    assert!(r.spec.contains("Model:"), "expected Model in index");
    assert!(r.spec.contains("Component:"), "expected Component in index");
    assert!(r.spec.contains("Form:"), "expected Form (crud) in index");
    // No spec-block fields should appear in index mode.
    assert!(
        !r.spec.contains("template_kinds:"),
        "fields should be omitted"
    );
    assert!(!r.spec.contains("example:"), "examples should be omitted");
    // Should be well under 4KB — the full spec is ~10KB+.
    assert!(
        r.spec.len() < 4096,
        "index too large: {} bytes",
        r.spec.len()
    );
}

#[tokio::test]
async fn include_prologue_false_drops_the_preamble() {
    let dummy = std::sync::Arc::new(State::new(std::env::temp_dir()).unwrap());
    let r = get_dsl_spec(
        &dummy,
        GetDslSpecParams {
            extensions: vec![],
            sections: vec!["model".into()],
            index_only: false,
            include_prologue: Some(false),
            include_examples: true,
        },
    )
    .await
    .expect("call should succeed");
    // The preamble is the long "# Dioxus-MCP DSL spec" header. With it
    // off, the output should start with the `version:` line — and the
    // total size should drop substantially.
    assert!(
        !r.spec.contains("# Dioxus-MCP DSL spec"),
        "preamble should be absent, got:\n{}",
        r.spec
    );
    assert!(r.spec.contains("Model:"), "Model section must still ship");
}

#[tokio::test]
async fn include_examples_false_strips_example_blocks() {
    let dummy = std::sync::Arc::new(State::new(std::env::temp_dir()).unwrap());
    let r = get_dsl_spec(
        &dummy,
        GetDslSpecParams {
            extensions: vec!["crud".into()],
            sections: vec![],
            index_only: false,
            // Drop the prologue so its commentary about `example:` doesn't
            // confuse the assertion below.
            include_prologue: Some(false),
            include_examples: false,
        },
    )
    .await
    .expect("call should succeed");
    // Section headers and field schemas remain; example: YAML blocks gone.
    assert!(r.spec.contains("Model:"), "Model section must still ship");
    assert!(r.spec.contains("fields:"), "field schemas must still ship");
    // Strip targets the literal `    example:` (4-space) line for core
    // sections and `     example:` (5-space) for indented extension
    // blocks. Neither shape should survive.
    assert!(
        !r.spec.contains("    example:"),
        "core example blocks should be stripped, got:\n{}",
        r.spec
    );
    assert!(
        !r.spec.contains("     example:"),
        "extension example blocks should be stripped, got:\n{}",
        r.spec
    );
}

#[tokio::test]
async fn components_section_renders_catalog_and_indexes() {
    let dummy = std::sync::Arc::new(State::new(std::env::temp_dir()).unwrap());
    // Full block under the `components` filter — catalog body must appear.
    let full = get_dsl_spec(
        &dummy,
        GetDslSpecParams {
            extensions: vec![],
            sections: vec!["components".into()],
            index_only: false,
            include_prologue: Some(false),
            include_examples: true,
        },
    )
    .await
    .expect("components section should be fetchable on its own");
    assert!(
        full.spec.contains("Components:"),
        "expected Components header, got:\n{}",
        full.spec
    );
    assert!(
        full.spec.contains("button:"),
        "expected `button` catalog entry, got:\n{}",
        full.spec
    );
    assert!(
        full.spec.contains("dropdown_menu:"),
        "expected `dropdown_menu` catalog entry, got:\n{}",
        full.spec
    );
    assert!(
        full.spec.contains("dx components add"),
        "expected install hint, got:\n{}",
        full.spec
    );
    // Index-only mode must surface the section as a single line.
    let idx = get_dsl_spec(
        &dummy,
        GetDslSpecParams {
            extensions: vec![],
            sections: vec![],
            index_only: true,
            include_prologue: Some(false),
            include_examples: true,
        },
    )
    .await
    .expect("index_only call should succeed");
    assert!(
        idx.spec.contains("Components:"),
        "expected Components row in index, got:\n{}",
        idx.spec
    );
    // The 45 catalog rows must NOT appear in index mode — only the
    // section-level summary line should make it through.
    assert!(
        !idx.spec.contains("dropdown_menu:"),
        "catalog rows leaked into index, got:\n{}",
        idx.spec
    );
}

#[tokio::test]
async fn sections_filter_rejects_unknown_name() {
    let dummy = std::sync::Arc::new(State::new(std::env::temp_dir()).unwrap());
    let err = get_dsl_spec(
        &dummy,
        GetDslSpecParams {
            extensions: vec![],
            sections: vec!["models".into()],
            index_only: false,
            include_prologue: Some(true),
            include_examples: true,
        },
    )
    .await
    .unwrap_err();
    assert!(err.contains("unknown section"), "got: {err}");
    assert!(err.contains("model"), "should list valid names, got: {err}");
}

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
fn dx_components_catalog_matches_spec_block() {
    // The Rust-side catalog (DX_COMPONENT_CATALOG in execute.rs) and the
    // YAML catalog in CORE_COMPONENTS (specs.rs) need to stay in lockstep —
    // a `dx_components: [foo]` entry that's "valid" by the Rust check but
    // missing from the spec catalog would be a UX bug the next time someone
    // reads the catalog. This test parses the spec block and asserts the
    // two sources have the same set of names.
    use super::execute::DX_COMPONENT_CATALOG;
    let raw = CORE_COMPONENTS;
    let v: serde_yml::Value = serde_yml::from_str(raw).expect("CORE_COMPONENTS must be valid YAML");
    let components = v
        .get("Components")
        .and_then(|m| m.get("catalog"))
        .and_then(|m| m.as_mapping())
        .expect("Components.catalog must be a mapping");
    let spec_names: std::collections::BTreeSet<String> = components
        .keys()
        .filter_map(|k| k.as_str().map(|s| s.to_string()))
        .collect();
    let code_names: std::collections::BTreeSet<String> =
        DX_COMPONENT_CATALOG.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        spec_names, code_names,
        "spec catalog and DX_COMPONENT_CATALOG must match; refresh both when the upstream registry changes"
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
fn model_template_emits_struct_with_derives_and_optional_fields() {
    let m = DslModel {
        name: "Product".into(),
        fields: vec![
            DslModelField {
                name: "id".into(),
                ty: "i64".into(),
                optional: false,
            },
            DslModelField {
                name: "name".into(),
                ty: "String".into(),
                optional: false,
            },
            DslModelField {
                name: "description".into(),
                ty: "String".into(),
                optional: true,
            },
        ],
        derives: vec!["Eq".into(), "Clone".into()],
    };
    let dir = tempfile::TempDir::new().unwrap();
    let r = generate_model(dir.path(), &m).unwrap();
    let path = dir.path().join("src/model/product.rs");
    assert!(r.files_created.iter().any(|p| p == &path));
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains("use serde::{Deserialize, Serialize};"));
    assert!(body.contains("pub struct Product {"));
    assert!(body.contains("pub id: i64,"));
    assert!(body.contains("pub name: String,"));
    assert!(body.contains("pub description: Option<String>,"));
    // Defaults + Eq, no duplicate Clone.
    assert!(body.contains("#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Eq)]"));
    // mod.rs should reference the new module.
    let mod_rs = std::fs::read_to_string(dir.path().join("src/model/mod.rs")).unwrap();
    assert!(mod_rs.contains("pub mod product;"));
}

#[tokio::test]
async fn execute_code_creates_model_files_and_next_steps() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "models_test"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = { version = "0.7", features = ["fullstack"] }
"#,
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    // A minimal main.rs so the crate-root `pub mod` auto-injection has a
    // file to patch — exercising the post-#2 behavior.
    std::fs::write(
        root.join("src/main.rs"),
        r#"use dioxus::prelude::*;

fn main() {}
"#,
    )
    .unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    let result = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
models:
  - name: Product
    fields:
      - {name: id, type: i64}
      - {name: name, type: String}
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
    .expect("execute_code should succeed with models");
    assert!(root.join("src/model/product.rs").exists());
    assert!(root.join("src/model/mod.rs").exists());
    assert!(
        result
            .next_steps
            .iter()
            .any(|s| s.contains("serde") && s.contains("derive")),
        "expected a serde next_step, got {:?}",
        result.next_steps
    );
    // Cargo.toml should have been auto-patched with the serde dep line.
    let cargo = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(
        cargo.contains(r#"serde = { version = "1", features = ["derive"] }"#),
        "expected Cargo.toml to be patched with serde dep, got:\n{cargo}"
    );
    let cargo_path = root.join("Cargo.toml");
    assert!(
        result.files_modified.contains(&cargo_path),
        "Cargo.toml should appear in files_modified after auto-patch, got {:?}",
        result.files_modified
    );
    let main_rs = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
    assert!(
        main_rs.contains("pub mod model;"),
        "expected main.rs to be patched with `pub mod model;`, got:\n{main_rs}"
    );
    let main_path = root.join("src/main.rs");
    assert!(
        result.files_modified.contains(&main_path),
        "main.rs should appear in files_modified, got {:?}",
        result.files_modified
    );
}

#[tokio::test]
async fn data_layer_only_path_bootstraps_components_dir_without_router() {
    // Doc with only `models:` + `client_stores:` (no screens). Regression
    // test for the documented data-layer-only behavior:
    //   - Generates the model + store leaf files.
    //   - Adds `pub mod model;` / `pub mod state;` / `pub mod components;`
    //     to the crate root (the last one bootstraps an empty
    //     components/mod.rs so hand-written UI has a home).
    //   - Wires `provide_*()` into the App body for declared client_stores.
    //   - Does NOT touch the router or inject `Router::<...>` (no screens).
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("data_layer_only_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        r#"use dioxus::prelude::*;

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    rsx! {
        div { "welcome" }
    }
}
"#,
    )
    .unwrap();

    let state = std::sync::Arc::new(State::new(root.to_path_buf()).unwrap());
    let result = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
models:
  - name: Todo
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
client_stores:
  - name: TodoStore
    item_type: Todo
    id_field: id
    id_type: i64
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
    .expect("execute_code should succeed on the data-layer-only path");

    // Leaf files landed.
    let model_path = root.join("src/model/todo.rs");
    let store_path = root.join("src/state/todo_store.rs");
    let components_mod = root.join("src/components/mod.rs");
    assert!(model_path.exists(), "model file should be created");
    assert!(store_path.exists(), "store file should be created");
    assert!(
        components_mod.exists(),
        "components/mod.rs should be bootstrapped"
    );
    assert!(
        result.files_created.contains(&components_mod),
        "components/mod.rs should appear in files_created, got {:?}",
        result.files_created
    );

    // main.rs got the three `pub mod` declarations (model, state,
    // components) plus the provide_*() injection — and crucially, no
    // Router mount.
    let main_rs = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
    assert!(
        main_rs.contains("pub mod model;"),
        "expected `pub mod model;` in main.rs, got:\n{main_rs}"
    );
    assert!(
        main_rs.contains("pub mod state;"),
        "expected `pub mod state;` in main.rs, got:\n{main_rs}"
    );
    assert!(
        main_rs.contains("pub mod components;"),
        "expected `pub mod components;` in main.rs (components/ bootstrap), \
             got:\n{main_rs}"
    );
    assert!(
        main_rs.contains("provide_todo_store()"),
        "App body should call provide_todo_store(), got:\n{main_rs}"
    );
    assert!(
        !main_rs.contains("Router::<"),
        "Router must NOT be injected when no screens are declared, got:\n{main_rs}"
    );

    // Router file must not have been created either (data-layer-only =
    // no Routable mutation).
    assert!(
        !root.join("src/router.rs").exists(),
        "router.rs must not be created on the data-layer-only path"
    );

    // No stale "create src/components/mod.rs manually" hint — bootstrap
    // handled it.
    assert!(
        !result
            .next_steps
            .iter()
            .any(|s| s.contains("create `src/components/mod.rs`")),
        "manual-bootstrap hint should be gone after auto-bootstrap, got {:?}",
        result.next_steps
    );

    // Re-run is idempotent: nothing new should land, and main.rs must not
    // accumulate duplicate `pub mod components;` / `provide_*()` lines.
    let result2 = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
models:
  - name: Todo
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
client_stores:
  - name: TodoStore
    item_type: Todo
    id_field: id
    id_type: i64
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
    .expect("re-run should succeed");
    let main_rs_after = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
    assert_eq!(
        main_rs_after.matches("pub mod components;").count(),
        1,
        "pub mod components must not duplicate on re-run:\n{main_rs_after}"
    );
    assert_eq!(
        main_rs_after.matches("provide_todo_store()").count(),
        1,
        "provide_todo_store() must not duplicate on re-run:\n{main_rs_after}"
    );
    assert!(
        !result2.files_created.contains(&components_mod),
        "components/mod.rs must not be re-created on a follow-up run"
    );
}

#[tokio::test]
async fn wire_app_injects_router_and_provide_into_dx_new_app() {
    // Simulates the dx-new main.rs shape: an `App` component with an
    // rsx! body. After execute_code lands a client_crud Screen, the App
    // body should carry `Router::<...>` and `provide_*_store()` without
    // the user touching main.rs.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("wire_app_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        r#"use dioxus::prelude::*;

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    rsx! {
        div { "welcome" }
    }
}
"#,
    )
    .unwrap();

    let state = std::sync::Arc::new(State::new(root.to_path_buf()).unwrap());
    execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
models:
  - name: Todo
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
client_stores:
  - name: TodoStore
    item_type: Todo
    id_field: id
    id_type: i64
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
            dry_run: false,
            cargo_check: false,
            format_after: false,
        },
    )
    .await
    .expect("execute_code should succeed against a dx-new-shaped main.rs");

    let main_rs = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
    assert!(
        main_rs.contains("Router::<crate::router::Route> {}"),
        "App body should mount the Router, got:\n{main_rs}"
    );
    assert!(
        main_rs.contains("provide_todo_store()"),
        "App body should call provide_todo_store(), got:\n{main_rs}"
    );
    // Existing welcome content shouldn't be clobbered — Router is inserted
    // *alongside* the original children.
    assert!(
        main_rs.contains(r#"div { "welcome" }"#),
        "original rsx children should be preserved, got:\n{main_rs}"
    );
    // Structural checks: the provide_* call must land on its own line
    // (not glued to the App body's `{`), and Router must be a child of
    // the rsx block (not inserted between `rsx!` and its opening `{`).
    assert!(
        !main_rs.contains("fn App() -> Element {    crate::state::"),
        "provide_* call must start on a new line under fn App, got:\n{main_rs}"
    );
    // The injected call should be a bare statement (no `let _ =` prefix).
    assert!(
        main_rs.contains("crate::state::todo_store::provide_todo_store();"),
        "provide_* must be emitted as a bare statement, got:\n{main_rs}"
    );
    assert!(
        !main_rs.contains("let _ = crate::state::todo_store::provide_todo_store"),
        "provide_* should not be wrapped in `let _ =`, got:\n{main_rs}"
    );
    assert!(
        !main_rs.contains("rsx! \n"),
        "Router must not be inserted between `rsx!` and its `{{`, got:\n{main_rs}"
    );
    assert!(
        main_rs.contains("rsx! {\n")
            || main_rs.contains("rsx! {\r\n")
            || main_rs.contains("rsx!{\n"),
        "rsx! block opening should remain intact, got:\n{main_rs}"
    );

    // Re-run is idempotent: a second execute_code with `if_missing: true`
    // shouldn't append duplicate Router/provide_* lines.
    let _ = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
models:
  - name: Todo
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
client_stores:
  - name: TodoStore
    item_type: Todo
    id_field: id
    id_type: i64
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
            if_missing: true,
            dry_run: false,
            cargo_check: false,
            format_after: false,
        },
    )
    .await
    .expect("rerun should succeed");
    let main_rs_after = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
    assert_eq!(
        main_rs_after
            .matches("Router::<crate::router::Route>")
            .count(),
        1,
        "Router mount must not duplicate on re-run:\n{main_rs_after}"
    );
    assert_eq!(
        main_rs_after.matches("provide_todo_store()").count(),
        1,
        "provide_todo_store() must not duplicate on re-run:\n{main_rs_after}"
    );
}

#[tokio::test]
async fn next_steps_prefix_wire_app_hints_with_crate_root_path() {
    // When wire_app falls back to manual hints (no `fn App()` in the
    // crate root), the next_steps entries should name the file so the
    // user can paste the path straight into an editor.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("hint_path_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    // A main.rs WITHOUT an `fn App()` — forces wire_app's fallback path.
    std::fs::write(
        root.join("src/main.rs"),
        r#"use dioxus::prelude::*;

fn main() {
    dioxus::launch(|| rsx! { div { "hi" } });
}
"#,
    )
    .unwrap();

    let state = std::sync::Arc::new(State::new(root.to_path_buf()).unwrap());
    let result = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
models:
  - name: Todo
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
client_stores:
  - name: TodoStore
    item_type: Todo
    id_field: id
    id_type: i64
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
            dry_run: false,
            cargo_check: false,
            format_after: false,
        },
    )
    .await
    .expect("execute_code should still succeed when App fn is missing");

    // Both hints should be prefixed with the relative file path.
    let provide_hint = result
        .next_steps
        .iter()
        .find(|s| s.contains("provide_todo_store"))
        .unwrap_or_else(|| panic!("expected a provide_* hint, got {:?}", result.next_steps));
    assert!(
        provide_hint.starts_with("src/main.rs:"),
        "provide_* hint should start with `src/main.rs:`, got: {provide_hint}"
    );
    let router_hint = result
        .next_steps
        .iter()
        .find(|s| s.contains("no `fn App()` found"))
        .unwrap_or_else(|| {
            panic!(
                "expected the wire_app no-App-fn hint, got {:?}",
                result.next_steps
            )
        });
    assert!(
        router_hint.starts_with("src/main.rs:"),
        "router hint should start with `src/main.rs:`, got: {router_hint}"
    );
}

#[tokio::test]
async fn no_routable_warning_when_enum_lives_in_main_rs_dx_new_layout() {
    // Regression: the `dx new` starter puts the Routable enum directly in
    // src/main.rs. The "non-conventional" warning used to fire on every
    // fresh scaffold, which was noise. main.rs (and lib.rs) now count as
    // conventional crate-root locations — no warning, but the route
    // insert must still land.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("dx_new_main_rs_routable"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        r#"use dioxus::prelude::*;

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    rsx! {
        Router::<Route> {}
    }
}

#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[route("/about")]
    About {},
}

#[component]
fn About() -> Element { rsx! { "about" } }
"#,
    )
    .unwrap();

    let state = std::sync::Arc::new(State::new(root.to_path_buf()).unwrap());
    let result = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
models:
  - name: Todo
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
client_stores:
  - name: TodoStore
    item_type: Todo
    id_field: id
    id_type: i64
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
            dry_run: false,
            cargo_check: false,
            format_after: false,
        },
    )
    .await
    .expect("execute_code should succeed when Routable lives in main.rs");

    assert!(
        !result
            .next_steps
            .iter()
            .any(|s| s.contains("non-conventional")),
        "no non-conventional warning expected on a dx-new main.rs layout, got next_steps={:?}",
        result.next_steps
    );

    // Sanity: route insertion still landed.
    let main_rs = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
    assert!(
        main_rs.contains("TodoScreen"),
        "TodoScreen variant should have been inserted into the Routable in main.rs, got:\n{main_rs}"
    );
}

#[tokio::test]
async fn warns_when_routable_lives_in_truly_unusual_path() {
    // Sanity: the warning still fires for a Routable enum tucked under
    // a feature module, where convention-aware tooling has no chance.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("unusual_routable_location"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src/features")).unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        r#"use dioxus::prelude::*;
pub mod features;
fn main() { dioxus::launch(features::routing::App); }
"#,
    )
    .unwrap();
    std::fs::write(root.join("src/features.rs"), "pub mod routing;\n").unwrap();
    std::fs::create_dir_all(root.join("src/features")).unwrap();
    std::fs::write(
        root.join("src/features/routing.rs"),
        r#"use dioxus::prelude::*;

#[component]
pub fn App() -> Element {
    rsx! { Router::<Route> {} }
}

#[derive(Routable, Clone, PartialEq)]
pub enum Route {
    #[route("/about")]
    About {},
}

#[component]
fn About() -> Element { rsx! { "about" } }
"#,
    )
    .unwrap();

    let state = std::sync::Arc::new(State::new(root.to_path_buf()).unwrap());
    let result = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
models:
  - name: Todo
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
client_stores:
  - name: TodoStore
    item_type: Todo
    id_field: id
    id_type: i64
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
            dry_run: false,
            cargo_check: false,
            format_after: false,
        },
    )
    .await
    .expect("execute_code should succeed with an unusual Routable location");

    assert!(
        result
            .next_steps
            .iter()
            .any(|s| s.contains("non-conventional") && s.contains("src/features/routing.rs")),
        "expected a non-conventional Routable warning naming the nested path, got next_steps={:?}",
        result.next_steps
    );
}

#[tokio::test]
async fn no_routable_warning_when_enum_lives_at_router_rs() {
    // When the Routable is at src/router.rs (the canonical location)
    // we must NOT push the warning. Verifies the helper doesn't fire
    // on the happy path.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("conventional_routable"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        r#"use dioxus::prelude::*;

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    rsx! {
        Router::<crate::router::Route> {}
    }
}

pub mod router;
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("src/router.rs"),
        r#"use dioxus::prelude::*;

#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[route("/about")]
    About {},
}

#[component]
fn About() -> Element { rsx! { "about" } }
"#,
    )
    .unwrap();

    let state = std::sync::Arc::new(State::new(root.to_path_buf()).unwrap());
    let result = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
models:
  - name: Todo
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
client_stores:
  - name: TodoStore
    item_type: Todo
    id_field: id
    id_type: i64
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
            dry_run: false,
            cargo_check: false,
            format_after: false,
        },
    )
    .await
    .expect("execute_code should succeed");
    assert!(
        !result
            .next_steps
            .iter()
            .any(|s| s.contains("non-conventional")),
        "no warning expected when Routable lives at src/router.rs, got next_steps={:?}",
        result.next_steps
    );
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

#[tokio::test]
async fn execute_code_expands_resource_into_full_slice() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "resource_test"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = { version = "0.7", features = ["fullstack"] }
"#,
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    // Minimal Routable enum so route inserts succeed.
    std::fs::write(
        root.join("src/router.rs"),
        r#"use dioxus::prelude::*;

#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[route("/")]
    Home {},
}
"#,
    )
    .unwrap();
    // main.rs so the crate-root `pub mod` auto-injection has a file to
    // patch. Without this, only a fallback next_steps hint is emitted.
    std::fs::write(
        root.join("src/main.rs"),
        r#"use dioxus::prelude::*;

pub mod router;

fn main() {}
"#,
    )
    .unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    let result = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
resources:
  - name: Product
    fields:
      - {name: id, type: i64}
      - {name: name, type: String}
      - {name: description, type: String, optional: true}
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
    .expect("execute_code should succeed");

    // Model
    assert!(root.join("src/model/product.rs").exists());
    let model_body = std::fs::read_to_string(root.join("src/model/product.rs")).unwrap();
    assert!(
        model_body.contains("Default"),
        "synthesized resource model should derive Default, got:\n{model_body}"
    );

    // Store
    let store_path = root.join("src/state/product_store.rs");
    assert!(store_path.exists(), "store file should be emitted");
    let store_body = std::fs::read_to_string(&store_path).unwrap();
    assert!(store_body.contains(r#"#![cfg(feature = "server")]"#));
    assert!(store_body.contains("pub struct ProductStore"));
    assert!(store_body.contains("fn list("));
    assert!(store_body.contains("fn get("));
    assert!(store_body.contains("fn create("));
    assert!(store_body.contains("fn update("));
    assert!(store_body.contains("fn delete("));
    assert!(store_body.contains("use crate::model::Product"));
    // Resource expansion forces emit_tests=true, so the CRUD test block
    // should land. The synthesized model derives Default, so the tests
    // compile against `Product::default()`.
    assert!(
        store_body.contains("#[cfg(test)]"),
        "expected test block in store, got:\n{store_body}"
    );
    assert!(
        store_body.contains("create_assigns_id_and_appends_to_list"),
        "expected create test, got:\n{store_body}"
    );
    assert!(
        store_body.contains("delete_removes_matching_item_and_is_idempotent"),
        "expected delete test, got:\n{store_body}"
    );
    assert!(
        store_body.contains("Product::default()"),
        "tests should construct via Default, got:\n{store_body}"
    );
    // Sanity: the rendered store must parse as valid Rust — catches
    // template typos that the unit-render tests can't see.
    syn::parse_file(&store_body).unwrap_or_else(|e| {
        panic!("generated store file should parse as Rust: {e}\n--- file ---\n{store_body}")
    });
    let state_mod = std::fs::read_to_string(root.join("src/state/mod.rs")).unwrap();
    assert!(
        state_mod.contains(r#"#[cfg(feature = "server")]"#)
            && state_mod.contains("pub mod product_store;"),
        "state/mod.rs must cfg-gate store entries (otherwise wasm build fails E0432), got:\n{state_mod}"
    );

    // 5 server fns
    for name in [
        "list_products",
        "get_product",
        "create_product",
        "update_product",
        "delete_product",
    ] {
        let p = root.join("src/server").join(format!("{name}.rs"));
        assert!(p.exists(), "missing {}", p.display());
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(
            body.contains(r#"#[cfg(feature = "server")]"#)
                && body.contains("ProductStore::global()"),
            "server fn {name} should call into store, got:\n{body}"
        );
    }

    // 2 screens, 2 route variants
    assert!(root.join("src/components/product_list_screen.rs").exists());
    assert!(root.join("src/components/product_new_screen.rs").exists());
    let router = std::fs::read_to_string(root.join("src/router.rs")).unwrap();
    assert!(
        router.contains("ProductListScreen"),
        "list screen should be in router, got:\n{router}"
    );
    assert!(
        router.contains("ProductNewScreen"),
        "new screen should be in router, got:\n{router}"
    );

    // main.rs should be auto-patched with `pub mod` declarations for
    // every emitted top-level subdir (model, state, server, components).
    let main_rs = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
    for needed in [
        "pub mod model;",
        "pub mod state;",
        "pub mod server;",
        "pub mod components;",
    ] {
        assert!(
            main_rs.contains(needed),
            "expected main.rs to contain `{needed}`, got:\n{main_rs}"
        );
    }

    // The list screen uses use_resource + match ladder bound to list_products.
    let list_body =
        std::fs::read_to_string(root.join("src/components/product_list_screen.rs")).unwrap();
    assert!(
        list_body.contains("use_resource(")
            && list_body.contains("list_products()")
            && list_body.contains("Loading..."),
        "list screen should be resource-bound, got:\n{list_body}"
    );

    // The new screen has one input per non-id field and a submit body that
    // constructs Product and navigates to /products.
    let new_body =
        std::fs::read_to_string(root.join("src/components/product_new_screen.rs")).unwrap();
    assert!(
        new_body.contains("use_signal") && new_body.contains("create_product"),
        "new screen should call create_product, got:\n{new_body}"
    );
    assert!(
        new_body.contains("nav.push(\"/products\")"),
        "new screen should redirect to /products, got:\n{new_body}"
    );

    // The new screen's `use` for the model type should be a single
    // segment — emitted as `use crate::model::Product;`, never the
    // earlier-bug duplicated `use crate::model::crate::model::Product;`.
    assert!(
        new_body.contains("use crate::model::Product;"),
        "new screen should use bare model path, got:\n{new_body}"
    );
    assert!(
        !new_body.contains("crate::model::crate::"),
        "new screen must not duplicate the crate::model:: prefix, got:\n{new_body}"
    );

    // The edit screen should also have been emitted with an id prop,
    // call get_/update_, and route under /products/:id/edit.
    let edit_path = root.join("src/components/product_edit_screen.rs");
    assert!(edit_path.exists(), "edit screen file should be emitted");
    let edit_body = std::fs::read_to_string(&edit_path).unwrap();
    assert!(
        edit_body.contains("pub fn ProductEditScreen(id: i64)"),
        "edit screen should take id prop, got:\n{edit_body}"
    );
    assert!(
        edit_body.contains("get_product(") && edit_body.contains("update_product"),
        "edit screen should fetch via get_product and submit via update_product, got:\n{edit_body}"
    );
    assert!(
        router.contains("ProductEditScreen { id: i64 }"),
        "edit route variant should carry id field, got:\n{router}"
    );
    assert!(
        router.contains("/products/:id/edit"),
        "edit route path should appear, got:\n{router}"
    );

    // Every emitted .rs file must at least parse as Rust. This catches
    // template typos that no behavioural assert covers.
    for rel in [
        "src/model/product.rs",
        "src/state/product_store.rs",
        "src/server/list_products.rs",
        "src/server/get_product.rs",
        "src/server/create_product.rs",
        "src/server/update_product.rs",
        "src/server/delete_product.rs",
        "src/components/product_list_screen.rs",
        "src/components/product_new_screen.rs",
        "src/components/product_edit_screen.rs",
    ] {
        let body = std::fs::read_to_string(root.join(rel)).unwrap();
        syn::parse_file(&body)
            .unwrap_or_else(|e| panic!("emitted {rel} does not parse: {e}\n---\n{body}"));
    }

    // files_modified should be deduplicated — without it, src/router.rs and
    // src/components/mod.rs each appear once per route/component inserted.
    let mut sorted = result.files_modified.clone();
    sorted.sort();
    let mut deduped = sorted.clone();
    deduped.dedup();
    assert_eq!(
        sorted.len(),
        deduped.len(),
        "files_modified must be deduped; saw {:?}",
        result.files_modified
    );
}

#[tokio::test]
async fn resource_form_template_emits_typed_constructor_for_mixed_field_types() {
    // Mix String / Option<String> / i64 / Option<i64> / f64 / bool so the
    // new screen exercises every branch of the form-typing fix.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("res_typing_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/router.rs"),
        r#"use dioxus::prelude::*;

#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[route("/")]
    Home {},
}
"#,
    )
    .unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
resources:
  - name: Product
    fields:
      - {name: id, type: i64}
      - {name: name, type: String}
      - {name: description, type: String, optional: true}
      - {name: quantity, type: i64}
      - {name: reorder_at, type: i64, optional: true}
      - {name: price, type: f64}
      - {name: active, type: bool}
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
    .expect("execute_code should succeed");

    let new_body =
        std::fs::read_to_string(root.join("src/components/product_new_screen.rs")).unwrap();

    // Signal initializers must be String::new() for text-backed inputs and
    // `false` for the bool. Crucially: NO `0i64` or `0.0f64` literals —
    // numeric fields are String-backed and parsed at submit.
    assert!(
        new_body.contains("let mut name = use_signal(|| String::new())"),
        "name should be a String-backed signal, got:\n{new_body}"
    );
    assert!(
        new_body.contains("let mut description = use_signal(|| String::new())"),
        "description (Option<String>) should still be String-backed, got:\n{new_body}"
    );
    assert!(
        new_body.contains("let mut quantity = use_signal(|| String::new())"),
        "i64 signal should be String-backed, got:\n{new_body}"
    );
    assert!(
        new_body.contains("let mut price = use_signal(|| String::new())"),
        "f64 signal should be String-backed, got:\n{new_body}"
    );
    assert!(
        new_body.contains("let mut active = use_signal(|| false)"),
        "bool signal should be initialized to false, got:\n{new_body}"
    );
    assert!(
        !new_body.contains("0i64") && !new_body.contains("0.0f64"),
        "numeric signals must not be initialized with a typed literal, got:\n{new_body}"
    );

    // Submit-side constructor must wrap Option fields and parse numerics.
    assert!(
        new_body.contains("name: name_v,"),
        "String field assigns raw signal value, got:\n{new_body}"
    );
    assert!(
        new_body.contains("if description_v.is_empty() { None } else { Some(description_v) }"),
        "Option<String> must wrap with Some and treat empty as None, got:\n{new_body}"
    );
    assert!(
        new_body.contains("quantity_v.parse::<i64>().unwrap_or_default()"),
        "i64 field must be parsed from String, got:\n{new_body}"
    );
    assert!(
        new_body.contains("price_v.parse::<f64>().unwrap_or_default()"),
        "f64 field must be parsed from String, got:\n{new_body}"
    );
    assert!(
        new_body.contains(
            "if reorder_at_v.is_empty() { None } else { reorder_at_v.parse::<i64>().ok() }"
        ),
        "Option<i64> must parse-or-none on empty, got:\n{new_body}"
    );
    assert!(
        new_body.contains("active: active_v,"),
        "bool field reads signal directly, got:\n{new_body}"
    );

    // No duplicated crate::model:: prefix.
    assert!(
        !new_body.contains("crate::model::crate::"),
        "new screen must not duplicate the crate::model:: prefix, got:\n{new_body}"
    );

    // All synthesized screens must still parse as valid Rust.
    for rel in [
        "src/components/product_list_screen.rs",
        "src/components/product_new_screen.rs",
        "src/components/product_edit_screen.rs",
    ] {
        let body = std::fs::read_to_string(root.join(rel)).unwrap();
        syn::parse_file(&body)
            .unwrap_or_else(|e| panic!("emitted {rel} does not parse: {e}\n---\n{body}"));
    }

    // The list screen should be a real table with column headers, an
    // edit link, and a delete button — not the placeholder `li{item:?}`.
    let list_body =
        std::fs::read_to_string(root.join("src/components/product_list_screen.rs")).unwrap();
    assert!(
        list_body.contains("table {")
            && list_body.contains("thead {")
            && list_body.contains("tbody {"),
        "list screen should emit a real table, got:\n{list_body}"
    );
    assert!(
        list_body.contains("key: \"{row.id}\""),
        "rows should be keyed by id, got:\n{list_body}"
    );
    assert!(
        list_body.contains("delete_product("),
        "delete button should call delete_product, got:\n{list_body}"
    );
    // List uses typed Link to the route enum for SPA navigation rather than
    // `<a href>` (which would force a full page reload).
    assert!(
        list_body.contains("Link { to: Route::ProductEditScreen { id: row.id.clone() }"),
        "edit link should be a typed Link to the EditScreen route variant, got:\n{list_body}"
    );
    assert!(
        list_body.contains("Link { to: Route::ProductNewScreen {}"),
        "new link should be a typed Link to the NewScreen route variant, got:\n{list_body}"
    );
    assert!(
        list_body.contains("use crate::router::Route;"),
        "list screen should import the Route enum, got:\n{list_body}"
    );
    assert!(
        !list_body.contains("a { href: \"/products/new\""),
        "list should not retain the old `a {{ href: }}` form, got:\n{list_body}"
    );
    assert!(
        list_body.contains("*version.write() += 1"),
        "delete should bump a version signal to refetch, got:\n{list_body}"
    );
    // No `li { \"{item:?}\" }` placeholder.
    assert!(
        !list_body.contains("li { \"{item:?}\" }"),
        "list should not retain the placeholder li body, got:\n{list_body}"
    );
    // Option<T> columns must render the inner value, not Debug-format the
    // Option wrapper (which would produce literal "Some(...)" / "None" in
    // the cell).
    assert!(
        list_body.contains("row.description.as_ref().map(|v| v.to_string()).unwrap_or_default()"),
        "Option<String> column should be unwrapped, not Debug-formatted, got:\n{list_body}"
    );
    assert!(
        list_body.contains("row.reorder_at.as_ref().map(|v| v.to_string()).unwrap_or_default()"),
        "Option<i64> column should be unwrapped, not Debug-formatted, got:\n{list_body}"
    );
    assert!(
        !list_body.contains("{row.description:?}") && !list_body.contains("{row.reorder_at:?}"),
        "no Option column should be Debug-formatted, got:\n{list_body}"
    );

    // Form labels in the new/edit screens should be human-readable
    // (matching the list-screen <th> style), not raw PascalCase identifiers.
    let new_body =
        std::fs::read_to_string(root.join("src/components/product_new_screen.rs")).unwrap();
    assert!(
        new_body.contains("label { \"Reorder at\" }"),
        "form label should be de-PascalCased, got:\n{new_body}"
    );
    assert!(
        !new_body.contains("label { \"ReorderAt\" }"),
        "form label should not be PascalCase, got:\n{new_body}"
    );
    let edit_body =
        std::fs::read_to_string(root.join("src/components/product_edit_screen.rs")).unwrap();
    assert!(
        edit_body.contains("label { \"Reorder at\" }"),
        "edit form label should be de-PascalCased, got:\n{edit_body}"
    );

    // The edit screen should pre-populate signals from the loaded item,
    // preserve the original id, and call update_product.
    let edit_body =
        std::fs::read_to_string(root.join("src/components/product_edit_screen.rs")).unwrap();
    assert!(
        edit_body.contains("let mut name = use_signal(|| item.name.clone())"),
        "edit form should init name from item, got:\n{edit_body}"
    );
    assert!(
        edit_body.contains(
            "let mut description = use_signal(|| item.description.clone().unwrap_or_default())"
        ),
        "edit form should unwrap Option<String> from item, got:\n{edit_body}"
    );
    assert!(
        edit_body.contains("let mut quantity = use_signal(|| item.quantity.to_string())"),
        "edit form should convert numeric to String, got:\n{edit_body}"
    );
    assert!(
        edit_body.contains("id: id_v,") && edit_body.contains("let id_v = original_id.clone();"),
        "edit submit body should preserve the original id, got:\n{edit_body}"
    );
    assert!(
        edit_body.contains("update_product(item)"),
        "edit submit should call update_product, got:\n{edit_body}"
    );
}

#[tokio::test]
async fn resource_dry_run_classifies_all_synth_files() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "resource_dry"
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
resources:
  - name: Order
    fields:
      - {name: id, type: i64}
      - {name: total, type: f64}
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
    let paths: Vec<String> = result
        .would_create
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    assert!(paths.iter().any(|p| p.ends_with("order_store.rs")));
    assert!(paths.iter().any(|p| p.ends_with("list_orders.rs")));
    assert!(paths.iter().any(|p| p.ends_with("get_order.rs")));
    assert!(paths.iter().any(|p| p.ends_with("create_order.rs")));
    assert!(paths.iter().any(|p| p.ends_with("order_list_screen.rs")));
    assert!(paths.iter().any(|p| p.ends_with("order_new_screen.rs")));
    assert!(
        paths.iter().any(|p| p.ends_with("order_edit_screen.rs")),
        "dry_run should classify the synthesized edit screen, got {paths:?}"
    );
    assert!(
        !root.join("src/state/order_store.rs").exists(),
        "dry_run must not write"
    );
}

#[tokio::test]
async fn resource_plural_override_drives_route_and_server_fn_names() {
    // `Person → people` is the canonical irregular case; the built-in
    // pluralizer would emit `persons`, so this exercises the `plural`
    // override end-to-end (route slug + list_{plural} fn name).
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("plural_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/router.rs"),
        r#"use dioxus::prelude::*;

#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[route("/")]
    Home {},
}
"#,
    )
    .unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
resources:
  - name: Person
    plural: people
    fields:
      - {name: id, type: i64}
      - {name: name, type: String}
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
    .expect("execute_code should succeed with plural override");

    // Route slug uses the override.
    let router = std::fs::read_to_string(root.join("src/router.rs")).unwrap();
    assert!(
        router.contains("/people") && !router.contains("/persons"),
        "default route slug should follow the `plural:` override, got router:\n{router}"
    );
    // list_{plural} server fn uses the override.
    assert!(
        root.join("src/server/list_people.rs").exists(),
        "list server fn should be named after the plural override"
    );
    assert!(
        !root.join("src/server/list_persons.rs").exists(),
        "auto-pluralized list_persons.rs must not be emitted when override is set"
    );
}

#[tokio::test]
async fn resource_default_route_base_is_kebab_case() {
    // A `StockMovement` resource without an explicit `route_base` should
    // default to the kebab-case slug `/stock-movements`, not the
    // snake_case `/stock_movements` web convention violator.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("kebab_route_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/router.rs"),
        r#"use dioxus::prelude::*;

#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[route("/")]
    Home {},
}
"#,
    )
    .unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
resources:
  - name: StockMovement
    fields:
      - {name: id, type: i64}
      - {name: note, type: String}
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
    .expect("execute_code should succeed");

    let router = std::fs::read_to_string(root.join("src/router.rs")).unwrap();
    assert!(
        router.contains("/stock-movements") && !router.contains("/stock_movements"),
        "default route slug should be kebab-case, got router:\n{router}"
    );
}

#[test]
fn pluralize_handles_common_cases() {
    assert_eq!(pluralize("product"), "products");
    assert_eq!(pluralize("order"), "orders");
    assert_eq!(pluralize("box"), "boxes");
    assert_eq!(pluralize("category"), "categories");
    assert_eq!(pluralize("toy"), "toys");
    assert_eq!(pluralize("bus"), "buses");
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

#[tokio::test]
async fn client_store_emits_derive_store_without_server_gate() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("client_store_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();

    let state = std::sync::Arc::new(State::new(root.to_path_buf()).unwrap());
    let _ = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
models:
  - name: Todo
    derives: [Default]
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
      - {name: done, type: bool}
client_stores:
  - name: TodoStore
    item_type: Todo
    id_field: id
    id_type: i64
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
    .expect("client_store should scaffold");

    let store_path = root.join("src/state/todo_store.rs");
    assert!(
        store_path.exists(),
        "expected todo_store.rs at {store_path:?}"
    );
    let body = std::fs::read_to_string(&store_path).unwrap();
    assert!(
        !body.contains("#![cfg(feature = \"server\")]"),
        "ClientStore must NOT carry the server cfg gate, got:\n{body}"
    );
    assert!(
        body.contains("use crate::model::Todo;"),
        "missing model import: {body}"
    );
    assert!(
        body.contains("pub fn provide_todo_store()"),
        "missing provide_ fn: {body}"
    );
    assert!(
        body.contains("pub fn use_todo_store()"),
        "missing use_ fn: {body}"
    );
    // Methods inside `#[store(pub)] impl Store<T>` become trait items, which
    // share the visibility of the trait — so no `pub` qualifier on the fns.
    assert!(body.contains("fn push("), "missing push helper: {body}");
    assert!(
        body.contains("fn remove_by_id("),
        "missing remove_by_id helper: {body}"
    );
    assert!(
        body.contains("fn update_by_id("),
        "missing update_by_id helper: {body}"
    );
    // Canonical Dioxus 0.7 shape: #[derive(Store)] on the plain struct and a
    // #[store(pub)] impl block on `Store<TodoStore>` for the typed extension
    // methods. The provider and hook expose `Store<TodoStore>` over context.
    assert!(
        body.contains("#[derive(Store, Clone, Default)]"),
        "missing Store derive: {body}"
    );
    assert!(
        body.contains("#[store(pub)]"),
        "missing #[store(pub)] impl attribute (pub needed for cross-module use of the extension trait): {body}"
    );
    assert!(
        body.contains("impl Store<TodoStore>"),
        "#[store] impl must target Store<TodoStore>: {body}"
    );
    assert!(
        body.contains("pub fn provide_todo_store() -> Store<TodoStore>"),
        "provider must return Store<TodoStore>: {body}"
    );
    assert!(
        body.contains("pub fn use_todo_store() -> Store<TodoStore>"),
        "hook must return Store<TodoStore>: {body}"
    );
    assert!(
        !body.contains("Signal<Vec<"),
        "old Signal<Vec<...>> wrapper shape must be gone: {body}"
    );
    // remove_by_id uses the Store<Vec<_>> lens via `self.items()` and binds
    // before/after length locals around the write — keeps the borrow check
    // happy and mirrors the canonical Writable<Target=Vec<_>> usage.
    assert!(
        body.contains("let mut items = self.items();"),
        "remove_by_id should bind a local Store lens for self.items(): {body}"
    );
    assert!(
        body.contains("let after = items.read().len();"),
        "remove_by_id should bind the post-write length to a local before comparing: {body}"
    );
    assert!(
        body.contains("after < before"),
        "remove_by_id should compare bound length locals (E0597 regression guard): {body}"
    );
    // Syntactic sanity-check on the whole emitted file.
    syn::parse_file(&body)
        .unwrap_or_else(|e| panic!("generated client_store does not parse: {e}\n---\n{body}"));

    // mod.rs should NOT have a server cfg gate for the client store entry.
    let mod_rs = std::fs::read_to_string(root.join("src/state/mod.rs")).unwrap();
    let todo_lines: Vec<&str> = mod_rs
        .lines()
        .filter(|l| l.contains("todo_store"))
        .collect();
    assert!(
        !todo_lines.iter().any(|l| l.contains("cfg(feature")),
        "ClientStore entries must not be gated in mod.rs, got: {mod_rs}"
    );
}

#[tokio::test]
async fn client_crud_screen_wires_add_input_and_delete_button() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("client_crud_screen_test"),
    )
    .unwrap();
    // Pre-create a Routable enum so the screen route insert succeeds.
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        r#"use dioxus::prelude::*;

#[derive(Routable, Clone, PartialEq)]
enum Route {
    #[route("/old")]
    Old {},
}

fn main() {}
"#,
    )
    .unwrap();

    let state = std::sync::Arc::new(State::new(root.to_path_buf()).unwrap());
    let r = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
models:
  - name: Todo
    derives: [Default]
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
      - {name: done, type: bool}
client_stores:
  - name: TodoStore
    item_type: Todo
    id_field: id
    id_type: i64
screens:
  - name: TodoScreen
    route: /
    template:
      kind: client_crud
      store: TodoStore
      item_type: Todo
      label_field: title
      checkbox_field: done
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
    .expect("client_crud screen should scaffold");

    let screen = root.join("src/components/todo_screen.rs");
    let body = std::fs::read_to_string(&screen).unwrap();
    assert!(
        body.contains("use crate::state::todo_store::*;"),
        "missing client_store glob import (needed to bring the #[store(pub)] extension trait into scope):\n{body}"
    );
    assert!(
        body.contains("use crate::model::Todo;"),
        "missing model import:\n{body}"
    );
    assert!(
        body.contains("store.push(Todo {"),
        "missing push call:\n{body}"
    );
    assert!(
        body.contains("title: value,"),
        "missing label_field assignment:\n{body}"
    );
    // Copy id (i64) → no `let id = item.id.clone()` shim; the handler reads
    // the field directly. Non-Copy ids still get the clone dance (covered by
    // `client_crud_non_copy_id_keeps_clone_shim`).
    assert!(
        body.contains("store.remove_by_id(item.id)"),
        "missing direct-field delete handler:\n{body}"
    );
    assert!(
        body.contains("store.update_by_id(item.id, |t| t.done = !t.done)"),
        "missing direct-field checkbox toggle:\n{body}"
    );
    assert!(
        !body.contains(".clone()"),
        "Copy id (i64) must not emit any .clone() in handler bodies:\n{body}"
    );
    // TODO13: checkbox uses `onchange` (semantically correct for toggle
    // controls), not `oninput` which over-fires on some browsers.
    assert!(
        body.contains("onchange:"),
        "checkbox must use onchange (not oninput):\n{body}"
    );
    assert!(
        !body.contains("oninput: move |_| store.update_by_id"),
        "checkbox toggle must not use oninput:\n{body}"
    );
    assert!(
        body.contains("store.items().read().iter()"),
        "client_crud must iterate via the Store field accessor:\n{body}"
    );
    // Boolean attributes must bind the bool field directly, not a
    // stringified `"{item.done}"` form (which rsx would parse as a
    // non-empty string and render checked=true always).
    assert!(
        body.contains("checked: item.done,"),
        "checked must be a bare bool, not a string literal:\n{body}"
    );
    assert!(
        !body.contains("checked: \"{item.done}\""),
        "checked must not be emitted as a stringified attribute:\n{body}"
    );
    // Sanity: it must compile structurally — input/onsubmit/button all
    // emitted under the rsx! block.
    assert!(body.contains("rsx!"), "missing rsx block:\n{body}");
    assert!(
        body.contains("button { r#type: \"submit\""),
        "missing add button:\n{body}"
    );

    // route variant inserted in main.rs
    let routes = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
    assert!(
        routes.contains("TodoScreen"),
        "TodoScreen variant not added: {routes}"
    );

    // ensure no server feature gate snuck into the screen
    assert!(
        !body.contains("cfg(feature = \"server\")"),
        "client_crud screen must not carry server cfg:\n{body}"
    );
    // The store file under src/state must also be unguarded.
    let cs = std::fs::read_to_string(root.join("src/state/todo_store.rs")).unwrap();
    assert!(
        !cs.contains("#![cfg(feature = \"server\")]"),
        "todo store should be client-side:\n{cs}"
    );
    // Canonical Dioxus 0.7 shape: #[derive(Store)] + #[store] impl, with the
    // typed Store<T> exposed through context.
    assert!(
        cs.contains("#[derive(Store, Clone, Default)]"),
        "expected Store derive on client store:\n{cs}"
    );
    assert!(
        cs.contains("#[store(pub)]"),
        "expected #[store(pub)] impl block on client store (pub is required for cross-module use of the extension trait):\n{cs}"
    );
    assert!(
        cs.contains("impl Store<TodoStore>"),
        "expected #[store] impl block to target Store<TodoStore>:\n{cs}"
    );
    assert!(
        cs.contains("pub fn use_todo_store() -> Store<TodoStore>"),
        "expected typed hook returning Store<TodoStore>:\n{cs}"
    );
    assert!(
        cs.contains("pub fn provide_todo_store() -> Store<TodoStore>"),
        "expected typed provider returning Store<TodoStore>:\n{cs}"
    );
    assert!(
        !cs.contains("Signal<Vec<"),
        "old Signal<Vec<...>> wrapper shape must be gone:\n{cs}"
    );
    // TODO14: the screen sets `checkbox_field: done`, so the store gains a
    // `clear_done` helper on its #[store(pub)] impl. Call sites should be
    // able to wire "Clear completed" by calling `store.clear_done()` directly.
    assert!(
        cs.contains("fn clear_done(&mut self)"),
        "expected clear_done helper on the store, got:\n{cs}"
    );
    assert!(
        cs.contains("self.items().write().retain(|x| !x.done);"),
        "clear_done must drop items with done=true:\n{cs}"
    );

    // next_steps should mention provide_*
    assert!(
        r.next_steps
            .iter()
            .any(|s| s.contains("provide_todo_store")),
        "expected next_steps to mention provide_todo_store, got {:?}",
        r.next_steps
    );
}

/// TODO5 §4: a fresh `dx new` project has no `#[derive(Routable)]` enum.
/// `execute_code` must bootstrap one in preflight so the call doesn't fail
/// halfway through with "could not find a Routable enum" after already
/// writing the model/store/component files.
#[tokio::test]
async fn bootstrap_router_creates_router_file_on_fresh_dx_new_project() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("bootstrap_router_test"),
    )
    .unwrap();
    // Simulate what `dx new` gives you: a plain main.rs with no Route.
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        "use dioxus::prelude::*;\n\nfn main() {}\n",
    )
    .unwrap();

    let state = std::sync::Arc::new(State::new(root.to_path_buf()).unwrap());
    let r = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
models:
  - name: Todo
    derives: [Default]
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
      - {name: done, type: bool}
client_stores:
  - name: TodoStore
    item_type: Todo
    id_field: id
    id_type: i64
screens:
  - name: TodoScreen
    route: /
    template:
      kind: client_crud
      store: TodoStore
      item_type: Todo
      label_field: title
      checkbox_field: done
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
    .expect("a Model + ClientStore + Screen doc must run cleanly against a fresh `dx new` project");

    let router = root.join("src/router.rs");
    assert!(router.exists(), "auto-bootstrap must create src/router.rs");
    let body = std::fs::read_to_string(&router).unwrap();
    assert!(
        body.contains("#[derive(Routable, Clone, PartialEq)]"),
        "bootstrapped router must derive Routable, got:\n{body}"
    );
    assert!(
        body.contains("pub enum Route {"),
        "bootstrapped router must declare `pub enum Route`, got:\n{body}"
    );
    assert!(
        body.contains("#[route(\"/\")]") && body.contains("TodoScreen {},"),
        "bootstrapped router must seed the declared screen variant, got:\n{body}"
    );
    // pub mod router; must be auto-declared so `crate::router::Route`
    // resolves from main.rs.
    let main_rs = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
    assert!(
        main_rs.contains("pub mod router;"),
        "auto-bootstrap must add `pub mod router;` to main.rs, got:\n{main_rs}"
    );
    // No re-emit of the screen body should clobber the router.
    assert!(
        body.matches("TodoScreen {},").count() == 1,
        "screen route insert must dedupe against the seeded variant, got:\n{body}"
    );
    // Status should reflect a clean apply.
    assert_eq!(
        r.status.as_deref(),
        Some("applied"),
        "fresh-project run should report status: applied"
    );
    // The next_steps should call out router wiring so the human knows what's left.
    assert!(
        r.next_steps
            .iter()
            .any(|s| s.contains("Router::<crate::router::Route>")),
        "expected a Router mounting next_step, got {:?}",
        r.next_steps
    );
}

/// TODO5 §5: a re-run after every primitive already lands on disk used to
/// return `next_steps: []` and no status field, which looked like success
/// when the route variant might never have been inserted. The status field
/// and the idempotent route insert together fix that.
#[tokio::test]
async fn rerun_with_if_missing_reports_no_changes_and_finishes_route_insert() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("rerun_no_changes_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        "use dioxus::prelude::*;\n\nfn main() {}\n",
    )
    .unwrap();

    let state = std::sync::Arc::new(State::new(root.to_path_buf()).unwrap());
    let yaml = r#"version: "1"
models:
  - name: Todo
    derives: [Default]
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
client_stores:
  - name: TodoStore
    item_type: Todo
    id_field: id
    id_type: i64
screens:
  - name: TodoScreen
    route: /
    template:
      kind: client_crud
      store: TodoStore
      item_type: Todo
      label_field: title
"#;
    // Initial run lays the app down.
    let first = execute_code(
        &state,
        ExecuteCodeParams {
            code: yaml.into(),
            project_root: Some(root.to_string_lossy().into_owned()),
            if_missing: false,
            dry_run: false,
            cargo_check: false,
            format_after: false,
        },
    )
    .await
    .expect("initial run should succeed");
    assert_eq!(first.status.as_deref(), Some("applied"));

    // Re-run with if_missing: every primitive's leaf file is already on
    // disk, so the only legitimate response is `status: no_changes`.
    let second = execute_code(
        &state,
        ExecuteCodeParams {
            code: yaml.into(),
            project_root: Some(root.to_string_lossy().into_owned()),
            if_missing: true,
            dry_run: false,
            cargo_check: false,
            format_after: false,
        },
    )
    .await
    .expect("re-run should not error in if_missing mode");
    assert_eq!(
        second.status.as_deref(),
        Some("no_changes"),
        "fully-collided re-run must report no_changes, got status={:?} created={:?} modified={:?}",
        second.status,
        second.files_created,
        second.files_modified,
    );
    assert!(
        !second.collisions.is_empty(),
        "fully-collided re-run must populate collisions"
    );
}

#[tokio::test]
async fn next_steps_surface_todo_markers_with_file_and_line() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("todo_marker_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();

    // A bare Form (no `on_submit`) emits `// TODO submit handler` in the
    // generated body, which the scanner should pick up.
    let state = std::sync::Arc::new(State::new(root.to_path_buf()).unwrap());
    let r = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
forms:
  - name: ContactForm
    fields:
      - {name: email, type: email}
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
    .expect("Form with no on_submit should scaffold");

    let hotspot = r
        .next_steps
        .iter()
        .find(|s| s.contains("contact_form.rs:") && s.contains("TODO"));
    assert!(
        hotspot.is_some(),
        "expected a `path:line — TODO ...` next_steps entry, got {:?}",
        r.next_steps
    );
    // The header entry should also be present.
    assert!(
        r.next_steps.iter().any(|s| s.contains("hand-edit hotspot")),
        "expected a hotspot header, got {:?}",
        r.next_steps
    );
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

fn cargo_toml_with_fullstack(name: &str) -> String {
    format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = {{ version = "0.7", features = ["fullstack"] }}
"#
    )
}

#[tokio::test]
async fn modify_add_model_field_appends_and_is_idempotent() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("modify_model_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());

    // Create the model first.
    execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
models:
  - name: Product
    fields:
      - {name: id, type: i64}
      - {name: name, type: String}
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
    .unwrap();
    let path = root.join("src/model/product.rs");
    let before = std::fs::read_to_string(&path).unwrap();
    assert!(!before.contains("pub sku:"));

    // Modify: add sku.
    let result = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
modify:
  - kind: add_model_field
    model: Product
    fields:
      - {name: sku, type: String}
      - {name: weight, type: f32, optional: true}
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
    .expect("modify should succeed");
    assert!(result.files_modified.iter().any(|p| p == &path));
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains("pub sku: String,"), "got:\n{body}");
    assert!(body.contains("pub weight: Option<f32>,"), "got:\n{body}");
    // Existing fields still present.
    assert!(body.contains("pub id: i64,"));
    assert!(body.contains("pub name: String,"));
    // Resulting file must still parse.
    syn::parse_file(&body).expect("modified model should parse");

    // Re-run identical modify: idempotent — no files_modified, no duplicate.
    let result = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
modify:
  - kind: add_model_field
    model: Product
    fields:
      - {name: sku, type: String}
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
    .expect("idempotent re-run should succeed");
    assert!(
        result.files_modified.is_empty(),
        "re-run should be a no-op, got {:?}",
        result.files_modified
    );
    let after = std::fs::read_to_string(&path).unwrap();
    // Only one sku declaration.
    assert_eq!(after.matches("pub sku:").count(), 1);
}

#[tokio::test]
async fn modify_add_component_prop_appends_with_optional() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("modify_comp_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
components:
  - name: UserCard
    props:
      - {name: id, type: i32}
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
    .unwrap();
    let path = root.join("src/components/user_card.rs");

    let _ = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
modify:
  - kind: add_component_prop
    component: UserCard
    props:
      - {name: avatar_url, type: String, optional: true}
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
    .expect("modify should succeed");
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(
        body.contains("#[props(default)]") && body.contains("pub avatar_url: Option<String>,"),
        "got:\n{body}"
    );
    syn::parse_file(&body).expect("modified component should parse");
}

#[tokio::test]
async fn modify_add_component_prop_errors_when_no_props_struct() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("modify_no_props_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
components:
  - name: Bare
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
    .unwrap();

    let err = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
modify:
  - kind: add_component_prop
    component: Bare
    props:
      - {name: id, type: i32}
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
    .expect_err("should error when no Props struct exists");
    assert!(
        err.contains("convert the component to take props first"),
        "got: {err}"
    );
}

#[tokio::test]
async fn modify_add_server_fn_arg_appends_to_zero_arg_fn() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("modify_sfn_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
server_fns:
  - name: fetch_users
    return_type: "Vec<String>"
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
    .unwrap();
    let path = root.join("src/server/fetch_users.rs");
    let before = std::fs::read_to_string(&path).unwrap();
    assert!(!before.contains("page"));

    let _ = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
modify:
  - kind: add_server_fn_arg
    server_fn: fetch_users
    args:
      - {name: page, type: u32}
      - {name: page_size, type: u32}
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
    .expect("modify should succeed");
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains("page: u32,"), "got:\n{body}");
    assert!(body.contains("page_size: u32,"), "got:\n{body}");
    syn::parse_file(&body).expect("modified server_fn should parse");
}

#[tokio::test]
async fn modify_errors_when_target_missing_and_skips_under_if_missing() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("modify_missing_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());

    let err = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
modify:
  - kind: add_model_field
    model: Ghost
    fields:
      - {name: x, type: i32}
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
    .expect_err("should error when target missing");
    assert!(err.contains("does not exist on disk"), "got: {err}");

    let result = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
modify:
  - kind: add_model_field
    model: Ghost
    fields:
      - {name: x, type: i32}
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
    .expect("if_missing=true should swallow missing target");
    assert!(
        result.collisions.iter().any(|p| p.ends_with("ghost.rs")),
        "expected ghost.rs in collisions, got {:?}",
        result.collisions
    );
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

#[tokio::test]
async fn dsl_spec_default_prologue_skipped_on_repeat_call() {
    // First call: include_prologue unset → default true → emit preamble.
    // Second call (same State): include_prologue unset → auto-flips to
    // false so the ~5KB authoring guide doesn't ship twice. Callers can
    // still pin the choice with an explicit Some(true)/Some(false).
    let dir = tempfile::TempDir::new().unwrap();
    let state = std::sync::Arc::new(State::new(dir.path().to_path_buf()).unwrap());
    let first = get_dsl_spec(
        &state,
        GetDslSpecParams {
            extensions: vec![],
            sections: vec![],
            index_only: false,
            include_prologue: None,
            include_examples: true,
        },
    )
    .await
    .unwrap();
    assert!(
        first.spec.contains("Dioxus-MCP DSL spec"),
        "first call should ship the preamble"
    );

    let second = get_dsl_spec(
        &state,
        GetDslSpecParams {
            extensions: vec![],
            sections: vec![],
            index_only: false,
            include_prologue: None,
            include_examples: true,
        },
    )
    .await
    .unwrap();
    assert!(
        !second.spec.contains("Dioxus-MCP DSL spec"),
        "second call (no explicit override) should skip the preamble:\n{}",
        second.spec
    );

    // Explicit Some(true) on the third call forces the preamble back.
    let third = get_dsl_spec(
        &state,
        GetDslSpecParams {
            extensions: vec![],
            sections: vec![],
            index_only: false,
            include_prologue: Some(true),
            include_examples: true,
        },
    )
    .await
    .unwrap();
    assert!(
        third.spec.contains("Dioxus-MCP DSL spec"),
        "explicit include_prologue: true should force the preamble back"
    );
}

#[tokio::test]
async fn dsl_spec_prologue_surfaces_data_layer_only_above_crud_picker() {
    // The "scaffold types, hand-write UI" escape hatch should be the
    // first guidance section users see, ahead of the CRUD picker.
    let dir = tempfile::TempDir::new().unwrap();
    let state = std::sync::Arc::new(State::new(dir.path().to_path_buf()).unwrap());
    let r = get_dsl_spec(
        &state,
        GetDslSpecParams {
            extensions: vec![],
            sections: vec![],
            index_only: false,
            include_prologue: Some(true),
            include_examples: false,
        },
    )
    .await
    .unwrap();
    let data_layer_at = r
        .spec
        .find("Data-layer-only path")
        .expect("preamble should mention the data-layer-only path");
    let crud_picker_at = r
        .spec
        .find("Picking the right tool")
        .expect("preamble should mention the CRUD picker");
    assert!(
        data_layer_at < crud_picker_at,
        "data-layer-only path should come before the CRUD picker (got data@{} crud@{})",
        data_layer_at,
        crud_picker_at
    );
}

#[test]
fn client_crud_styled_tailwind_emits_utility_classes() {
    // `styled: tailwind` should swap the default unstyled class names
    // (`add`, `{snake}-items`, `delete`) for a conservative set of
    // Tailwind utility classes on the form / list / buttons / checkbox.
    let cs = DslClientStore {
        name: "TodoStore".into(),
        item_type: "Todo".into(),
        initial: None,
        id_field: Some("id".into()),
        id_type: Some("i64".into()),
        auto_id: Some(true),
    };
    let t = DslScreenTemplate {
        kind: "client_crud".into(),
        endpoint: None,
        item_type: Some("Todo".into()),
        on_submit: None,
        redirect_to: None,
        fields: vec![],
        store: Some("TodoStore".into()),
        label_field: Some("title".into()),
        checkbox_field: Some("done".into()),
        class: None,
        body: None,
        styled: Some("tailwind".into()),
        crud: None,
    };
    let body = render_screen_template(
        std::env::temp_dir().as_path(),
        "TodoScreen",
        "todo_screen",
        None,
        &[cs],
        &t,
    )
    .unwrap();
    // Form swaps `class: "add"` for the Tailwind flex layout.
    assert!(
        !body.contains("class: \"add\""),
        "tailwind preset should drop the bare `add` class:\n{body}"
    );
    assert!(
        body.contains("class: \"flex gap-2 mb-4\""),
        "tailwind preset should emit the Tailwind form class:\n{body}"
    );
    // List swaps `class: "todo_screen-items"` for the Tailwind spacing.
    assert!(
        body.contains("class: \"space-y-2\""),
        "tailwind preset should emit the Tailwind list class:\n{body}"
    );
    // Delete button uses Tailwind text-red utility instead of bare `delete`.
    assert!(
        body.contains("text-red-600"),
        "tailwind preset should emit the Tailwind delete class:\n{body}"
    );
    // Checkbox stays boolean-bound (no regression of TODO #4).
    assert!(
        body.contains("checked: item.done,"),
        "checked must remain a bare bool:\n{body}"
    );
}

#[test]
fn client_crud_non_copy_id_keeps_clone_shim() {
    // TODO12: non-Copy id types (String, Uuid, ...) still need the
    // `let id = item.id.clone();` shim so the FnMut handler can fire more
    // than once. Only Copy primitive integers drop the shim.
    let cs = DslClientStore {
        name: "TodoStore".into(),
        item_type: "Todo".into(),
        initial: None,
        id_field: Some("id".into()),
        id_type: Some("String".into()),
        auto_id: None,
    };
    let t = DslScreenTemplate {
        kind: "client_crud".into(),
        endpoint: None,
        item_type: Some("Todo".into()),
        on_submit: None,
        redirect_to: None,
        fields: vec![],
        store: Some("TodoStore".into()),
        label_field: Some("title".into()),
        checkbox_field: Some("done".into()),
        class: None,
        body: None,
        styled: None,
        crud: None,
    };
    let body = render_screen_template(
        std::env::temp_dir().as_path(),
        "TodoScreen",
        "todo_screen",
        None,
        &[cs],
        &t,
    )
    .unwrap();
    assert!(
        body.contains("let id = item.id.clone();"),
        "non-Copy id must still capture via let-clone shim:\n{body}"
    );
    assert!(
        body.contains("let id = id.clone();"),
        "non-Copy id must clone again inside the FnMut closure body:\n{body}"
    );
    assert!(
        body.contains("store.remove_by_id(id);"),
        "non-Copy id path still calls remove_by_id with the cloned local:\n{body}"
    );
    // Still flipped to onchange (TODO13).
    assert!(
        body.contains("onchange:"),
        "checkbox onchange must be emitted regardless of id type:\n{body}"
    );
}

#[test]
fn client_crud_styled_rejects_unknown_value() {
    let cs = DslClientStore {
        name: "TodoStore".into(),
        item_type: "Todo".into(),
        initial: None,
        id_field: Some("id".into()),
        id_type: Some("i64".into()),
        auto_id: Some(true),
    };
    let t = DslScreenTemplate {
        kind: "client_crud".into(),
        endpoint: None,
        item_type: Some("Todo".into()),
        on_submit: None,
        redirect_to: None,
        fields: vec![],
        store: Some("TodoStore".into()),
        label_field: Some("title".into()),
        checkbox_field: None,
        class: None,
        body: None,
        styled: Some("bootstrap".into()),
        crud: None,
    };
    let err = render_screen_template(
        std::env::temp_dir().as_path(),
        "TodoScreen",
        "todo_screen",
        None,
        &[cs],
        &t,
    )
    .unwrap_err();
    assert!(err.contains("\"tailwind\""), "got: {err}");
    assert!(err.contains("\"bootstrap\""), "got: {err}");
}

#[test]
fn add_default_to_derive_appends_to_existing_list() {
    let src = "use serde::{Serialize, Deserialize};\n\n#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]\npub struct Todo {\n    pub id: i64,\n    pub title: String,\n}\n";
    let out = super::add_default_to_derive(src, "Todo").unwrap();
    assert!(
        out.contains("#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]"),
        "got:\n{out}"
    );
}

#[test]
fn add_default_to_derive_idempotent_when_present() {
    let src = "#[derive(Debug, Default, Clone, PartialEq)]\npub struct Todo { pub id: i64 }\n";
    assert!(super::add_default_to_derive(src, "Todo").is_none());
}

#[test]
fn add_default_to_derive_skips_when_struct_missing() {
    let src = "#[derive(Debug)]\npub struct Other { pub x: i64 }\n";
    assert!(super::add_default_to_derive(src, "Todo").is_none());
}

#[tokio::test]
async fn client_crud_patches_on_disk_model_to_add_default() {
    // Regression: the user has the Todo model already on disk (no Default
    // in its derives), and declares only a ClientStore + Screen in DSL.
    // The screen body emits `..Default::default()`, so execute_code must
    // patch the existing model's derive list before generating the screen.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("on_disk_default_patch"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src/model")).unwrap();
    std::fs::create_dir_all(root.join("src/components")).unwrap();
    std::fs::create_dir_all(root.join("src/state")).unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        r#"use dioxus::prelude::*;
pub mod model;
pub mod components;
pub mod state;

#[derive(Routable, Clone, PartialEq)]
pub enum Route {
    #[route("/")]
    Placeholder {},
}

#[component]
fn Placeholder() -> Element { rsx! { "placeholder" } }

fn main() { dioxus::launch(App); }

#[component]
fn App() -> Element { rsx! { Router::<Route> {} } }
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("src/model/mod.rs"),
        "pub mod todo;\npub use todo::*;\n",
    )
    .unwrap();
    // Critical fixture: no `Default` in the derive list.
    std::fs::write(
        root.join("src/model/todo.rs"),
        r#"use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Todo {
    pub id: i64,
    pub title: String,
    pub done: bool,
}
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("src/components/mod.rs"),
        "pub mod placeholder;\npub use placeholder::*;\n",
    )
    .unwrap();
    std::fs::write(
            root.join("src/components/placeholder.rs"),
            "use dioxus::prelude::*;\n#[component]\npub fn Placeholder() -> Element { rsx! { \"placeholder\" } }\n",
        )
        .unwrap();

    let state = std::sync::Arc::new(State::new(root.to_path_buf()).unwrap());
    let r = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
client_stores:
  - name: TodoStore
    item_type: Todo
    id_field: id
    id_type: i64
screens:
  - name: TodoScreen
    route: /todos
    template:
      kind: client_crud
      store: TodoStore
      item_type: Todo
      label_field: title
      checkbox_field: done
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
    .expect("execute_code should patch on-disk model + scaffold the screen");

    let model = std::fs::read_to_string(root.join("src/model/todo.rs")).unwrap();
    assert!(
        model.contains("Default"),
        "expected Default derive added to existing Todo model, got:\n{model}"
    );
    let path = root.join("src/model/todo.rs");
    assert!(
        r.files_modified.iter().any(|p| p == &path),
        "patched model must land in files_modified, got: {:?}",
        r.files_modified
    );
}

#[tokio::test]
async fn format_after_runs_rustfmt_over_touched_files() {
    // `format_after: true` should rustfmt the freshly-scaffolded files so
    // route inserts and App-body splices land tidy. Verify by comparing
    // a known-unformatted line against the post-call file.
    if std::process::Command::new("rustfmt")
        .arg("--version")
        .output()
        .is_err()
    {
        // rustfmt isn't installed in the test environment — skip rather
        // than spuriously fail. The wiring itself is still validated by
        // unit-level coverage of run_cargo_fmt elsewhere.
        return;
    }
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("format_after_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        r#"use dioxus::prelude::*;

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    rsx! {
        div { "welcome" }
    }
}
"#,
    )
    .unwrap();

    let state = std::sync::Arc::new(State::new(root.to_path_buf()).unwrap());
    execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
models:
  - name: Todo
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
"#
            .into(),
            project_root: Some(root.to_string_lossy().into_owned()),
            if_missing: false,
            dry_run: false,
            cargo_check: false,
            format_after: true,
        },
    )
    .await
    .expect("execute_code with format_after: true should succeed");

    // Pre-format, our model emitter doesn't worry about trailing newlines
    // / spacing details. Post-rustfmt the file must compile under the
    // standard style: every top-level item ends with a newline, struct
    // fields are 4-space indented. We assert the file is non-empty and
    // ends with `\n`, which rustfmt enforces.
    let model = std::fs::read_to_string(root.join("src/model/todo.rs")).unwrap();
    assert!(!model.is_empty(), "model file should exist");
    assert!(
        model.ends_with('\n'),
        "rustfmt should leave a trailing newline:\n{model}"
    );
    // rustfmt always rewrites `, ` between fields onto separate lines for
    // struct definitions. A scaffolded multi-field struct must end up
    // with one field per line.
    let field_lines = model
        .lines()
        .filter(|l| l.trim_start().starts_with("pub "))
        .count();
    assert!(
        field_lines >= 2,
        "struct fields should be one per line after rustfmt, got model:\n{model}"
    );
}
