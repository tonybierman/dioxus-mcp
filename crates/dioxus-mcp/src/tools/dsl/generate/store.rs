use std::collections::BTreeSet;
use std::path::Path;

use heck::{ToPascalCase, ToSnakeCase};
use minijinja::context;

use crate::tools::scaffold::ScaffoldResult;

use super::super::render::*;
use super::super::templates::*;
use super::super::types::*;

pub(crate) fn generate_store(
    crate_root: &Path,
    store: &DslStore,
) -> Result<ScaffoldResult, String> {
    let kind = store.kind.as_deref().unwrap_or("in_memory");
    if kind != "in_memory" {
        return Err(format!(
            "store {:?}: kind {kind:?} not implemented yet (only `in_memory`)",
            store.name
        ));
    }
    let store_pascal = store.name.to_pascal_case();
    let store_snake = store.name.to_snake_case();
    let res_pascal = store.resource.to_pascal_case();
    let id_field = store.id_field.as_deref().unwrap_or("id").to_snake_case();
    let id_type = store.id_type.as_deref().unwrap_or("i64").to_string();
    let emit_tests = store.emit_tests.unwrap_or(false);
    let body = render(
        "store",
        STORE_TPL,
        context! {
            store_pascal => store_pascal.clone(),
            res_pascal => res_pascal,
            id_field => id_field,
            id_type => id_type,
            emit_tests => emit_tests,
        },
    )?;
    let mut r = write_module_file(crate_root, "src/state", &store_snake, body)?;
    if emit_tests {
        r.next_steps.push(format!(
            "run `cargo test --features server -p <crate>` to execute the generated CRUD tests for {store_pascal}"
        ));
    }
    Ok(r)
}

pub(crate) fn generate_client_store(
    crate_root: &Path,
    cs: &DslClientStore,
    model_names: &BTreeSet<String>,
    // Optional bool-field name (e.g. `done`, `completed`) collected from any
    // `client_crud` Screen template that references this store. When set, the
    // generator emits a `clear_{field}` helper on the `#[store(pub)] impl` so
    // call sites don't have to reach into `store.items().write().retain(...)`
    // to implement the canonical "Clear completed" action.
    checkbox_field: Option<&str>,
) -> Result<ScaffoldResult, String> {
    let pascal = cs.name.to_pascal_case();
    let snake = cs.name.to_snake_case();
    let item_type = cs.item_type.trim().to_string();
    let id_field = cs.id_field.as_ref().map(|s| s.to_snake_case());
    let id_type = cs.id_type.clone().unwrap_or_else(|| "i64".into());
    let initial = cs.initial.clone().unwrap_or_else(|| "Vec::new()".into());
    let auto_id = cs.auto_id.unwrap_or(false);
    let checkbox_field = checkbox_field.map(|s| s.to_snake_case());
    if auto_id {
        if id_field.is_none() {
            return Err(format!(
                "client_store {:?}: `auto_id: true` requires `id_field` to be set so the allocator knows which field to assign",
                cs.name
            ));
        }
        if !is_primitive_integer_ty(&id_type) {
            return Err(format!(
                "client_store {:?}: `auto_id: true` requires a primitive integer `id_type` (i8..i128/u8..u128/isize/usize), got {id_type:?}",
                cs.name
            ));
        }
    }
    let id_type_suffix = if auto_id {
        id_type.clone()
    } else {
        String::new()
    };
    // Emit `use crate::model::ItemType;` when the type matches an in-doc model.
    let needs_model_import = model_names.contains(&item_type.to_snake_case());

    let body = render(
        "client_store",
        CLIENT_STORE_TPL,
        context! {
            pascal => pascal,
            snake => snake.clone(),
            item_type => item_type,
            needs_model_import => needs_model_import,
            id_field => id_field,
            id_type => id_type,
            id_type_suffix => id_type_suffix,
            initial => initial,
            auto_id => auto_id,
            checkbox_field => checkbox_field,
        },
    )?;
    // No server cfg gate — ClientStore runs in both wasm and server builds.
    // `provide_*` wiring is handled by wire_app_if_needed — it either splices
    // the call into `fn App()` automatically or, on hand-rolled layouts,
    // surfaces a tailored hint with the file path. Pushing a generic hint
    // here would duplicate that messaging on every successful run.
    let r = write_module_file_with_cfg(crate_root, "src/state", &snake, body, None)?;
    Ok(r)
}

