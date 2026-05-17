use std::collections::BTreeSet;
use std::path::Path;

use heck::{ToPascalCase, ToSnakeCase};

use crate::tools::scaffold::{self, ScaffoldResult};

use super::plan::normalize_route_path;
use super::preflight::{preflight, preflight_disk_aware};
use super::resources::SynthServerFn;
use super::types::*;
use super::util::leaf_for;

/// Expand every `replace_route: true` on Screen / LoginScreen into an explicit
/// `RemoveRoute` entry on `doc.remove`. Looks at the on-disk Routable enum,
/// matches each route path against the existing variants (skipping same-variant
/// no-ops and variants the user already listed for removal), and appends one
/// entry per genuine collision. If there's no Routable file yet (a fresh `dx
/// new` project before bootstrap), `replace_route` is a silent no-op — there
/// is nothing on disk to replace.
pub(super) fn synthesize_replace_route_removes(doc: &mut DslDoc, crate_root: &Path) {
    let any_replace = doc.screens.iter().any(|s| s.replace_route)
        || doc.login_screens.iter().any(|ls| ls.replace_route);
    if !any_replace {
        return;
    }
    let Some(router_path) = scaffold::find_routable(crate_root) else {
        return;
    };
    let Ok(src) = std::fs::read_to_string(&router_path) else {
        return;
    };
    let existing = scaffold::existing_route_paths(&src);
    if existing.is_empty() {
        return;
    }
    let mut already_removed: BTreeSet<String> = doc
        .remove
        .iter()
        .filter_map(|r| match r {
            DslRemove::RemoveRoute { variant } => Some(variant.to_pascal_case()),
            _ => None,
        })
        .collect();
    let candidates: Vec<(String, String, bool)> = doc
        .screens
        .iter()
        .map(|s| (s.name.clone(), s.route.clone(), s.replace_route))
        .chain(
            doc.login_screens
                .iter()
                .map(|ls| (ls.name.clone(), ls.route.clone(), ls.replace_route)),
        )
        .collect();
    for (name, route, replace) in candidates {
        if !replace {
            continue;
        }
        let normalized = normalize_route_path(&route);
        let new_variant = name.to_pascal_case();
        for (existing_variant, existing_path) in &existing {
            if normalize_route_path(existing_path) != normalized {
                continue;
            }
            if existing_variant == &new_variant {
                continue;
            }
            if !already_removed.insert(existing_variant.clone()) {
                continue;
            }
            doc.remove.push(DslRemove::RemoveRoute {
                variant: existing_variant.clone(),
            });
        }
    }
}

/// Expand `prune_dx_new_starter: true` into explicit `remove:` entries for
/// the well-known boilerplate `dx new` ships: the `Hero` component file at
/// `src/components/hero.rs` and the `Home` variant in the project's
/// Routable enum. Each is only synthesized when the target exists on disk
/// (so the doc.remove list stays honest and dry-run plans match real runs).
/// Idempotent: duplicates already declared by the user are skipped.
pub(super) fn synthesize_dx_new_starter_removes(doc: &mut DslDoc, crate_root: &Path) {
    if !doc.prune_dx_new_starter {
        return;
    }
    let mut already_components: BTreeSet<String> = doc
        .remove
        .iter()
        .filter_map(|r| match r {
            DslRemove::RemoveComponent { component } => Some(component.to_pascal_case()),
            _ => None,
        })
        .collect();
    let mut already_routes: BTreeSet<String> = doc
        .remove
        .iter()
        .filter_map(|r| match r {
            DslRemove::RemoveRoute { variant } => Some(variant.to_pascal_case()),
            _ => None,
        })
        .collect();

    // Hero component file. `dx new` puts the demo widget under this exact
    // path; if the user already moved or removed it, the leaf check below
    // silently skips the synthesis.
    let hero_path = crate_root.join("src/components/hero.rs");
    if hero_path.exists() && already_components.insert("Hero".to_string()) {
        doc.remove.push(DslRemove::RemoveComponent {
            component: "Hero".into(),
        });
    }

    // Home variant. `dx new` maps `/` to a `Home` route variant that renders
    // the Hero. Detect it by parsing the on-disk Routable file.
    if let Some(router_path) = scaffold::find_routable(crate_root)
        && let Ok(src) = std::fs::read_to_string(&router_path)
    {
        let existing = scaffold::existing_route_paths(&src);
        let has_home = existing.iter().any(|(v, _)| v == "Home");
        if has_home && already_routes.insert("Home".to_string()) {
            doc.remove.push(DslRemove::RemoveRoute {
                variant: "Home".into(),
            });
        }
    }
}

