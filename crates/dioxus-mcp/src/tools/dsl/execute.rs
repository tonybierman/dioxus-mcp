use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use heck::{ToPascalCase, ToSnakeCase};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::state::State;
use crate::tools::scaffold::{
    self, ArgSpec, CreateRouteParams, CreateServerFnParams, PropSpec, ScaffoldResult,
};

use super::cargo_patch::*;
use super::generate::*;
use super::modify::apply_modify;
use super::plan::plan_dsl;
use super::remove::*;
use super::resources::*;
use super::specs::SPEC_VERSION;
use super::types::*;
use super::util::*;
use super::wire::*;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ExecuteCodeParams {
    /// A YAML doc conforming to the spec returned by get_dsl_spec.
    pub code: String,
    /// Absolute path to the Dioxus project root. Required when the MCP server
    /// was not started in the target project directory.
    pub project_root: Option<String>,
    /// When true, primitives whose leaf file already exists on disk are
    /// silently skipped (and reported in `collisions`) instead of erroring.
    /// Makes re-runs safe during iteration. Default: false (strict).
    #[serde(default)]
    pub if_missing: bool,
    /// When true, no files are written. The response contains `would_create`
    /// and `would_modify` lists describing what *would* happen, plus any
    /// collisions detected on disk. Default: false.
    #[serde(default)]
    pub dry_run: bool,
    /// When true, run `cargo check` (no-op build) in the crate root after a
    /// successful (non-dry-run) write to surface compile-time API drift
    /// inline. Failures are appended as a single `next_steps` entry; the
    /// written files are kept either way. Default: false.
    #[serde(default)]
    pub cargo_check: bool,
    /// When true, run `cargo fmt` over the files this call wrote/modified
    /// after the (non-dry-run) scaffold completes. Route inserts and
    /// App-body splices land unformatted otherwise — flip this on if you
    /// want the generated source to settle into the project's style without
    /// a manual follow-up. Failures land as a single `next_steps` entry;
    /// the scaffolded files are kept either way. Default: false.
    #[serde(default)]
    pub format_after: bool,
}

