use super::super::*;
use super::cargo_toml_with_fullstack;

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

#[tokio::test]
async fn resource_auth_required_propagates_prologue_to_every_synth_server_fn() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("resource_auth_required_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();

    let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
    execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
resources:
  - name: Card
    auth_required: true
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
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
    .expect("resource auth_required scaffold should succeed");

    // All five synth fns must carry the same prologue.
    for name in [
        "list_cards",
        "get_card",
        "create_card",
        "update_card",
        "delete_card",
    ] {
        let body = std::fs::read_to_string(root.join(format!("src/server/{name}.rs"))).unwrap();
        assert!(
            body.contains("cookies: TypedHeader<Cookie>"),
            "{name} should auto-add cookies extractor; got:\n{body}"
        );
        // Verb-macro attribute only — no duplicate in the fn signature.
        let cookie_occurrences = body.matches("cookies: TypedHeader<Cookie>").count();
        assert_eq!(
            cookie_occurrences, 1,
            "{name}: cookies extractor must appear only in the verb-macro attr; got:\n{body}"
        );
        assert!(
            body.contains(".get(\"session_id\")"),
            "{name} should read the session_id cookie; got:\n{body}"
        );
        assert!(
            body.contains("\"not logged in\""),
            "{name} should map missing cookie to ServerFnError; got:\n{body}"
        );
        syn::parse_file(&body).expect("synth server_fn should still parse with prologue");
    }
}
