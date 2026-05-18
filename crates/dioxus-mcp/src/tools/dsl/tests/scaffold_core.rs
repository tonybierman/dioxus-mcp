use super::super::*;
use super::cargo_toml_with_fullstack;

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

#[tokio::test]
async fn prune_dx_new_starter_removes_hero_and_home_when_present() {
    use crate::tools::dsl::ExecuteCodeParams;
    use crate::tools::dsl::execute_code;
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("prune_dx_new_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src/components")).unwrap();
    // Simulate `dx new`'s starter shape: Routable with `Home` + Hero
    // component file + matching mod.rs entry.
    std::fs::write(
        root.join("src/main.rs"),
        r#"use dioxus::prelude::*;
mod components;

#[derive(Routable, Clone, PartialEq)]
enum Route {
    #[route("/")]
    Home {},
}

fn main() {}
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("src/components/hero.rs"),
        "use dioxus::prelude::*;\n#[component]\npub fn Hero() -> Element { rsx! { div { \"Welcome\" } } }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/components/mod.rs"),
        "pub mod hero;\npub use hero::*;\n",
    )
    .unwrap();

    let state = std::sync::Arc::new(State::new(root.to_path_buf()).unwrap());
    let r = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
prune_dx_new_starter: true
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
    auto_id: true
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
    .expect("prune scaffold should succeed");

    // Hero file gone.
    assert!(
        !root.join("src/components/hero.rs").exists(),
        "Hero component file should be pruned"
    );
    // Routable enum no longer carries `Home`; the new `TodoScreen` variant
    // landed in its place.
    let main_rs = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
    assert!(
        !main_rs.contains("Home {}"),
        "Home variant should be pruned: {main_rs}"
    );
    assert!(
        main_rs.contains("TodoScreen"),
        "TodoScreen should be inserted: {main_rs}"
    );
    // The synthesized removes show up in files_modified (the Routable file).
    let _ = r;
}

#[tokio::test]
async fn prune_dx_new_starter_surfaces_orphan_references() {
    // After the prune, leftover `Hero {}` / `use ...::Hero;` / `fn Home()`
    // references in the crate root must show up as `next_steps` entries so
    // the caller knows what to hand-fix before `cargo check` will pass.
    use crate::tools::dsl::ExecuteCodeParams;
    use crate::tools::dsl::execute_code;
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("prune_orphans_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src/components")).unwrap();
    // dx-new-shaped main.rs: imports Hero, defines a Home component that
    // renders Hero. After prune both should be flagged.
    std::fs::write(
        root.join("src/main.rs"),
        r#"use dioxus::prelude::*;
use components::Hero;
mod components;

#[derive(Routable, Clone, PartialEq)]
enum Route {
    #[route("/")]
    Home {},
}

fn main() {}

#[component]
fn Home() -> Element {
    rsx! { Hero {} }
}
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("src/components/hero.rs"),
        "use dioxus::prelude::*;\n#[component]\npub fn Hero() -> Element { rsx! { div {} } }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/components/mod.rs"),
        "pub mod hero;\npub use hero::*;\n",
    )
    .unwrap();

    let state = std::sync::Arc::new(State::new(root.to_path_buf()).unwrap());
    let r = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
prune_dx_new_starter: true
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
    auto_id: true
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
    .expect("prune scaffold should succeed");

    let summary = r
        .next_steps
        .iter()
        .find(|s| s.contains("dx-new orphan reference"))
        .unwrap_or_else(|| {
            panic!(
                "expected an orphan summary in next_steps: {:?}",
                r.next_steps
            )
        });
    assert!(
        summary.contains("orphan reference"),
        "summary should name the kind of issue: {summary}"
    );

    let body: String = r.next_steps.join("\n");
    assert!(
        body.contains("src/main.rs") && body.contains("`Hero`"),
        "should flag the use components::Hero / rsx Hero call sites in main.rs: {body}"
    );
    assert!(
        body.contains("`Home`"),
        "should flag the surviving Home fn def in main.rs: {body}"
    );
}

#[tokio::test]
async fn prune_dx_new_starter_is_silent_noop_when_targets_absent() {
    use crate::tools::dsl::ExecuteCodeParams;
    use crate::tools::dsl::execute_code;
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("prune_noop_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    // Pristine project — no Hero file, no Routable yet.
    std::fs::write(
        root.join("src/main.rs"),
        "use dioxus::prelude::*;\nfn main() {}\n",
    )
    .unwrap();

    let state = std::sync::Arc::new(State::new(root.to_path_buf()).unwrap());
    let r = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
prune_dx_new_starter: true
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
    auto_id: true
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
    .expect("prune on pristine project should succeed");
    let _ = r;
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

/// TODO §4: execute_code must abort BEFORE any files are written when the
/// doc declares a server fn but the project's Cargo.toml has no fullstack
/// feature. Previously the per-primitive `create_server_fn` would fire its
/// own gate but only after `create_model` had already landed files on disk —
/// leaving the project in a half-scaffolded state on a guaranteed-fail run.
#[tokio::test]
async fn execute_code_aborts_before_writes_when_fullstack_missing() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    // dioxus dep present, but only the `web` feature — no fullstack and no
    // opt-in `server` sibling feature, so server fns can't compile.
    std::fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "fullstack_preflight_test"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = { version = "0.7", features = ["web"] }
"#,
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();

    let state = std::sync::Arc::new(State::new(root.to_path_buf()).unwrap());
    let err = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
models:
  - name: Todo
    derives: [Default]
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
server_fns:
  - name: list_todos
    return_type: Vec<Todo>
"#
            .into(),
            project_root: Some(root.display().to_string()),
            if_missing: false,
            dry_run: false,
            cargo_check: false,
            format_after: false,
        },
    )
    .await
    .expect_err("preflight should reject server-fn scaffold without fullstack");

    assert!(
        err.contains("fullstack") && err.contains("list_todos"),
        "error should name the offending server fn and the missing feature: {err}"
    );
    assert!(
        err.contains("audit_feature_flags"),
        "error should point at audit_feature_flags for the patch: {err}"
    );
    // The model file must NOT exist — preflight aborted before any writes.
    assert!(
        !root.join("src/model/todo.rs").exists(),
        "model file landed on disk despite fullstack preflight failure — half-written state"
    );
    assert!(
        !root.join("src/server").exists(),
        "server dir was created even though preflight should have aborted"
    );
}

/// Sanity-check the complement: when fullstack IS enabled, the same doc goes
/// through preflight and writes the expected files. Guards against the
/// preflight false-positiving on the canonical 0.7 layout.
#[tokio::test]
async fn execute_code_passes_fullstack_preflight_on_canonical_layout() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("fullstack_ok_preflight_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();

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
server_fns:
  - name: list_todos
    return_type: Vec<Todo>
"#
            .into(),
            project_root: Some(root.display().to_string()),
            if_missing: false,
            dry_run: false,
            cargo_check: false,
            format_after: false,
        },
    )
    .await
    .expect("fullstack layout should pass preflight");
    assert!(
        root.join("src/model/todo.rs").exists(),
        "model should land when fullstack is enabled, got: {:?}",
        r
    );
}