/// Return the leaf-file paths a `remove:` block would delete. Routes don't
/// have leaf files — they're slotted out of the Routable enum source — so
/// `remove_route` contributes nothing here. Used by preflight to suppress the
/// "file already exists" check for slots that are about to be cleared.
pub(super) fn removed_leaf_paths(doc: &DslDoc, crate_root: &Path) -> BTreeSet<std::path::PathBuf> {
    let mut s = BTreeSet::new();
    for r in &doc.remove {
        match r {
            DslRemove::RemoveRoute { .. } => {}
            DslRemove::RemoveComponent { component } => {
                s.insert(leaf_for(crate_root, "src/components", component));
            }
            DslRemove::RemoveModel { model } => {
                s.insert(leaf_for(crate_root, "src/model", model));
            }
            DslRemove::RemoveServerFn { server_fn } => {
                s.insert(leaf_for(crate_root, "src/server", server_fn));
            }
        }
    }
    s
}

/// Augment plan_dsl's would_modify list with the files a `remove:` block would
/// touch (leaf files and their mod.rs entries), and the router for route
/// removals.
pub(super) fn plan_removes(plan: &mut ScaffoldResult, doc: &DslDoc, crate_root: &Path) {
    for r in &doc.remove {
        match r {
            DslRemove::RemoveRoute { .. } => {
                if let Some(router) = scaffold::find_routable(crate_root)
                    && !plan.would_modify.iter().any(|p| p == &router)
                {
                    plan.would_modify.push(router);
                }
            }
            DslRemove::RemoveComponent { component } => {
                let leaf = leaf_for(crate_root, "src/components", component);
                if leaf.exists() && !plan.would_modify.iter().any(|p| p == &leaf) {
                    plan.would_modify.push(leaf);
                }
                let mod_rs = crate_root.join("src/components/mod.rs");
                if mod_rs.exists() && !plan.would_modify.iter().any(|p| p == &mod_rs) {
                    plan.would_modify.push(mod_rs);
                }
            }
            DslRemove::RemoveModel { model } => {
                let leaf = leaf_for(crate_root, "src/model", model);
                if leaf.exists() && !plan.would_modify.iter().any(|p| p == &leaf) {
                    plan.would_modify.push(leaf);
                }
                let mod_rs = crate_root.join("src/model/mod.rs");
                if mod_rs.exists() && !plan.would_modify.iter().any(|p| p == &mod_rs) {
                    plan.would_modify.push(mod_rs);
                }
            }
            DslRemove::RemoveServerFn { server_fn } => {
                let leaf = leaf_for(crate_root, "src/server", server_fn);
                if leaf.exists() && !plan.would_modify.iter().any(|p| p == &leaf) {
                    plan.would_modify.push(leaf);
                }
                let mod_rs = crate_root.join("src/server/mod.rs");
                if mod_rs.exists() && !plan.would_modify.iter().any(|p| p == &mod_rs) {
                    plan.would_modify.push(mod_rs);
                }
            }
        }
    }
}

