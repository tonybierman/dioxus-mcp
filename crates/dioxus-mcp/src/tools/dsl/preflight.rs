use std::collections::BTreeSet;
use std::path::Path;

use heck::{ToPascalCase, ToSnakeCase};

use crate::tools::scaffold;

use super::plan::normalize_route_path;
use super::resources::SynthServerFn;
use super::types::*;

pub(super) fn preflight(
    doc: &DslDoc,
    synth_server_fns: &[SynthServerFn],
    crate_root: &Path,
    if_missing: bool,
) -> Result<(), String> {
    preflight_inner(doc, synth_server_fns, crate_root, if_missing, false)
}

/// Same as `preflight`, but with cross-reference checks relaxed against
/// the live filesystem — a `store:` / `endpoint:` / `socket:` / etc.
/// reference is satisfied if the corresponding leaf file already exists on
/// disk, even when it isn't redeclared in the doc. Used by execute_code in
/// `dry_run` mode so callers can preview a Screen against an already-
/// scaffolded primitive without copying its definition into the YAML.
pub(super) fn preflight_disk_aware(
    doc: &DslDoc,
    synth_server_fns: &[SynthServerFn],
    crate_root: &Path,
    if_missing: bool,
) -> Result<(), String> {
    preflight_inner(doc, synth_server_fns, crate_root, if_missing, true)
}

/// True when the leaf file `src/{subdir}/{snake}.rs` exists under
/// `crate_root`. Used by `preflight_inner` to relax cross-ref checks when
/// the referenced primitive lives on disk rather than in the doc.
fn leaf_exists(crate_root: &Path, subdir: &str, snake: &str) -> bool {
    crate_root.join(subdir).join(format!("{snake}.rs")).exists()
}

/// True when a cross-ref is satisfied: either the referenced primitive is
/// declared in this doc, or (disk_aware mode) the corresponding leaf file
/// already exists on disk. Folds the seven near-identical `!in_doc && !(disk
/// && leaf)` checks below into one readable predicate.
fn xref_satisfied(
    in_doc: &BTreeSet<String>,
    snake: &str,
    crate_root: &Path,
    subdir: &str,
    disk_aware: bool,
) -> bool {
    in_doc.contains(snake) || (disk_aware && leaf_exists(crate_root, subdir, snake))
}