pub async fn execute_code(
    state: &Arc<State>,
    p: ExecuteCodeParams,
) -> Result<ScaffoldResult, String> {
    // Reject multi-document YAML — `serde_yml::from_str` would silently take
    // the first doc only and leave the rest dropped.
    if has_extra_documents(&p.code) {
        return Err(
            "execute_code: input must be a single YAML document; remove `---` separators".into(),
        );
    }
    let mut doc: DslDoc = serde_yml::from_str(&p.code).map_err(|e| format!("YAML parse: {e}"))?;
    if doc.version != SPEC_VERSION {
        return Err(format!(
            "execute_code: version must be {SPEC_VERSION:?}, got {:?}",
            doc.version
        ));
    }

    let synth_server_fns = expand_resources(&mut doc)?;
    // The `client_crud` Screen template emits `..Default::default()` in its
    // "add" form constructor. If the referenced model is declared in this
    // same doc but the user forgot to derive `Default` on it, the generated
    // body fails to compile (E0277). Quietly promote `Default` onto those
    // models so the doc-level wiring is self-consistent.
    ensure_default_on_client_crud_models(&mut doc);

    let crate_root = scaffold::crate_root(state, p.project_root.as_deref()).await?;

    // `replace_route: true` on a Screen / LoginScreen is a shorthand for
    // "drop the colliding on-disk variant first, then insert the new one."
    // Expand it into actual `remove: [{kind: remove_route, ...}]` entries so
    // the rest of the pipeline (preflight, dry_run plan, apply_removes) sees
    // the same shape it would if the user had written the removes themselves.
    synthesize_replace_route_removes(&mut doc, &crate_root);

    // Pre-compute the set of leaf files `remove:` will delete. Preflight
    // collision checks skip these so a single doc can "remove demo Hero;
    // create my Hero" in one call.
    let to_be_removed = removed_leaf_paths(&doc, &crate_root);

    preflight_with_removes(
        &doc,
        &synth_server_fns,
        &crate_root,
        p.if_missing,
        &to_be_removed,
    )?;

    if p.dry_run {
        // Removes are not applied in dry_run mode; the plan reports what
        // would be removed via the standard would_modify channel.
        let mut plan = plan_dsl(&doc, &synth_server_fns, &crate_root);
        plan_removes(&mut plan, &doc, &crate_root);
        plan.routable_file = detected_routable_file(&doc, &crate_root);
        // Screen previews: render the body each Screen entry would emit so
        // agents can inspect template output before committing. Skipped for
        // entries whose target path collides (the existing file wins).
        let collision_set: BTreeSet<&std::path::PathBuf> = plan.collisions.iter().collect();
        for sc in &doc.screens {
            let snake = sc.name.to_snake_case();
            let leaf = leaf_for(&crate_root, "src/components", &snake);
            if collision_set.contains(&leaf) {
                continue;
            }
            if let Ok(body) = build_screen_body(&crate_root, sc, &doc.client_stores) {
                plan.previews.insert(leaf, body);
            }
        }
        return Ok(plan);
    }

    // Apply removes first so the create steps below don't trip on the files
    // they're about to replace. Errors stop the run before any creates land.
    let mut result = ScaffoldResult::default();
    apply_removes(&doc, &crate_root, &mut result)?;

    // Companion to ensure_default_on_client_crud_models, but for the case
    // where the referenced model lives on disk (not in `doc.models`). The
    // generated client_crud screen body emits `..Default::default()`, so any
    // on-disk struct the user is reusing must also derive Default. Patches
    // the file's `#[derive(...)]` line in-place; idempotent.
    let patched_models = patch_on_disk_models_for_client_crud_default(&doc, &crate_root)?;
    for path in patched_models {
        if !result.files_modified.contains(&path) {
            result.files_modified.push(path);
        }
    }

    // Global preconditions that the per-primitive emitters used to discover
    // *after* writing files (and that left the project in a half-written state
    // on failure). Run these first so the call is atomic — either everything
    // applies or nothing does.
    let bootstrap = bootstrap_router_if_needed(&doc, &crate_root)?;
    let app_wiring = wire_app_if_needed(&doc, &crate_root)?;
    let routable_warning = routable_location_warning(&doc, &crate_root, &bootstrap);

    let skip: BTreeSet<std::path::PathBuf> = if p.if_missing {
        skip_set(&doc, &synth_server_fns, &crate_root)
    } else {
        BTreeSet::new()
    };

    let versioned_lists: BTreeSet<String> = doc
        .forms
        .iter()
        .filter_map(|f| f.feeds_into.as_ref().map(|l| l.to_snake_case()))
        .collect();
    let session_names: BTreeSet<String> = doc
        .session_states
        .iter()
        .map(|s| s.name.to_snake_case())
        .collect();

    // Fold in any router-bootstrap output up front so files_created/modified
    // (and the wiring `next_step`) appear in the response even when the rest
    // of the call is a no-op re-run.
    result.files_created.extend(bootstrap.created);
    result.files_modified.extend(bootstrap.modified);
    if let Some(s) = bootstrap.next_step {
        result.next_steps.push(s);
    }
    // App-wiring output: any main.rs/lib.rs edits land in files_modified, and
    // hints for cases we couldn't auto-wire land in next_steps.
    result.files_modified.extend(app_wiring.modified);
    result.next_steps.extend(app_wiring.next_steps);
    if let Some(w) = routable_warning {
        result.next_steps.push(w);
    }
    result.routable_file = detected_routable_file(&doc, &crate_root);

    // Order matters: models first (so server fn return types and stores can
    // resolve them), then server fns (fail-fast on fullstack gating), then
    // leaf primitives, then screens (which call create_route serially).
    for m in &doc.models {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/model", &m.name),
        ) {
            continue;
        }
        let r = generate_model(&crate_root, m)?;
        merge(&mut result, r);
    }

    for sf in &doc.server_fns {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/server", &sf.name),
        ) {
            continue;
        }
        let r = scaffold::create_server_fn(
            state,
            CreateServerFnParams {
                name: sf.name.clone(),
                args: sf
                    .args
                    .iter()
                    .map(|a| ArgSpec {
                        name: a.name.clone(),
                        ty: a.ty.clone(),
                    })
                    .collect(),
                return_type: sf.return_type.clone(),
                method: sf.method.clone(),
                path: sf.path.clone(),
                project_root: p.project_root.clone(),
            },
        )
        .await?;
        merge(&mut result, r);
    }

    for st in &doc.stores {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/state", &st.name),
        ) {
            continue;
        }
        let r = generate_store(&crate_root, st)?;
        merge(&mut result, r);
    }

    let model_names_for_imports: BTreeSet<String> =
        doc.models.iter().map(|m| m.name.to_snake_case()).collect();
    for cs in &doc.client_stores {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/state", &cs.name),
        ) {
            continue;
        }
        let r = generate_client_store(&crate_root, cs, &model_names_for_imports)?;
        merge(&mut result, r);
    }

    for sf in &synth_server_fns {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/server", &sf.name),
        ) {
            continue;
        }
        let r = generate_synth_server_fn(state, &crate_root, sf, p.project_root.as_deref()).await?;
        merge(&mut result, r);
    }

    for sig in &doc.signals {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/signals", &sig.name),
        ) {
            continue;
        }
        let r = generate_signal(&crate_root, sig)?;
        merge(&mut result, r);
    }

    let mut needs_websys = false;
    for s in &doc.sockets {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/sockets", &s.name),
        ) {
            continue;
        }
        let r = generate_socket(&crate_root, s)?;
        merge(&mut result, r);
        needs_websys = true;
    }

    for f in &doc.feeds {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/components", &f.name),
        ) {
            continue;
        }
        let r = generate_feed(&crate_root, f)?;
        merge(&mut result, r);
    }

    for c in &doc.components {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/components", &c.name),
        ) {
            continue;
        }
        let r = scaffold::create_component(
            state,
            scaffold::CreateComponentParams {
                name: c.name.clone(),
                props: c
                    .props
                    .iter()
                    .map(|p| PropSpec {
                        name: p.name.clone(),
                        ty: p.ty.clone(),
                        optional: p.optional,
                    })
                    .collect(),
                path: None,
                template: c.template.clone(),
                project_root: p.project_root.clone(),
            },
        )
        .await?;
        merge(&mut result, r);
    }

    for f in &doc.forms {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/components", &f.name),
        ) {
            continue;
        }
        let r = generate_form(&crate_root, f)?;
        merge(&mut result, r);
    }

    for l in &doc.lists {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/components", &l.name),
        ) {
            continue;
        }
        let v = versioned_lists.contains(&l.name.to_snake_case());
        let r = generate_list(&crate_root, l, v)?;
        merge(&mut result, r);
    }

    for t in &doc.tables {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/components", &t.name),
        ) {
            continue;
        }
        let r = generate_table(&crate_root, t)?;
        merge(&mut result, r);
    }

    for s in &doc.session_states {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/auth", &s.name),
        ) {
            continue;
        }
        let r = generate_session(&crate_root, s)?;
        merge(&mut result, r);
    }

    for ls in &doc.login_screens {
        let leaf = leaf_for(&crate_root, "src/components", &ls.name);
        if skip.contains(&leaf) {
            // Body already on disk; still run the idempotent route insert so
            // a re-run after a partial failure finishes the wiring. Without
            // this, the response on rerun says `next_steps: []` even though
            // the Routable variant was never added.
            result.collisions.push(leaf);
            let route = scaffold::create_route(
                state,
                CreateRouteParams {
                    path: ls.route.clone(),
                    component: ls.name.to_pascal_case(),
                    router_file: None,
                    project_root: p.project_root.clone(),
                    params: Vec::new(),
                    import_path: Some("crate::components".to_string()),
                },
            )
            .await?;
            merge(&mut result, route);
            continue;
        }
        let r = generate_login_screen(state, &crate_root, ls, p.project_root.as_deref()).await?;
        merge(&mut result, r);
    }

    for pr in &doc.protected_routes {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/components", &pr.name),
        ) {
            continue;
        }
        let r = generate_protected_route(&crate_root, pr, &session_names)?;
        merge(&mut result, r);
    }

    for sc in &doc.screens {
        let leaf = leaf_for(&crate_root, "src/components", &sc.name);
        if skip.contains(&leaf) {
            // See login_screens loop above: idempotent route insert on skip.
            result.collisions.push(leaf);
            let route = scaffold::create_route(
                state,
                CreateRouteParams {
                    path: sc.route.clone(),
                    component: sc.name.to_pascal_case(),
                    router_file: None,
                    project_root: p.project_root.clone(),
                    params: sc.route_params.clone(),
                    import_path: Some("crate::components".to_string()),
                },
            )
            .await?;
            merge(&mut result, route);
            continue;
        }
        let r = generate_screen(
            state,
            &crate_root,
            sc,
            &doc.client_stores,
            p.project_root.as_deref(),
        )
        .await?;
        merge(&mut result, r);
    }

    for m in &doc.modify {
        apply_modify(&crate_root, m, p.if_missing, &mut result)?;
    }

    if needs_websys {
        result.next_steps.push(
            "add `web-sys = { version = \"0.3\", features = [\"WebSocket\", \"MessageEvent\", \"BinaryType\", \"ErrorEvent\"] }` and `wasm-bindgen = \"0.2\"` to your Cargo.toml for the generated socket(s)".into(),
        );
    }

    // Auto-declare top-level modules in the crate root (src/main.rs or
    // src/lib.rs) for every subdir we wrote into. Skips quietly if no crate
    // root is found (e.g. workspace-only layout); the generated files will
    // still be on disk and a next_steps hint covers the manual case.
    let touched_top_mods = top_level_modules_touched(&result, &crate_root);
    for module in &touched_top_mods {
        match scaffold::upsert_crate_mod(&crate_root, module) {
            Ok(Some(path)) => result.files_modified.push(path),
            Ok(None) => {}
            Err(e) => {
                result.next_steps.push(format!(
                    "could not auto-declare `pub mod {module};` in crate root: {e} — add it yourself in src/main.rs or src/lib.rs"
                ));
            }
        }
    }
    if scaffold::find_crate_root_file(&crate_root).is_none() && !touched_top_mods.is_empty() {
        let mods = touched_top_mods.join(", ");
        result.next_steps.push(format!(
            "no src/main.rs or src/lib.rs found — declare `pub mod {{{mods}}};` in your crate root manually"
        ));
    }

    // When the run scaffolded a data layer (model / state) but no
    // src/components dir exists, bootstrap an empty `src/components/mod.rs`
    // and declare `pub mod components;` in the crate root. Keeps the
    // components/ subdir symmetric with model/ and state/ — hand-written
    // components can drop in immediately with `use crate::components::Foo;`
    // and no follow-up scaffold step.
    {
        let comp_dir = crate_root.join("src/components");
        let data_dir_touched = touched_top_mods
            .iter()
            .any(|m| m == "model" || m == "state");
        if data_dir_touched && !comp_dir.exists() {
            if let Err(e) = std::fs::create_dir_all(&comp_dir) {
                result.next_steps.push(format!(
                    "could not create `src/components/`: {e} — create it yourself and declare \
                     `pub mod components;` in your crate root"
                ));
            } else {
                let mod_rs = comp_dir.join("mod.rs");
                if !mod_rs.exists() {
                    match std::fs::write(&mod_rs, "") {
                        Ok(()) => result.files_created.push(mod_rs),
                        Err(e) => result
                            .next_steps
                            .push(format!("could not create `src/components/mod.rs`: {e}")),
                    }
                }
                match scaffold::upsert_crate_mod(&crate_root, "components") {
                    Ok(Some(path)) => result.files_modified.push(path),
                    Ok(None) => {}
                    Err(e) => result.next_steps.push(format!(
                        "created `src/components/` but could not declare `pub mod components;` \
                         in crate root: {e} — add it yourself"
                    )),
                }
            }
        }
    }

    // Patch Cargo.toml whenever the doc declares models — not just when a
    // model file was emitted this run. A re-run with `if_missing: true` skips
    // every model write but still needs the serde dep to be in place; without
    // this, a first-call partial failure followed by a successful re-run could
    // leave Cargo.toml unpatched.
    if !doc.models.is_empty() {
        match ensure_serde_in_cargo_toml(&crate_root) {
            Ok(SerdePatch::AlreadyOk) => {}
            Ok(SerdePatch::Patched(path)) => {
                result.files_modified.push(path);
                result
                    .next_steps
                    .push("Cargo.toml: added `serde = { version = \"1\", features = [\"derive\"] }` (required by the generated model(s))".into());
            }
            Ok(SerdePatch::PresentWithoutDeriveFeature) => {
                result.next_steps.push(
                    "Cargo.toml: `serde` is declared without the `derive` feature — add `features = [\"derive\"]` so the generated model(s) compile".into(),
                );
            }
            Ok(SerdePatch::NoCargoToml) => {
                result.next_steps.push(
                    "Cargo.toml: missing at the crate root — declare `serde = { version = \"1\", features = [\"derive\"] }` somewhere upstream for the generated model(s)".into(),
                );
            }
            Err(e) => {
                result.next_steps.push(format!(
                    "Cargo.toml: auto-patch for serde failed ({e}) — add `serde = {{ version = \"1\", features = [\"derive\"] }}` manually"
                ));
            }
        }
    }

    // Patch Cargo.toml's `dioxus` features to include `router` whenever the
    // doc declares any routable primitive (Screen / LoginScreen). Parity with
    // the serde patch above: we run on the declared doc, not just the
    // files-actually-emitted set, so a partial-run / re-run still converges.
    if !doc.screens.is_empty() || !doc.login_screens.is_empty() {
        match ensure_dioxus_router_in_cargo_toml(&crate_root) {
            Ok(DioxusRouterPatch::AlreadyOk) => {}
            Ok(DioxusRouterPatch::Patched(path)) => {
                result.files_modified.push(path);
                result
                    .next_steps
                    .push("Cargo.toml: enabled the `router` feature on the `dioxus` dep (required by the generated screen(s))".into());
            }
            Ok(DioxusRouterPatch::DioxusNotATable) => {
                result.next_steps.push(
                    "Cargo.toml: the `dioxus` dep is a bare version string — switch it to a table and add `features = [\"router\"]` so the generated screen(s) compile".into(),
                );
            }
            Ok(DioxusRouterPatch::DioxusMissing) => {
                result.next_steps.push(
                    "Cargo.toml: no `dioxus` dep — add one with `features = [\"router\"]` (or `\"fullstack\"`) so the generated screen(s) compile".into(),
                );
            }
            Ok(DioxusRouterPatch::NoCargoToml) => {
                result.next_steps.push(
                    "Cargo.toml: missing at the crate root — declare `dioxus` with the `router` feature somewhere upstream for the generated screen(s)".into(),
                );
            }
            Err(e) => {
                result.next_steps.push(format!(
                    "Cargo.toml: auto-patch for dioxus/router failed ({e}) — add `router` to the `dioxus` dep's features array manually"
                ));
            }
        }
    }

    // Adjacent-audit hints: surface common feature/dep gaps that the per-
    // primitive writers don't catch on their own (explicit `server_fns:` go
    // through scaffold::create_server_fn which doesn't gate on fullstack).
    surface_feature_gap_hints(&doc, &synth_server_fns, &crate_root, &mut result);

    dedup_paths(&mut result.files_created);
    dedup_paths(&mut result.files_modified);
    dedup_paths(&mut result.collisions);

    // Surface hand-edit hotspots: for every newly-created file the scaffolder
    // wrote, find `// TODO` markers and add one `next_steps` entry per
    // occurrence, formatted `path:line — message`. Lets the caller jump
    // straight to the body lines that still need a human (TODO4 §4.2).
    append_todo_next_steps(&mut result, &crate_root);

    // Opt-in `cargo fmt` so route inserts / App-body splices end up tidy
    // without a manual follow-up. Runs only on calls that actually wrote
    // something, and over the exact set of files we touched (avoids
    // surprising the user by re-formatting unrelated source).
    if p.format_after
        && (!result.files_created.is_empty() || !result.files_modified.is_empty())
        && let Some(msg) = run_cargo_fmt(&crate_root, &result).await
    {
        result.next_steps.push(msg);
    }

    // Opt-in `cargo check` so callers can surface compile-time breakage
    // from generated-vs-host API drift in the same call instead of finding
    // out 30s later. We only run it when the call actually wrote something
    // — a pure no-change re-run wouldn't have new breakage to surface.
    if p.cargo_check
        && (!result.files_created.is_empty() || !result.files_modified.is_empty())
        && let Some(msg) = run_cargo_check(&crate_root).await
    {
        result.next_steps.push(msg);
    } else if !p.cargo_check
        && !p.dry_run
        && (!result.files_created.is_empty() || !result.files_modified.is_empty())
        && is_nontrivial_scaffold(&doc)
    {
        // Discoverability: surface the `cargo_check: true` opt-in on any
        // non-trivial scaffold that actually wrote something, so callers
        // (especially agents) learn it exists without having to read the
        // schema. Suppressed when cargo_check was already opted in, on
        // dry-runs, on pure no-op re-runs, or on trivial single-primitive
        // scaffolds where a manual `cargo check` is trivially equivalent.
        result.next_steps.push(
            "tip: re-run with `cargo_check: true` to surface compile-time errors from the scaffolded code in the same response".into(),
        );
    }

    // High-level outcome so callers don't have to interpret three vector
    // lengths. `no_changes` means everything collided (a totally idempotent
    // re-run); `partial` means at least one primitive was skipped while the
    // rest applied; `applied` is the clean-run case.
    let touched = !result.files_created.is_empty() || !result.files_modified.is_empty();
    let collided = !result.collisions.is_empty();
    result.status = Some(match (touched, collided) {
        (false, true) => "no_changes".into(),
        (true, true) => "partial".into(),
        _ => "applied".into(),
    });

    Ok(result)
}

