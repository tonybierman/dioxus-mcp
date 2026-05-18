use super::super::*;
use super::cargo_toml_with_fullstack;

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
    let r = generate_model(dir.path(), &m, &Default::default()).unwrap();
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
async fn execute_code_model_auto_imports_sibling_models() {
    // A model field whose type is another in-doc Model should emit
    // `use crate::model::{snake}::{Pascal};` at the top of the generated file
    // — including when the reference is wrapped in Vec / Option.
    use crate::tools::dsl::ExecuteCodeParams;
    use crate::tools::dsl::execute_code;
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("cross_model_imports"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        "use dioxus::prelude::*;\nfn main() {}\n",
    )
    .unwrap();

    let state = std::sync::Arc::new(State::new(root.to_path_buf()).unwrap());
    let _ = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
models:
  - name: Column
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
  - name: Board
    fields:
      - {name: id, type: i64}
      - {name: columns, type: "Vec<Column>"}
      - {name: featured, type: Column, optional: true}
      - {name: explicit, type: "crate::model::column::Column"}
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
    .expect("scaffold should succeed");

    let board = std::fs::read_to_string(root.join("src/model/board.rs")).unwrap();
    assert!(
        board.contains("use crate::model::column::Column;"),
        "Board should auto-import Column: {board}"
    );
    // Only one import line — `Vec<Column>` and `Column` (twice) share it.
    assert_eq!(
        board.matches("use crate::model::column::Column;").count(),
        1,
        "exactly one Column import expected: {board}"
    );
    // The path-qualified field is still emitted as written.
    assert!(
        board.contains("pub explicit: crate::model::column::Column,"),
        "path-qualified field preserved: {board}"
    );

    // The Column model itself does not get a `use crate::model::column::Column;`
    // self-import.
    let column = std::fs::read_to_string(root.join("src/model/column.rs")).unwrap();
    assert!(
        !column.contains("use crate::model::column::"),
        "Column should not import itself: {column}"
    );
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
    // With `checkbox_field` set, the row body lives in a sibling
    // `TodoScreenRow` component (decomposed for a clean `todo: Todo` prop
    // boundary). The parent's `for` loop just renders that component.
    assert!(
        body.contains("#[component]\nfn TodoScreenRow(todo: Todo) -> Element {"),
        "missing sibling row component:\n{body}"
    );
    assert!(
        body.contains("TodoScreenRow { key: \"{item.id}\", todo: item.clone() }"),
        "parent loop must delegate to the sibling row component:\n{body}"
    );
    // Copy id (i64) → no `let id = todo.id.clone()` shim; the handler reads
    // the field directly. Non-Copy ids still get the clone dance (covered by
    // `client_crud_non_copy_id_keeps_clone_shim`).
    assert!(
        body.contains("store.remove_by_id(todo.id)"),
        "missing direct-field delete handler:\n{body}"
    );
    assert!(
        body.contains("store.update_by_id(todo.id, |t| t.done = !t.done)"),
        "missing direct-field checkbox toggle:\n{body}"
    );
    // The single `item.clone()` at the call site is expected — the prop
    // takes ownership of the row's item. No other `.clone()` should land
    // inside the handler bodies for a Copy id.
    let clone_count = body.matches(".clone()").count();
    assert_eq!(
        clone_count, 1,
        "Copy id (i64) row component should only carry the call-site item.clone(); body has {clone_count} clones:\n{body}"
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
    // stringified `"{todo.done}"` form (which rsx would parse as a
    // non-empty string and render checked=true always).
    assert!(
        body.contains("checked: todo.done,"),
        "checked must be a bare bool, not a string literal:\n{body}"
    );
    assert!(
        !body.contains("checked: \"{todo.done}\""),
        "checked must not be emitted as a stringified attribute:\n{body}"
    );
    // The row component re-acquires the store from context — cheap, and
    // avoids plumbing the store through a prop.
    assert!(
        body.contains("    let store = use_todo_store();"),
        "row component must call use_todo_store():\n{body}"
    );
    // Sanity: it must compile structurally — input/onsubmit/button all
    // emitted under the rsx! block.
    assert!(body.contains("rsx!"), "missing rsx block:\n{body}");
    assert!(
        body.contains("button { r#type: \"submit\""),
        "missing add button:\n{body}"
    );
    // Accessibility defaults: per-row delete button uses a × glyph plus an
    // aria_label tied to the row's label_field so AT users hear
    // "Delete {title}" instead of just "Delete". Checkbox also carries a
    // matching aria_label so the toggle isn't announced as "checkbox" alone.
    assert!(
        body.contains("aria_label: \"Delete {todo.title}\""),
        "missing aria_label on delete button:\n{body}"
    );
    assert!(
        body.contains("\"\u{00D7}\""),
        "delete button text must be the × glyph (aria_label carries meaning):\n{body}"
    );
    assert!(
        body.contains("aria_label: \"Toggle {todo.title}\""),
        "missing aria_label on checkbox:\n{body}"
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
        compose_style: None,
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
    // Checkbox stays boolean-bound (no regression of TODO #4). With
    // checkbox_field set the row body lives in TodoScreenRow, so the prop
    // binding is `todo`, not `item`.
    assert!(
        body.contains("checked: todo.done,"),
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
        compose_style: None,
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
    // With `checkbox_field` set the row body lives in the sibling
    // `TodoScreenRow` component; the clone shim now binds against the
    // `todo` prop instead of the `item` loop binding.
    assert!(
        body.contains("let id = todo.id.clone();"),
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
fn client_crud_styled_vanilla_css_emits_semantic_classes() {
    // `styled: vanilla-css` should swap the default unstyled class names
    // for semantic ones (`compose`, `list`, `field`, `toggle`, `delete`, …)
    // matched to the starter stylesheet the screen will reference.
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
        styled: Some("vanilla-css".into()),
        compose_style: None,
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
    // Compose form uses `.compose` not `.add`.
    assert!(
        body.contains("class: \"compose\""),
        "vanilla-css preset should emit `.compose` for the form:\n{body}"
    );
    assert!(
        !body.contains("class: \"add\""),
        "vanilla-css preset should drop the bare `add` class:\n{body}"
    );
    // List uses `.list` not `{snake}-items`.
    assert!(
        body.contains("class: \"list\""),
        "vanilla-css preset should emit `.list`:\n{body}"
    );
    // Field, toggle, delete.
    assert!(body.contains("class: \"field\""), "missing .field:\n{body}");
    assert!(
        body.contains("class: \"toggle\""),
        "missing .toggle:\n{body}"
    );
    assert!(
        body.contains("class: \"delete\""),
        "missing .delete:\n{body}"
    );
}

#[tokio::test]
async fn client_crud_styled_vanilla_css_emits_starter_stylesheet() {
    use crate::tools::dsl::ExecuteCodeParams;
    use crate::tools::dsl::execute_code;
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("vanilla_css_starter_test"),
    )
    .unwrap();
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
    auto_id: true
screens:
  - name: TodoScreen
    route: /
    template:
      kind: client_crud
      store: TodoStore
      item_type: Todo
      label_field: title
      checkbox_field: done
      styled: vanilla-css
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
    .expect("vanilla-css scaffold should succeed");

    let css = root.join("assets/todo_screen.css");
    assert!(
        css.exists(),
        "starter stylesheet must be written to assets/{{snake}}.css"
    );
    assert!(
        r.files_created.iter().any(|p| p == &css),
        "files_created must include the starter CSS path: {:?}",
        r.files_created
    );
    let body = std::fs::read_to_string(&css).unwrap();
    // Spot-check the contract: the sheet keys off `.screen.{snake}` and
    // styles each of the semantic class names the rsx! emits.
    assert!(
        body.contains(".screen.todo_screen"),
        "missing root selector:\n{body}"
    );
    assert!(body.contains(".compose"), "missing .compose:\n{body}");
    assert!(body.contains(".row"), "missing .row:\n{body}");
    assert!(body.contains(".delete"), "missing .delete:\n{body}");
    // Mount-hint should surface in next_steps so the agent knows to wire
    // the stylesheet into App.
    assert!(
        r.next_steps.iter().any(|s| s.contains("todo_screen.css")),
        "missing stylesheet mount hint in next_steps: {:?}",
        r.next_steps
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
        compose_style: None,
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
fn client_crud_compose_style_enter_only_drops_submit_button() {
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
        styled: None,
        compose_style: Some("enter_only".into()),
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
    // onsubmit still wired up (Enter fires the form), but the button is gone.
    assert!(body.contains("onsubmit:"), "must keep onsubmit:\n{body}");
    assert!(
        !body.contains("r#type: \"submit\""),
        "must omit the submit button:\n{body}"
    );
}

#[test]
fn client_crud_compose_style_default_keeps_submit_button() {
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
        styled: None,
        compose_style: None,
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
        body.contains("r#type: \"submit\""),
        "default must keep submit button:\n{body}"
    );
}

#[tokio::test]
async fn view_state_with_enum_variants_generates_enum_and_wires_app() {
    use crate::tools::dsl::ExecuteCodeParams;
    use crate::tools::dsl::execute_code;
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        cargo_toml_with_fullstack("view_state_test"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        r#"use dioxus::prelude::*;

fn main() {}

#[component]
fn App() -> Element {
    rsx! { div { "placeholder" } }
}
"#,
    )
    .unwrap();

    let state = std::sync::Arc::new(State::new(root.to_path_buf()).unwrap());
    let _ = execute_code(
        &state,
        ExecuteCodeParams {
            code: r#"version: "1"
view_states:
  - name: TodoFilter
    type: TodoFilter
    initial: "TodoFilter::All"
    enum_variants: [All, Active, Done]
  - name: SearchQuery
    type: String
    initial: "String::new()"
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
    .expect("view_state scaffold should succeed");

    let body = std::fs::read_to_string(root.join("src/state/todo_filter.rs")).unwrap();
    assert!(body.contains("pub enum TodoFilter {"));
    assert!(body.contains("pub fn provide_todo_filter()"));
    assert!(body.contains("pub fn use_todo_filter()"));

    let search = std::fs::read_to_string(root.join("src/state/search_query.rs")).unwrap();
    assert!(
        !search.contains("pub enum"),
        "no enum should be emitted when enum_variants is absent:\n{search}"
    );

    // Both provide_* hooks should be spliced into App.
    let main_rs = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
    assert!(
        main_rs.contains("crate::state::todo_filter::provide_todo_filter();"),
        "App body should call provide_todo_filter:\n{main_rs}"
    );
    assert!(
        main_rs.contains("crate::state::search_query::provide_search_query();"),
        "App body should call provide_search_query:\n{main_rs}"
    );
}

#[test]
fn client_crud_without_checkbox_field_keeps_row_inline() {
    // The Row split is gated on `checkbox_field` — without it the row body
    // is trivial (just label + delete button) and stays inside the parent's
    // `for` loop.
    let cs = DslClientStore {
        name: "DraftStore".into(),
        item_type: "Draft".into(),
        initial: None,
        id_field: Some("id".into()),
        id_type: Some("i64".into()),
        auto_id: Some(true),
    };
    let t = DslScreenTemplate {
        kind: "client_crud".into(),
        endpoint: None,
        item_type: Some("Draft".into()),
        on_submit: None,
        redirect_to: None,
        fields: vec![],
        store: Some("DraftStore".into()),
        label_field: Some("title".into()),
        checkbox_field: None,
        class: None,
        body: None,
        styled: None,
        compose_style: None,
        crud: None,
    };
    let body = render_screen_template(
        std::env::temp_dir().as_path(),
        "DraftScreen",
        "draft_screen",
        None,
        &[cs],
        &t,
    )
    .unwrap();
    assert!(
        !body.contains("DraftScreenRow"),
        "row split should not fire without checkbox_field:\n{body}"
    );
    assert!(
        body.contains("for item in store.items().read().iter()"),
        "loop must stay over `item`:\n{body}"
    );
    assert!(
        body.contains("li { key: \"{item.id}\""),
        "row body must remain inline as an `li`:\n{body}"
    );
}

#[test]
fn client_crud_compose_style_rejects_unknown_value() {
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
        styled: None,
        compose_style: Some("inline".into()),
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
    assert!(err.contains("compose_style"), "got: {err}");
    assert!(err.contains("\"inline\""), "got: {err}");
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