fn preflight_inner(
    doc: &DslDoc,
    synth_server_fns: &[SynthServerFn],
    crate_root: &Path,
    if_missing: bool,
    disk_aware: bool,
) -> Result<(), String> {
    // 1. Collect every snake_case name across every primitive and reject dups
    //    that would land in the same target directory.
    let mut comp_names: BTreeSet<String> = BTreeSet::new();
    let mut sig_names: BTreeSet<String> = BTreeSet::new();
    let mut sock_names: BTreeSet<String> = BTreeSet::new();
    let mut srv_names: BTreeSet<String> = BTreeSet::new();
    let mut sess_names: BTreeSet<String> = BTreeSet::new();
    let mut model_names: BTreeSet<String> = BTreeSet::new();
    let mut store_names: BTreeSet<String> = BTreeSet::new();

    let mut comp_dup = |name: &str| -> Result<(), String> {
        let s = name.to_snake_case();
        if !comp_names.insert(s.clone()) {
            return Err(format!("duplicate component-target name: {s}"));
        }
        Ok(())
    };

    for c in &doc.components {
        comp_dup(&c.name)?;
    }
    for f in &doc.forms {
        comp_dup(&f.name)?;
    }
    for l in &doc.lists {
        comp_dup(&l.name)?;
    }
    for t in &doc.tables {
        comp_dup(&t.name)?;
    }
    for f in &doc.feeds {
        comp_dup(&f.name)?;
    }
    for ls in &doc.login_screens {
        comp_dup(&ls.name)?;
    }
    for pr in &doc.protected_routes {
        comp_dup(&pr.name)?;
    }
    for sc in &doc.screens {
        comp_dup(&sc.name)?;
    }

    for s in &doc.signals {
        if !sig_names.insert(s.name.to_snake_case()) {
            return Err(format!("duplicate signal name: {}", s.name));
        }
    }
    for s in &doc.sockets {
        if !sock_names.insert(s.name.to_snake_case()) {
            return Err(format!("duplicate socket name: {}", s.name));
        }
    }
    for s in &doc.server_fns {
        if !srv_names.insert(s.name.to_snake_case()) {
            return Err(format!("duplicate server_fn name: {}", s.name));
        }
    }
    for s in synth_server_fns {
        if !srv_names.insert(s.name.to_snake_case()) {
            return Err(format!(
                "resources: expansion produced server_fn {:?} which collides with an explicit `server_fns:` entry — rename or remove the conflict",
                s.name
            ));
        }
    }
    for s in &doc.stores {
        if !store_names.insert(s.name.to_snake_case()) {
            return Err(format!("duplicate store name: {}", s.name));
        }
    }
    let mut client_store_names: BTreeSet<String> = BTreeSet::new();
    for cs in &doc.client_stores {
        let snake = cs.name.to_snake_case();
        if !client_store_names.insert(snake.clone()) {
            return Err(format!("duplicate client_store name: {}", cs.name));
        }
        if store_names.contains(&snake) {
            return Err(format!(
                "client_store {:?} collides with store {:?} — both write to src/state/{snake}.rs; rename one",
                cs.name, cs.name
            ));
        }
    }
    // ViewState shares src/state/{snake}.rs with Store + ClientStore.
    // Duplicate names across any of them silently overwrite, so reject early.
    let mut view_state_names: BTreeSet<String> = BTreeSet::new();
    for vs in &doc.view_states {
        let snake = vs.name.to_snake_case();
        if !view_state_names.insert(snake.clone()) {
            return Err(format!("duplicate view_state name: {}", vs.name));
        }
        if store_names.contains(&snake) || client_store_names.contains(&snake) {
            return Err(format!(
                "view_state {:?} collides with an existing store / client_store of the same name — all three write to src/state/{snake}.rs; rename one",
                vs.name
            ));
        }
    }
    for s in &doc.session_states {
        if !sess_names.insert(s.name.to_snake_case()) {
            return Err(format!("duplicate session_state name: {}", s.name));
        }
    }
    for m in &doc.models {
        let snake = m.name.to_snake_case();
        if !model_names.insert(snake.clone()) {
            return Err(format!("duplicate model name: {}", m.name));
        }
        let mut seen_field: BTreeSet<String> = BTreeSet::new();
        for f in &m.fields {
            let fs = f.name.to_snake_case();
            if !seen_field.insert(fs) {
                return Err(format!(
                    "model {:?} declares duplicate field {:?}",
                    m.name, f.name
                ));
            }
        }
    }

    // 2. Verify cross-references exist within the doc. When `disk_aware`
    //    (dry_run mode), a reference is also satisfied if its leaf file
    //    already exists on disk — callers can preview a Screen without
    //    redeclaring on-disk primitives in the YAML.
    for f in &doc.feeds {
        let snake = f.socket.to_snake_case();
        if !xref_satisfied(&sock_names, &snake, crate_root, "src/sockets", disk_aware) {
            return Err(format!(
                "feed {:?} references unknown socket {:?}",
                f.name, f.socket
            ));
        }
    }
    for l in &doc.lists {
        let snake = l.endpoint.to_snake_case();
        if !xref_satisfied(&srv_names, &snake, crate_root, "src/server", disk_aware) {
            return Err(format!(
                "list {:?} references unknown server_fn {:?}; declare it under server_fns",
                l.name, l.endpoint
            ));
        }
    }
    for t in &doc.tables {
        let snake = t.endpoint.to_snake_case();
        if !xref_satisfied(&srv_names, &snake, crate_root, "src/server", disk_aware) {
            return Err(format!(
                "table {:?} references unknown server_fn {:?}; declare it under server_fns",
                t.name, t.endpoint
            ));
        }
    }
    let list_names: BTreeSet<String> = doc.lists.iter().map(|l| l.name.to_snake_case()).collect();
    for f in &doc.forms {
        if let Some(target) = &f.feeds_into {
            let snake = target.to_snake_case();
            if !xref_satisfied(
                &list_names,
                &snake,
                crate_root,
                "src/components",
                disk_aware,
            ) {
                return Err(format!(
                    "form {:?} feeds_into unknown list {:?}; declare it under lists",
                    f.name, target
                ));
            }
        }
    }
    for pr in &doc.protected_routes {
        if let Some(req) = &pr.requires {
            let snake = req.to_snake_case();
            if !xref_satisfied(&sess_names, &snake, crate_root, "src/auth", disk_aware) {
                return Err(format!(
                    "protected_route {:?} requires unknown session_state {:?}; declare it under session_states",
                    pr.name, req
                ));
            }
        }
    }
    for s in &doc.stores {
        let snake = s.resource.to_snake_case();
        if !xref_satisfied(&model_names, &snake, crate_root, "src/model", disk_aware) {
            return Err(format!(
                "store {:?} references unknown model {:?}; declare it under models",
                s.name, s.resource
            ));
        }
    }
    // Route-path collisions within the doc: two screens / login_screens
    // pointing at the same path string. The route-insertion step would catch
    // this when it hits the second variant, but only after the first has
    // already been written to disk. Detect it up front so the call is atomic.
    {
        let mut path_owner: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for ls in &doc.login_screens {
            let normalized = normalize_route_path(&ls.route);
            if let Some(prev) = path_owner.insert(normalized.clone(), ls.name.clone()) {
                return Err(format!(
                    "route path {:?} is declared twice in this doc: by {prev:?} and by {:?} — rename one or change its `route:`",
                    ls.route, ls.name
                ));
            }
        }
        for sc in &doc.screens {
            let normalized = normalize_route_path(&sc.route);
            if let Some(prev) = path_owner.insert(normalized.clone(), sc.name.clone()) {
                return Err(format!(
                    "route path {:?} is declared twice in this doc: by {prev:?} and by {:?} — rename one or change its `route:`",
                    sc.route, sc.name
                ));
            }
        }
    }

    // Route-path collisions against the EXISTING Routable enum on disk. The
    // per-screen insert in `scaffold::create_route` catches this too, but only
    // after some primitives have already been written — pre-flight surfaces it
    // up front with the colliding variant name so the caller knows whether to
    // rename their new variant or `remove_route:` the existing one.
    //
    // Variants listed under the doc's `remove:` block are excluded — the
    // route_collision check has to assume those will be gone by the time the
    // create step runs (removes execute before creates in execute_code).
    {
        let router_file = scaffold::find_routable(crate_root);
        if let Some(path) = router_file.as_ref()
            && let Ok(src) = std::fs::read_to_string(path)
        {
            let existing: Vec<(String, String)> = scaffold::existing_route_paths(&src);
            if !existing.is_empty() {
                let rel = path.strip_prefix(crate_root).unwrap_or(path).display();
                let removed_variants: BTreeSet<String> = doc
                    .remove
                    .iter()
                    .filter_map(|r| match r {
                        DslRemove::RemoveRoute { variant } => Some(variant.to_pascal_case()),
                        _ => None,
                    })
                    .collect();
                let new_routes: Vec<(&str, &str, &str, bool)> = doc
                    .screens
                    .iter()
                    .map(|s| ("screen", s.name.as_str(), s.route.as_str(), s.replace_route))
                    .chain(doc.login_screens.iter().map(|ls| {
                        (
                            "login_screen",
                            ls.name.as_str(),
                            ls.route.as_str(),
                            ls.replace_route,
                        )
                    }))
                    .collect();
                for (kind, name, route, replace) in &new_routes {
                    let normalized = normalize_route_path(route);
                    let new_variant = name.to_pascal_case();
                    for (existing_variant, existing_path) in &existing {
                        if normalize_route_path(existing_path) != normalized {
                            continue;
                        }
                        if existing_variant == &new_variant {
                            // Same variant, same path → idempotent re-run. The
                            // per-screen create_route call returns AlreadyMatches
                            // for this case; nothing to flag here.
                            continue;
                        }
                        if removed_variants.contains(existing_variant) {
                            // The doc explicitly removes this variant first;
                            // the create step will see a clean slot.
                            continue;
                        }
                        if *replace {
                            // `replace_route: true` opts into the
                            // "drop the existing variant first" path; the
                            // actual remove is synthesized in execute_code
                            // before the create step.
                            continue;
                        }
                        return Err(format!(
                            "route path {route:?} is already mapped by variant {existing_variant:?} in `{rel}` — {kind} {name:?} would collide. \
                             Options: (a) change the {kind}'s `route:` to a fresh path, (b) rename the {kind} to {existing_variant:?} to take over the existing variant, \
                             (c) add `replace_route: true` on the {kind} to drop the on-disk variant automatically, \
                             or (d) add `remove: [{{ kind: remove_route, variant: {existing_variant:?} }}]` to drop the on-disk variant before re-running."
                        ));
                    }
                }
            }
        }
    }

    for sc in &doc.screens {
        if let Some(tpl) = &sc.template
            && tpl.kind == "client_crud"
        {
            let store = tpl.store.as_deref().ok_or_else(|| {
                format!(
                    "screen {:?} kind=client_crud requires `store:` (a client_stores name)",
                    sc.name
                )
            })?;
            let store_snake = store.to_snake_case();
            if !xref_satisfied(
                &client_store_names,
                &store_snake,
                crate_root,
                "src/state",
                disk_aware,
            ) {
                return Err(format!(
                    "screen {:?} references unknown client_store {:?}; declare it under client_stores",
                    sc.name, store
                ));
            }
            if tpl.label_field.is_none() {
                return Err(format!(
                    "screen {:?} kind=client_crud requires `label_field`",
                    sc.name
                ));
            }
        }
    }

    // 3. Validate `modify:` entries — non-empty, no duplicate field/arg/prop
    //    names within a single entry. Cross-doc references aren't required:
    //    the target item is allowed to exist only on disk.
    for (i, m) in doc.modify.iter().enumerate() {
        let (kind, names): (&str, Vec<String>) = match m {
            DslModify::AddModelField { fields, .. } => {
                if fields.is_empty() {
                    return Err(format!(
                        "modify[{i}] add_model_field: `fields` is empty — nothing to add"
                    ));
                }
                (
                    "add_model_field",
                    fields.iter().map(|f| f.name.to_snake_case()).collect(),
                )
            }
            DslModify::AddComponentProp { props, .. } => {
                if props.is_empty() {
                    return Err(format!(
                        "modify[{i}] add_component_prop: `props` is empty — nothing to add"
                    ));
                }
                (
                    "add_component_prop",
                    props.iter().map(|p| p.name.to_snake_case()).collect(),
                )
            }
            DslModify::AddServerFnArg { args, .. } => {
                if args.is_empty() {
                    return Err(format!(
                        "modify[{i}] add_server_fn_arg: `args` is empty — nothing to add"
                    ));
                }
                (
                    "add_server_fn_arg",
                    args.iter().map(|a| a.name.to_snake_case()).collect(),
                )
            }
            DslModify::RemoveModelField { fields, .. } => {
                if fields.is_empty() {
                    return Err(format!(
                        "modify[{i}] remove_model_field: `fields` is empty — nothing to remove"
                    ));
                }
                (
                    "remove_model_field",
                    fields.iter().map(|n| n.to_snake_case()).collect(),
                )
            }
            DslModify::RemoveComponentProp { props, .. } => {
                if props.is_empty() {
                    return Err(format!(
                        "modify[{i}] remove_component_prop: `props` is empty — nothing to remove"
                    ));
                }
                (
                    "remove_component_prop",
                    props.iter().map(|n| n.to_snake_case()).collect(),
                )
            }
        };
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for n in &names {
            if !seen.insert(n.clone()) {
                return Err(format!(
                    "modify[{i}] {kind}: duplicate name {n:?} in the entry"
                ));
            }
        }
    }

    // 4. Pre-check files that would collide with what's already on disk for
    //    each component-target name. (server_fn / signal / socket / state
    //    dirs may not exist yet; existence isn't an error there.) Suppressed
    //    when `if_missing` is set — those collisions become skip entries
    //    instead.
    if !if_missing {
        let comp_dir = crate_root.join("src/components");
        for n in &comp_names {
            if comp_dir.join(format!("{n}.rs")).exists() {
                return Err(format!(
                    "src/components/{n}.rs already exists; refusing to overwrite. \
                     Pass `if_missing: true` to skip existing primitives instead of erroring."
                ));
            }
        }
        let state_dir = crate_root.join("src/state");
        for n in &store_names {
            if state_dir.join(format!("{n}.rs")).exists() {
                return Err(format!(
                    "src/state/{n}.rs already exists; refusing to overwrite. \
                     Pass `if_missing: true` to skip existing primitives instead of erroring."
                ));
            }
        }
        for n in &client_store_names {
            if state_dir.join(format!("{n}.rs")).exists() {
                return Err(format!(
                    "src/state/{n}.rs already exists; refusing to overwrite. \
                     Pass `if_missing: true` to skip existing primitives instead of erroring."
                ));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::remove::synthesize_replace_route_removes;
    use super::*;

    #[test]
    fn preflight_rejects_two_screens_with_same_route() {
        let doc: DslDoc = serde_yml::from_str(
            r#"version: "1"
screens:
  - name: HomeScreen
    route: /
  - name: LandingScreen
    route: /
"#,
        )
        .unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let err = preflight(&doc, &[], dir.path(), false).unwrap_err();
        assert!(
            err.contains("route path") && err.contains("declared twice"),
            "expected route-path collision error, got: {err}"
        );
    }

    #[test]
    fn preflight_rejects_screen_and_login_with_same_route() {
        let doc: DslDoc = serde_yml::from_str(
            r#"version: "1"
login_screens:
  - name: Login
    route: /entry
    redirect_on_success: /
screens:
  - name: EntryScreen
    route: /entry
"#,
        )
        .unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let err = preflight(&doc, &[], dir.path(), false).unwrap_err();
        assert!(
            err.contains("/entry"),
            "expected the conflicting path in the error, got: {err}"
        );
    }

    #[test]
    fn preflight_rejects_route_already_in_on_disk_routable_enum() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/router.rs"),
            r#"use dioxus::prelude::*;

#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[route("/")]
    Home {},
    #[route("/users")]
    User {},
}
"#,
        )
        .unwrap();
        let doc: DslDoc = serde_yml::from_str(
            r#"version: "1"
screens:
  - name: Customers
    route: /users
"#,
        )
        .unwrap();
        let err = preflight(&doc, &[], dir.path(), false).unwrap_err();
        assert!(
            err.contains("/users") && err.contains("User"),
            "error should name the colliding path and the existing variant, got: {err}"
        );
        assert!(
            err.contains("remove_route"),
            "error should suggest the remove_route option, got: {err}"
        );
    }

    #[test]
    fn preflight_route_collision_check_skips_doc_remove_targets() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/router.rs"),
            r#"use dioxus::prelude::*;

#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[route("/")]
    Home {},
    #[route("/users")]
    User {},
}
"#,
        )
        .unwrap();
        let doc: DslDoc = serde_yml::from_str(
            r#"version: "1"
remove:
  - kind: remove_route
    variant: User
screens:
  - name: Customers
    route: /users
"#,
        )
        .unwrap();
        preflight(&doc, &[], dir.path(), false)
            .expect("remove of conflicting variant should be respected");
    }

    #[test]
    fn preflight_route_collision_check_allows_idempotent_rerun() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/router.rs"),
            r#"use dioxus::prelude::*;