/// True when the doc scaffolds enough that compile-time errors are plausible
/// and a `cargo check` is worth surfacing. Used to gate the `cargo_check: true`
/// discoverability hint — we don't want to nag callers for trivial one-primitive
/// scaffolds.
fn is_nontrivial_scaffold(doc: &DslDoc) -> bool {
    let touches_server =
        !doc.server_fns.is_empty() || doc.resources.iter().any(|r| !r.fields.is_empty());
    let touches_screens = !doc.screens.is_empty() || !doc.login_screens.is_empty();
    let touches_data = !doc.stores.is_empty()
        || !doc.client_stores.is_empty()
        || !doc.signals.is_empty()
        || !doc.sockets.is_empty();
    let primitive_count = doc.models.len()
        + doc.server_fns.len()
        + doc.resources.len()
        + doc.stores.len()
        + doc.client_stores.len()
        + doc.signals.len()
        + doc.sockets.len()
        + doc.feeds.len()
        + doc.components.len()
        + doc.forms.len()
        + doc.lists.len()
        + doc.tables.len()
        + doc.screens.len()
        + doc.login_screens.len()
        + doc.protected_routes.len()
        + doc.session_states.len();
    touches_server || touches_screens || touches_data || primitive_count >= 2
}

/// Append `next_steps` hints when the doc emitted primitives that depend on
/// dioxus features the project isn't currently building with. Keeps the
/// adjacency narrow — we only flag the case the user is one keystroke away
/// from hitting: added a server fn (explicit or synth) without `fullstack`
/// (or `server` + `web`) enabled on the `dioxus` dep.
fn surface_feature_gap_hints(
    doc: &DslDoc,
    synth_server_fns: &[SynthServerFn],
    crate_root: &Path,
    result: &mut ScaffoldResult,
) {
    let added_server_fn = !doc.server_fns.is_empty() || !synth_server_fns.is_empty();
    if !added_server_fn {
        return;
    }
    let info = crate::project::ProjectInfo::detect(crate_root);
    if !info.is_dioxus_project {
        return;
    }
    let active = &info.dioxus_features;
    let fullstack_capable = active.iter().any(|f| f == "fullstack")
        || (active.iter().any(|f| f == "server") && active.iter().any(|f| f == "web"));
    if fullstack_capable {
        return;
    }
    result.next_steps.push(format!(
        "audit hint: this run added server fn(s) but the `dioxus` dep's features ({:?}) don't include `fullstack` (or `server`+`web`) — call `audit_feature_flags` for the recommended patch, or add `features = [\"fullstack\"]` to the `dioxus` dep so the server-side code compiles",
        active
    ));
}

