use std::path::Path;

use minijinja::Environment;

use crate::tools::scaffold::{ModUpsert, ScaffoldResult, upsert_mod_entry};

pub(super) fn render(name: &str, tpl: &str, ctx: minijinja::Value) -> Result<String, String> {
    let mut env = Environment::new();
    env.add_template(name, tpl).map_err(|e| e.to_string())?;
    env.get_template(name)
        .map_err(|e| e.to_string())?
        .render(ctx)
        .map_err(|e| e.to_string())
}

pub(super) fn write_component_file(
    crate_root: &Path,
    snake: &str,
    body: String,
) -> Result<ScaffoldResult, String> {
    write_module_file(crate_root, "src/components", snake, body)
}

pub(super) fn write_module_file(
    crate_root: &Path,
    subdir: &str,
    snake: &str,
    body: String,
) -> Result<ScaffoldResult, String> {
    // src/state/ entries declare server-only store modules; without the
    // matching cfg gate on the `pub mod`/`pub use` lines, the wasm (web-only)
    // build fails with E0432 because the file is `#![cfg(feature = "server")]`
    // and effectively absent. ClientStore lives in the same dir but is NOT
    // server-only; it uses `write_module_file_with_cfg(... None)` directly.
    let cfg_attr = if subdir == "src/state" {
        Some("#[cfg(feature = \"server\")]")
    } else {
        None
    };
    write_module_file_with_cfg(crate_root, subdir, snake, body, cfg_attr)
}

pub(super) fn write_module_file_with_cfg(
    crate_root: &Path,
    subdir: &str,
    snake: &str,
    body: String,
    cfg_attr: Option<&str>,
) -> Result<ScaffoldResult, String> {
    let dir = crate_root.join(subdir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let target = dir.join(format!("{snake}.rs"));
    if target.exists() {
        return Err(format!("{} already exists", target.display()));
    }
    std::fs::write(&target, body).map_err(|e| e.to_string())?;
    let mod_rs = dir.join("mod.rs");
    let upsert = upsert_mod_entry(&mod_rs, snake, cfg_attr)?;
    let (created, modified) = match upsert {
        ModUpsert::Created => (vec![target, mod_rs], vec![]),
        ModUpsert::Modified => (vec![target], vec![mod_rs]),
        ModUpsert::Unchanged => (vec![target], vec![]),
    };
    Ok(ScaffoldResult {
        files_created: created,
        files_modified: modified,
        ..Default::default()
    })
}