#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[route("/users")]
    User {},
}
"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src/components")).unwrap();
        std::fs::write(dir.path().join("src/components/user.rs"), "// existing\n").unwrap();
        let doc: DslDoc = serde_yml::from_str(
            r#"version: "1"
screens:
  - name: User
    route: /users
"#,
        )
        .unwrap();
        preflight(&doc, &[], dir.path(), true).expect("idempotent re-run should pass pre-flight");
    }

    #[test]
    fn replace_route_synthesizes_remove_for_colliding_variant() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/router.rs"),
            r#"use dioxus::prelude::*;

#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[route("/users")]
    User {},
}
"#,
        )
        .unwrap();
        let mut doc: DslDoc = serde_yml::from_str(
            r#"version: "1"
screens:
  - name: Customers
    route: /users
    replace_route: true
"#,
        )
        .unwrap();
        synthesize_replace_route_removes(&mut doc, dir.path());
        let has_remove_user = doc.remove.iter().any(|r| {
            matches!(r,
                DslRemove::RemoveRoute { variant } if variant == "User")
        });
        assert!(
            has_remove_user,
            "expected a synthesized RemoveRoute for User, got: {:?}",
            doc.remove
        );
        preflight(&doc, &[], dir.path(), false)
            .expect("collision should be resolved by replace_route");
    }

    #[test]
    fn replace_route_is_noop_when_no_collision() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/router.rs"),
            r#"use dioxus::prelude::*;

#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[route("/home")]
    Home {},
}
"#,
        )
        .unwrap();
        let mut doc: DslDoc = serde_yml::from_str(
            r#"version: "1"
screens:
  - name: Customers
    route: /users
    replace_route: true
"#,
        )
        .unwrap();
        synthesize_replace_route_removes(&mut doc, dir.path());
        assert!(
            doc.remove.is_empty(),
            "no existing collision → no synthesized removes, got: {:?}",
            doc.remove
        );
    }
}