/// Run `cargo check --message-format=short` in `crate_root` with a generous
/// timeout. Returns `Some(message)` when the check fails (or doesn't complete),
/// `None` when it succeeds. The returned message is a single `next_steps`
/// entry — we truncate stderr so a slow build doesn't bloat the response.
async fn run_cargo_check(crate_root: &Path) -> Option<String> {
    use tokio::process::Command;
    use tokio::time::{Duration, timeout};

    let mut cmd = Command::new("cargo");
    cmd.arg("check")
        .arg("--message-format=short")
        .current_dir(crate_root);
    // Quiet down build progress so the captured output is just diagnostics.
    cmd.env("CARGO_TERM_COLOR", "never");

    let fut = cmd.output();
    let out = match timeout(Duration::from_secs(180), fut).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            return Some(format!(
                "cargo_check: failed to spawn `cargo check`: {e} — run it yourself in {}",
                crate_root.display()
            ));
        }
        Err(_) => {
            return Some(format!(
                "cargo_check: `cargo check` exceeded the 180s budget — run it yourself in {}",
                crate_root.display()
            ));
        }
    };
    if out.status.success() {
        return None;
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Pull the first ~20 lines of diagnostics — enough for the first few
    // errors without burying the rest of the response.
    let snippet: String = stderr.lines().take(20).collect::<Vec<_>>().join("\n");
    Some(format!(
        "cargo_check: `cargo check` failed (exit {:?}). First diagnostics:\n{snippet}",
        out.status.code()
    ))
}

