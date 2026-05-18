use super::super::*;
use super::cargo_toml_with_fullstack;

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
        ("REALTIME_BROWSER_PERSISTENCE", REALTIME_BROWSER_PERSISTENCE),
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
            index_only: Some(false),
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
            index_only: Some(false),
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
            index_only: Some(false),
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
            index_only: Some(true),
            // Drop the prologue so its commentary about `example:` doesn't
            // leak into the index-content assertions below.
            include_prologue: Some(false),
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
            index_only: Some(false),
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
            index_only: Some(false),
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
            index_only: Some(false),
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
            index_only: Some(true),
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
async fn first_call_defaults_to_index_only_plus_prologue() {
    let dummy = std::sync::Arc::new(State::new(std::env::temp_dir()).unwrap());
    // First call: omit index_only AND include_prologue — both should auto-pace
    // to "first call" defaults.
    let first = get_dsl_spec(
        &dummy,
        GetDslSpecParams {
            extensions: vec![],
            sections: vec![],
            index_only: None,
            include_prologue: None,
            include_examples: true,
        },
    )
    .await
    .expect("first call should succeed");
    // Index header lands.
    assert!(
        first.spec.contains("DSL spec — compact index"),
        "expected index marker on first call, got:\n{}",
        first.spec
    );
    // Body fields don't.
    assert!(
        !first.spec.contains("template_kinds:"),
        "expected NO full-spec fields on first call, got:\n{}",
        first.spec
    );

    // Second call (same dummy state): defaults flip — full blocks, no prologue.
    let second = get_dsl_spec(
        &dummy,
        GetDslSpecParams {
            extensions: vec![],
            sections: vec![],
            index_only: None,
            include_prologue: None,
            include_examples: true,
        },
    )
    .await
    .expect("second call should succeed");
    assert!(
        !second.spec.contains("DSL spec — compact index"),
        "expected full spec on second call, got:\n{}",
        second.spec
    );
    assert!(
        second.spec.contains("template_kinds:"),
        "expected full spec fields on second call, got:\n{}",
        second.spec
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
            index_only: Some(false),
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
fn dx_components_catalog_matches_spec_block() {
    // The Rust-side catalog (DX_COMPONENT_CATALOG in execute.rs) and the
    // YAML catalog in CORE_COMPONENTS (specs.rs) need to stay in lockstep —
    // a `dx_components: [foo]` entry that's "valid" by the Rust check but
    // missing from the spec catalog would be a UX bug the next time someone
    // reads the catalog. This test parses the spec block and asserts the
    // two sources have the same set of names AND the same descriptions AND
    // the same prop_hint strings.
    use super::dx_components::DX_COMPONENT_CATALOG_ENTRIES;
    let raw = CORE_COMPONENTS;
    let v: serde_yml::Value = serde_yml::from_str(raw).expect("CORE_COMPONENTS must be valid YAML");
    let components = v
        .get("Components")
        .and_then(|m| m.get("catalog"))
        .and_then(|m| m.as_mapping())
        .expect("Components.catalog must be a mapping");
    let hints = v
        .get("Components")
        .and_then(|m| m.get("prop_hints"))
        .and_then(|m| m.as_mapping())
        .expect("Components.prop_hints must be a mapping");
    let spec_descs: std::collections::BTreeMap<String, String> = components
        .iter()
        .filter_map(|(k, v)| Some((k.as_str()?.to_string(), v.as_str()?.to_string())))
        .collect();
    let spec_hints: std::collections::BTreeMap<String, String> = hints
        .iter()
        .filter_map(|(k, v)| Some((k.as_str()?.to_string(), v.as_str()?.to_string())))
        .collect();
    let code_descs: std::collections::BTreeMap<String, String> = DX_COMPONENT_CATALOG_ENTRIES
        .iter()
        .map(|(n, d, _)| (n.to_string(), d.to_string()))
        .collect();
    let code_hints: std::collections::BTreeMap<String, String> = DX_COMPONENT_CATALOG_ENTRIES
        .iter()
        .map(|(n, _, h)| (n.to_string(), h.to_string()))
        .collect();
    assert_eq!(
        spec_descs, code_descs,
        "spec catalog descriptions and DX_COMPONENT_CATALOG_ENTRIES descriptions must match; refresh both when the upstream registry changes"
    );
    assert_eq!(
        spec_hints, code_hints,
        "spec prop_hints and DX_COMPONENT_CATALOG_ENTRIES hints must match; refresh both when the upstream registry changes"
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
            index_only: Some(false),
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
            index_only: Some(false),
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
            index_only: Some(false),
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
            index_only: Some(false),
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