pub(crate) fn is_primitive_integer_ty(ty: &str) -> bool {
    matches!(
        ty,
        "i8" | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_store_auto_id_emits_push_new_and_next_id() {
        let dir = tempfile::TempDir::new().unwrap();
        let cs = DslClientStore {
            name: "TodoStore".into(),
            item_type: "Todo".into(),
            initial: None,
            id_field: Some("id".into()),
            id_type: None,
            auto_id: Some(true),
        };
        let r = generate_client_store(dir.path(), &cs, &BTreeSet::new(), None).unwrap();
        let file = r
            .files_created
            .iter()
            .find(|p| p.ends_with("todo_store.rs"))
            .expect("store file must be in files_created");
        let body = std::fs::read_to_string(file).unwrap();
        assert!(
            body.contains("fn push_new(&mut self, item: Todo)"),
            "push_new must be emitted (note: no `pub` on trait items inside #[store(pub)] impl, and no `mut` in the param pattern), got:\n{body}"
        );
        assert!(
            body.contains("pub next_id: i64,"),
            "next_id field must be present, got:\n{body}"
        );
        assert!(
            body.contains("next_id: 1i64,"),
            "next_id init must use typed literal, got:\n{body}"
        );
        assert!(
            body.contains("#[derive(Store, Clone, Default)]"),
            "derive Store must be emitted, got:\n{body}"
        );
        assert!(
            body.contains("#[store(pub)]"),
            "#[store(pub)] impl attribute must be emitted (pub makes the extension trait importable from other modules), got:\n{body}"
        );
        assert!(
            body.contains("impl Store<TodoStore>"),
            "#[store] impl must target Store<TodoStore>, not the bare struct, got:\n{body}"
        );
        assert!(
            body.contains("pub fn provide_todo_store() -> Store<TodoStore>"),
            "typed provider signature must be emitted, got:\n{body}"
        );
        assert!(
            !body.contains("Signal<Vec<"),
            "old Signal<Vec<...>> wrapper shape must be gone, got:\n{body}"
        );
    }

    #[test]
    fn client_store_no_auto_id_emits_derive_store_and_no_signal_wrapper() {
        let dir = tempfile::TempDir::new().unwrap();
        let cs = DslClientStore {
            name: "DraftStore".into(),
            item_type: "String".into(),
            initial: None,
            id_field: None,
            id_type: None,
            auto_id: None,
        };
        let r = generate_client_store(dir.path(), &cs, &BTreeSet::new(), None).unwrap();
        let file = r
            .files_created
            .iter()
            .find(|p| p.ends_with("draft_store.rs"))
            .expect("store file must be in files_created");
        let body = std::fs::read_to_string(file).unwrap();
        assert!(body.contains("#[derive(Store, Clone, Default)]"));
        assert!(body.contains("#[store(pub)]"));
        assert!(body.contains("impl Store<DraftStore>"));
        assert!(body.contains("pub items: Vec<String>,"));
        assert!(body.contains("pub fn use_draft_store() -> Store<DraftStore>"));
        assert!(body.contains("pub fn provide_draft_store() -> Store<DraftStore>"));
        assert!(!body.contains("Signal<Vec<"));
        assert!(!body.contains("next_id"));
    }

    #[test]
    fn client_store_with_checkbox_field_emits_clear_helper() {
        // When a companion client_crud Screen sets `checkbox_field`, the
        // store's #[store(pub)] impl gains a `clear_{field}` helper plus the
        // matching read-side derived helpers (`remaining`, `any_{field}`) so
        // call sites can wire "Clear completed", remaining counts, and CTA
        // gating without reaching into items().read().iter() at every render.
        let dir = tempfile::TempDir::new().unwrap();
        let cs = DslClientStore {
            name: "TodoStore".into(),
            item_type: "Todo".into(),
            initial: None,
            id_field: Some("id".into()),
            id_type: Some("i64".into()),
            auto_id: Some(true),
        };
        let r = generate_client_store(dir.path(), &cs, &BTreeSet::new(), Some("done")).unwrap();
        let file = r
            .files_created
            .iter()
            .find(|p| p.ends_with("todo_store.rs"))
            .expect("store file must be in files_created");
        let body = std::fs::read_to_string(file).unwrap();
        assert!(
            body.contains("fn clear_done(&mut self)"),
            "expected `fn clear_done` helper, got:\n{body}"
        );
        assert!(
            body.contains("self.items().write().retain(|x| !x.done);"),
            "clear_done body must drop items where `done` is true, got:\n{body}"
        );
        assert!(
            body.contains("fn remaining(&self) -> usize"),
            "expected `fn remaining` read helper, got:\n{body}"
        );
        assert!(
            body.contains("filter(|x| !x.done).count()"),
            "remaining body must count items where `done` is false, got:\n{body}"
        );
        assert!(
            body.contains("fn any_done(&self) -> bool"),
            "expected `fn any_done` read helper, got:\n{body}"
        );
        assert!(
            body.contains("iter().any(|x| x.done)"),
            "any_done body must check at least one item with `done` set, got:\n{body}"
        );
    }

    #[test]
    fn client_store_without_checkbox_field_omits_clear_helper() {
        let dir = tempfile::TempDir::new().unwrap();
        let cs = DslClientStore {
            name: "TodoStore".into(),
            item_type: "Todo".into(),
            initial: None,
            id_field: Some("id".into()),
            id_type: Some("i64".into()),
            auto_id: Some(true),
        };
        let r = generate_client_store(dir.path(), &cs, &BTreeSet::new(), None).unwrap();
        let file = r
            .files_created
            .iter()
            .find(|p| p.ends_with("todo_store.rs"))
            .expect("store file must be in files_created");
        let body = std::fs::read_to_string(file).unwrap();
        assert!(
            !body.contains("fn clear_"),
            "no `clear_{{field}}` helper should be emitted when checkbox_field is absent, got:\n{body}"
        );
    }

    #[test]
    fn client_store_auto_id_requires_id_field() {
        let dir = tempfile::TempDir::new().unwrap();
        let cs = DslClientStore {
            name: "TodoStore".into(),
            item_type: "Todo".into(),
            initial: None,
            id_field: None,
            id_type: None,
            auto_id: Some(true),
        };
        let err = generate_client_store(dir.path(), &cs, &BTreeSet::new(), None).unwrap_err();
        assert!(err.contains("id_field"), "got: {err}");
    }

    #[test]
    fn client_store_auto_id_rejects_non_integer_id_type() {
        let dir = tempfile::TempDir::new().unwrap();
        let cs = DslClientStore {
            name: "TodoStore".into(),
            item_type: "Todo".into(),
            initial: None,
            id_field: Some("id".into()),
            id_type: Some("String".into()),
            auto_id: Some(true),
        };
        let err = generate_client_store(dir.path(), &cs, &BTreeSet::new(), None).unwrap_err();
        assert!(err.contains("primitive integer"), "got: {err}");
    }
}