/// Run `rustfmt` over the exact set of files this scaffold call wrote or
/// modified. We bypass `cargo fmt` so the formatting is scoped — `cargo fmt`
/// would format the entire crate, which is surprising on top of a focused
/// scaffold. Returns `Some(message)` when formatting fails or rustfmt is
/// unavailable, `None` on success. The returned message is a single
/// `next_steps` entry; the scaffolded files are kept either way.
async fn run_cargo_fmt(crate_root: &Path, result: &ScaffoldResult) -> Option<String> {
    use tokio::process::Command;
    use tokio::time::{Duration, timeout};

    // Collect a deduped, .rs-only list of touched paths. rustfmt rejects
    // non-Rust files (e.g. Cargo.toml, mod.rs we wrote) wholesale, so we
    // filter rather than let it bail out.
    let mut paths: Vec<PathBuf> = Vec::new();
    for p in result
        .files_created
        .iter()
        .chain(result.files_modified.iter())
    {
        if p.extension().and_then(|x| x.to_str()) != Some("rs") {
            continue;
        }
        if !paths.contains(p) {
            paths.push(p.clone());
        }
    }
    if paths.is_empty() {
        return None;
    }

    let mut cmd = Command::new("rustfmt");
    cmd.arg("--edition=2024");
    for p in &paths {
        cmd.arg(p);
    }
    cmd.current_dir(crate_root);

    let fut = cmd.output();
    let out = match timeout(Duration::from_secs(60), fut).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            return Some(format!(
                "format_after: failed to spawn `rustfmt`: {e} — run `cargo fmt` yourself in {}",
                crate_root.display()
            ));
        }
        Err(_) => {
            return Some(format!(
                "format_after: `rustfmt` exceeded the 60s budget — run `cargo fmt` yourself in {}",
                crate_root.display()
            ));
        }
    };
    if out.status.success() {
        return None;
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let snippet: String = stderr.lines().take(10).collect::<Vec<_>>().join("\n");
    Some(format!(
        "format_after: `rustfmt` failed (exit {:?}). First diagnostics:\n{snippet}",
        out.status.code()
    ))
}

/// Scan every freshly-created file for `// TODO` markers and surface
/// `path:line — message` entries on `next_steps`. Paths are emitted relative
/// to the crate root so they paste cleanly into editors.
fn append_todo_next_steps(result: &mut ScaffoldResult, crate_root: &Path) {
    let mut hotspots: Vec<String> = Vec::new();
    for path in &result.files_created {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix("// TODO") {
                let message = rest.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
                let rel = path.strip_prefix(crate_root).unwrap_or(path);
                let entry = if message.is_empty() {
                    format!("{}:{} — TODO", rel.display(), i + 1)
                } else {
                    format!("{}:{} — TODO {}", rel.display(), i + 1, message)
                };
                hotspots.push(entry);
            }
        }
    }
    // Stable order: by path then line — the per-file scan above already gives
    // us this, but if multiple files emit hits we sort to keep output reviewable.
    hotspots.sort();
    if !hotspots.is_empty() {
        result.next_steps.push(format!(
            "{} hand-edit hotspot(s) marked `// TODO` in generated files:",
            hotspots.len()
        ));
        result.next_steps.extend(hotspots);
    }
}