/// Wrap preflight() with an early hook: temporarily mask any files the
/// remove block will delete so preflight's existence check doesn't flag them
/// as collisions. This is intentionally cheap — preflight performs at most a
/// handful of filesystem checks — and avoids threading a new parameter through
/// every internal call.
pub(super) fn preflight_with_removes(
    doc: &DslDoc,
    synth_server_fns: &[SynthServerFn],
    crate_root: &Path,
    if_missing: bool,
    to_be_removed: &BTreeSet<std::path::PathBuf>,
    // Dry-run callers pass `disk_aware: true` so cross-ref checks
    // (Screen.template.store, List.endpoint, Store.resource, …) are satisfied
    // by the corresponding on-disk leaf file as well as by an in-doc
    // declaration. Lets agents preview a Screen against an already-scaffolded
    // primitive without redeclaring it in the YAML.
    disk_aware: bool,
) -> Result<(), String> {
    let pf = |if_m: bool| {
        if disk_aware {
            preflight_disk_aware(doc, synth_server_fns, crate_root, if_m)
        } else {
            preflight(doc, synth_server_fns, crate_root, if_m)
        }
    };
    if to_be_removed.is_empty() {
        return pf(if_missing);
    }
    // Bypass strategy: clone the doc and filter out *create* primitives whose
    // leaf paths overlap a remove target. That way the existence check inside
    // preflight is exclusively about non-removed files. Doc-internal
    // duplicate-name checks still run against the original doc, so we keep
    // those by invoking preflight twice — once on a doc that has the
    // would-be-removed creates filtered out (to skip the FS check), and once
    // on the original doc with if_missing forced so the FS check itself
    // becomes a no-op for the masked paths.
    //
    // Simpler: preflight already has an `if_missing` knob that suppresses
    // exactly the FS check we need to skip. If any remove targets overlap
    // create targets, force if_missing on for the second-half FS check only.
    // The doc-internal duplicate-name check runs first and is unaffected.
    let any_overlap = {
        let mut any = false;
        for c in &doc.components {
            if to_be_removed.contains(&leaf_for(crate_root, "src/components", &c.name)) {
                any = true;
                break;
            }
        }
        if !any {
            for m in &doc.models {
                if to_be_removed.contains(&leaf_for(crate_root, "src/model", &m.name)) {
                    any = true;
                    break;
                }
            }
        }
        any
    };
    pf(if_missing || any_overlap)
}

/// Execute every `remove:` entry. Each kind is idempotent — naming a target
/// that's already gone is a silent no-op. Failures (e.g. router file can't be
/// parsed) abort the run before any create/modify step.
pub(super) fn apply_removes(
    doc: &DslDoc,
    crate_root: &Path,
    result: &mut ScaffoldResult,
) -> Result<(), String> {
    for r in &doc.remove {
        match r {
            DslRemove::RemoveRoute { variant } => {
                remove_route_variant(crate_root, variant, result)?;
            }
            DslRemove::RemoveComponent { component } => {
                remove_module_file(crate_root, "src/components", component, result)?;
            }
            DslRemove::RemoveModel { model } => {
                remove_module_file(crate_root, "src/model", model, result)?;
            }
            DslRemove::RemoveServerFn { server_fn } => {
                remove_module_file(crate_root, "src/server", server_fn, result)?;
            }
        }
    }
    Ok(())
}

/// Delete `src/{subdir}/{snake}.rs` if present and strip the matching
/// `pub mod` and `pub use` lines from the directory's mod.rs. Both operations
/// are idempotent. The leaf path lands in `files_modified` (rather than a new
/// `files_removed` field — keeping the response shape stable).
pub(super) fn remove_module_file(
    crate_root: &Path,
    subdir: &str,
    name: &str,
    result: &mut ScaffoldResult,
) -> Result<(), String> {
    let snake = name.to_snake_case();
    let leaf = crate_root.join(subdir).join(format!("{snake}.rs"));
    let mod_rs = crate_root.join(subdir).join("mod.rs");
    let mut touched_any = false;

    if leaf.exists() {
        std::fs::remove_file(&leaf)
            .map_err(|e| format!("remove: failed to delete {}: {e}", leaf.display()))?;
        if !result.files_modified.iter().any(|p| p == &leaf) {
            result.files_modified.push(leaf);
        }
        touched_any = true;
    }
    if mod_rs.exists() {
        let src = std::fs::read_to_string(&mod_rs).map_err(|e| e.to_string())?;
        let new_src = strip_mod_entry(&src, &snake);
        if new_src != src {
            std::fs::write(&mod_rs, &new_src).map_err(|e| e.to_string())?;
            if !result.files_modified.iter().any(|p| p == &mod_rs) {
                result.files_modified.push(mod_rs);
            }
            touched_any = true;
        }
    }
    let _ = touched_any;
    Ok(())
}

