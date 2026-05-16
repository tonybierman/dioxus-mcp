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
) -> Result<ScaffoldResult, String> {
    let pascal = cs.name.to_pascal_case();
    let snake = cs.name.to_snake_case();
    let item_type = cs.item_type.trim().to_string();
    let id_field = cs.id_field.as_ref().map(|s| s.to_snake_case());
    let id_type = cs.id_type.clone().unwrap_or_else(|| "i64".into());
    let initial = cs.initial.clone().unwrap_or_else(|| "Vec::new()".into());
    let auto_id = cs.auto_id.unwrap_or(false);
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
