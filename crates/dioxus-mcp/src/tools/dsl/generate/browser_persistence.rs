use std::collections::BTreeSet;
use std::path::Path;

use heck::{ToPascalCase, ToShoutySnakeCase, ToSnakeCase};
use minijinja::context;

use crate::tools::scaffold::ScaffoldResult;

use super::super::render::*;
use super::super::templates::*;
use super::super::types::*;

/// Emit `src/storage/{snake}.rs` for a single browser-persistence entry.
///
/// `model_names` is the set of model snake_case names declared in the same
/// DSL doc — when the entry's `value_type` resolves to one of them, the file
/// auto-emits `use crate::model::{Pascal};` so the caller doesn't have to.
pub(crate) fn generate_browser_persistence(
    crate_root: &Path,
    bp: &DslBrowserPersistence,
    model_names: &BTreeSet<String>,
) -> Result<ScaffoldResult, String> {
    let backend = bp.backend.trim();
    if !matches!(backend, "local_storage" | "session_storage" | "cookie") {
        return Err(format!(
            "browser_persistence {:?}: unknown backend {:?}; valid: local_storage, session_storage, cookie",
            bp.name, bp.backend
        ));
    }

    let snake = bp.name.to_snake_case();
    let pascal = bp.name.to_pascal_case();
    let upper = snake.to_shouty_snake_case();
    let is_string = bp.value_type.trim() == "String";
    let cookie_attributes = bp
        .cookie_attributes
        .clone()
        .unwrap_or_else(|| "path=/; SameSite=Lax".into());

    // Auto-import the value type when it matches a Model declared in the doc.
    // We only handle the bare-name case; `Vec<Model>` / `Option<Model>` would
    // need broader cross-import resolution and the caller can pre-declare
    // those with a `use` themselves.
    let value_type_import = match &bp.value_type {
        v if model_names.contains(&v.to_snake_case())
            && v.chars().all(|c| c.is_alphanumeric() || c == '_') =>
        {
            Some(format!("use crate::model::{};", v.to_pascal_case()))
        }
        _ => None,
    };

    let body = render(
        "browser_persistence",
        BROWSER_PERSISTENCE_TPL,
        context! {
            snake => snake.clone(),
            pascal => pascal,
            upper => upper,
            backend => backend.to_string(),
            key => bp.key.clone(),
            value_type => bp.value_type.clone(),
            is_string => is_string,
            cookie_attributes => cookie_attributes,
            value_type_import => value_type_import,
        },
    )?;

    write_module_file(crate_root, "src/storage", &snake, body)
}