/// Drop `pub mod {snake};` / `pub use {snake}::*;` / their `#[cfg(...)]`
/// attribute lines (and any adjacent `#[allow(...)]` shield) from a mod.rs.
/// Leaves the rest of the file untouched. Idempotent: a snake that's already
/// absent passes through unchanged.
pub(super) fn strip_mod_entry(src: &str, snake: &str) -> String {
    let lines: Vec<&str> = src.lines().collect();
    let mut keep: Vec<bool> = vec![true; lines.len()];
    let pub_mod = format!("pub mod {snake};");
    let bare_mod = format!("mod {snake};");
    let pub_use = format!("pub use {snake}::*;");
    let bare_use = format!("use {snake}::*;");
    for (i, raw) in lines.iter().enumerate() {
        let t = raw.trim();
        if t == pub_mod || t == bare_mod || t == pub_use || t == bare_use {
            keep[i] = false;
            // Walk back over an immediately-preceding `#[cfg(...)]` /
            // `#[allow(...)]` attribute on its own line.
            let mut j = i;
            while j > 0 {
                let prev = lines[j - 1].trim();
                if prev.starts_with("#[")
                    && prev.ends_with("]")
                    && (prev.contains("cfg(") || prev.contains("allow("))
                {
                    keep[j - 1] = false;
                    j -= 1;
                } else {
                    break;
                }
            }
        }
    }
    let mut out = String::with_capacity(src.len());
    for (i, raw) in lines.iter().enumerate() {
        if keep[i] {
            out.push_str(raw);
            out.push('\n');
        }
    }
    // Preserve original trailing-newline shape.
    if !src.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Remove a variant (and its `#[route(...)]` attribute) from the Routable
/// enum. Idempotent: if the variant isn't present the function is a no-op.
/// Errors only when the Routable file exists but can't be parsed.
pub(super) fn remove_route_variant(
    crate_root: &Path,
    variant: &str,
    result: &mut ScaffoldResult,
) -> Result<(), String> {
    let pascal = variant.to_pascal_case();
    let Some(router_path) = scaffold::find_routable(crate_root) else {
        return Ok(());
    };
    let src = std::fs::read_to_string(&router_path).map_err(|e| e.to_string())?;
    let parsed = syn::parse_file(&src)
        .map_err(|e| format!("remove: parse {}: {e}", router_path.display()))?;
    let routable = parsed.items.iter().find_map(|it| match it {
        syn::Item::Enum(e) if e.attrs.iter().any(|a| scaffold::has_derive(a, "Routable")) => {
            Some(e)
        }
        _ => None,
    });
    let Some(routable) = routable else {
        return Ok(());
    };
    let target = routable.variants.iter().find(|v| v.ident == pascal);
    let Some(target) = target else {
        return Ok(());
    };

    use syn::spanned::Spanned;
    let var_span = Spanned::span(target);
    let start = var_span.byte_range().start;
    let end = var_span.byte_range().end;
    let mut cut_start = start;
    for attr in &target.attrs {
        let s = Spanned::span(attr).byte_range().start;
        if s < cut_start {
            cut_start = s;
        }
    }
    let bytes = src.as_bytes();
    while cut_start > 0 {
        let prev = bytes[cut_start - 1];
        if prev == b' ' || prev == b'\t' {
            cut_start -= 1;
        } else {
            break;
        }
    }
    if cut_start > 0 && bytes[cut_start - 1] == b'\n' {
        cut_start -= 1;
    }
    let mut cut_end = end;
    while cut_end < bytes.len() && (bytes[cut_end] == b' ' || bytes[cut_end] == b'\t') {
        cut_end += 1;
    }
    if cut_end < bytes.len() && bytes[cut_end] == b',' {
        cut_end += 1;
    }
    let mut new_src = String::with_capacity(src.len());
    new_src.push_str(&src[..cut_start]);
    new_src.push_str(&src[cut_end..]);
    std::fs::write(&router_path, &new_src).map_err(|e| e.to_string())?;
    if !result.files_modified.iter().any(|p| p == &router_path) {
        result.files_modified.push(router_path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::modify::remove_struct_fields;
    use super::*;

    #[test]
    fn remove_module_file_deletes_leaf_and_mod_entry() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/components")).unwrap();
        std::fs::write(root.join("src/components/hero.rs"), "// demo\n").unwrap();
        std::fs::write(
            root.join("src/components/mod.rs"),
            "pub mod hero;\npub use hero::*;\npub mod other;\npub use other::*;\n",
        )
        .unwrap();
        let mut result = ScaffoldResult::default();
        remove_module_file(root, "src/components", "Hero", &mut result).unwrap();
        assert!(
            !root.join("src/components/hero.rs").exists(),
            "leaf must be gone"
        );
        let mod_rs = std::fs::read_to_string(root.join("src/components/mod.rs")).unwrap();
        assert!(
            !mod_rs.contains("hero"),
            "mod.rs still references hero:\n{mod_rs}"
        );
        assert!(
            mod_rs.contains("other"),
            "unrelated entry must survive:\n{mod_rs}"
        );

        // Second run: no-op.
        let mut result2 = ScaffoldResult::default();
        remove_module_file(root, "src/components", "Hero", &mut result2).unwrap();
        assert!(
            result2.files_modified.is_empty(),
            "absent target must be no-op"
        );
    }

    #[test]
    fn remove_route_variant_drops_variant_and_its_route_attr() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/router.rs"),
            r#"use dioxus::prelude::*;

#[derive(Routable, Clone, PartialEq)]
pub enum Route {
    #[route("/")]
    Home {},
    #[route("/about")]
    About {},
}
"#,
        )
        .unwrap();
        let mut result = ScaffoldResult::default();
        remove_route_variant(root, "Home", &mut result).unwrap();
        let body = std::fs::read_to_string(root.join("src/router.rs")).unwrap();
        assert!(!body.contains("Home"), "Home variant survived:\n{body}");
        assert!(
            !body.contains("#[route(\"/\")]"),
            "route attr survived:\n{body}"
        );
        assert!(body.contains("About"), "unrelated variant must remain");

        // Second run: variant already gone → no-op.
        let mut result2 = ScaffoldResult::default();
        remove_route_variant(root, "Home", &mut result2).unwrap();
        assert!(
            result2.files_modified.is_empty(),
            "absent variant must be no-op"
        );
    }

    #[test]
    fn remove_model_field_drops_named_field_and_is_idempotent() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("widget.rs");
        std::fs::write(
            &path,
            r#"pub struct Widget {
    pub id: i64,
    pub name: String,
    pub legacy_code: String,
}
"#,
        )
        .unwrap();
        let mut result = ScaffoldResult::default();
        remove_struct_fields(
            &path,
            "Widget",
            &["legacy_code".to_string()],
            false,
            &mut result,
            "model",
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            !body.contains("legacy_code"),
            "field must be gone, got:\n{body}"
        );
        assert!(body.contains("pub id: i64,"), "kept fields untouched");
        assert!(body.contains("pub name: String,"));
        assert!(result.files_modified.iter().any(|p| p == &path));

        // Second run: no-op, no extra files_modified entry.
        let mut result2 = ScaffoldResult::default();
        remove_struct_fields(
            &path,
            "Widget",
            &["legacy_code".to_string()],
            false,
            &mut result2,
            "model",
        )
        .unwrap();
        assert!(
            result2.files_modified.is_empty(),
            "second run should be a no-op"
        );
    }
}
