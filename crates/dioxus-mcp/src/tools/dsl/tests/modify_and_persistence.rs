use super::super::*;
use super::cargo_toml_with_fullstack;

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
async fn browser_persistence_local_storage_emits_wasm_and_ssr_branches() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("bp_local_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
browser_persistence:
  - name: PrefsStorage
    backend: local_storage
    key: "user.prefs"
    value_type: "String"
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
    .expect("local_storage scaffold should succeed");

    let path = root.join("src/storage/prefs_storage.rs");
    let body = std::fs::read_to_string(&path).expect("storage file should exist");
    assert!(
        body.contains("#[cfg(target_arch = \"wasm32\")]\npub fn read()"),
        "wasm read branch missing; got:\n{body}"
    );
    assert!(
        body.contains("#[cfg(not(target_arch = \"wasm32\"))]\npub fn read()"),
        "ssr no-op read branch missing; got:\n{body}"
    );
    assert!(
        body.contains("local_storage()"),
        "local_storage backend should invoke window().local_storage(); got:\n{body}"
    );
    assert!(
        body.contains("\"user.prefs\""),
        "storage key should appear verbatim; got:\n{body}"
    );
    // String value_type → no serde_json round-trip in the file.
    assert!(
        !body.contains("serde_json"),
        "String value_type should not pull in serde_json; got:\n{body}"
    );
    syn::parse_file(&body).expect("generated storage file should parse");

    // mod.rs entry is auto-upserted.
    let mod_rs = std::fs::read_to_string(root.join("src/storage/mod.rs")).unwrap();
    assert!(
        mod_rs.contains("pub mod prefs_storage;"),
        "src/storage/mod.rs should expose prefs_storage; got:\n{mod_rs}"
    );
}

#[tokio::test]
async fn browser_persistence_typed_value_round_trips_through_serde_json() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("bp_typed_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
models:
  - name: Draft
    fields:
      - {name: title, type: String}
      - {name: body, type: String}
browser_persistence:
  - name: DraftStorage
    backend: session_storage
    key: "compose.draft"
    value_type: "Draft"
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
    .expect("typed-value scaffold should succeed");

    let body = std::fs::read_to_string(root.join("src/storage/draft_storage.rs")).unwrap();
    assert!(
        body.contains("session_storage()"),
        "session_storage backend should invoke window().session_storage(); got:\n{body}"
    );
    assert!(
        body.contains("serde_json::from_str::<Draft>(&raw)"),
        "typed value_type should deserialize via serde_json; got:\n{body}"
    );
    assert!(
        body.contains("serde_json::to_string(value)"),
        "typed value_type should serialize via serde_json; got:\n{body}"
    );
    // Cross-imported because Draft is declared in the same doc.
    assert!(
        body.contains("use crate::model::Draft;"),
        "value_type that matches a doc Model should auto-import; got:\n{body}"
    );
    syn::parse_file(&body).expect("typed storage file should parse");
}

#[tokio::test]
async fn browser_persistence_cookie_backend_uses_document_cookie() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("bp_cookie_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
browser_persistence:
  - name: ConsentCookie
    backend: cookie
    key: "cookie_consent"
    value_type: "String"
    cookie_attributes: "path=/; max-age=31536000; SameSite=Lax"
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
    .expect("cookie scaffold should succeed");

    let body = std::fs::read_to_string(root.join("src/storage/consent_cookie.rs")).unwrap();
    assert!(
        body.contains("HtmlDocument"),
        "cookie backend should dyn_into HtmlDocument; got:\n{body}"
    );
    assert!(
        body.contains("fn parse_cookie"),
        "cookie backend should define parse_cookie helper; got:\n{body}"
    );
    assert!(
        body.contains("path=/; max-age=31536000; SameSite=Lax"),
        "custom cookie_attributes should appear in the file; got:\n{body}"
    );
    assert!(
        body.contains("Max-Age=0"),
        "clear() should expire the cookie via Max-Age=0; got:\n{body}"
    );
    syn::parse_file(&body).expect("cookie storage file should parse");
}

#[tokio::test]
async fn browser_persistence_rejects_unknown_backend() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("bp_bad_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    let err = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
browser_persistence:
  - name: Bogus
    backend: indexed_db
    key: "k"
    value_type: "String"
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
    .expect_err("unknown backend should fail preflight");
    assert!(
        err.contains("unknown backend") && err.contains("indexed_db"),
        "error should name the bad backend; got: {err}"
    );
}

#[tokio::test]
async fn server_fn_auth_required_injects_cookie_extractor_and_prologue() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("server_fn_auth_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
server_fns:
  - name: fetch_inbox
    method: get
    path: /api/inbox
    auth_required: true
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
    .expect("auth_required scaffold should succeed");

    let body = std::fs::read_to_string(root.join("src/server/fetch_inbox.rs")).unwrap();
    // Cookie extractor lives ONLY in the verb-macro — the Dioxus 0.7.9 macro
    // binds `cookies` into scope itself; repeating it in the fn signature
    // breaks `FromRequest` for the body tuple.
    assert!(
        body.contains("#[get(\"/api/inbox\", cookies: TypedHeader<Cookie>)]"),
        "auth_required should auto-add cookies extractor to the macro; got:\n{body}"
    );
    assert!(
        !body.contains("cookies: TypedHeader<Cookie>,"),
        "extractor must NOT be duplicated into the fn signature; got:\n{body}"
    );
    // Prologue lines.
    assert!(
        body.contains("cookies\n        .get(\"session_id\")"),
        "auth_required should read the session_id cookie; got:\n{body}"
    );
    assert!(
        body.contains("\"not logged in\""),
        "auth_required should map the missing-cookie case to a ServerFnError; got:\n{body}"
    );
    assert!(
        body.contains("TODO touch_session"),
        "auth_required should leave a touch_session TODO marker; got:\n{body}"
    );
    syn::parse_file(&body).expect("auth_required server_fn should parse");
}

#[tokio::test]
async fn server_fn_auth_required_respects_custom_session_cookie() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("server_fn_auth_cookie_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
server_fns:
  - name: fetch_inbox
    method: get
    auth_required: true
    session_cookie: "sid"
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
    .expect("custom session_cookie scaffold should succeed");

    let body = std::fs::read_to_string(root.join("src/server/fetch_inbox.rs")).unwrap();
    assert!(
        body.contains(".get(\"sid\")"),
        "custom session_cookie should be read; got:\n{body}"
    );
}

#[tokio::test]
async fn server_fn_auth_required_keeps_caller_supplied_cookie_extractor() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("server_fn_auth_cj_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
server_fns:
  - name: fetch_inbox
    method: get
    auth_required: true
    extractors:
      - {name: cookies, type: "CookieJar"}
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
    .expect("caller-supplied cookies extractor should win");

    let body = std::fs::read_to_string(root.join("src/server/fetch_inbox.rs")).unwrap();
    assert!(
        body.contains("cookies: CookieJar"),
        "caller-supplied cookies type should be preserved (in the verb-macro \
         attr); got:\n{body}"
    );
    assert!(
        !body.contains("TypedHeader<Cookie>"),
        "auth_required should NOT clobber the caller's cookies extractor; got:\n{body}"
    );
    // The extractor lives in the verb-macro only — it must not be repeated in
    // the rust fn signature.
    let cookies_jar_occurrences = body.matches("cookies: CookieJar").count();
    assert_eq!(
        cookies_jar_occurrences, 1,
        "cookies extractor must appear only in the verb-macro attribute, not also \
         in the fn signature; got:\n{body}"
    );
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
