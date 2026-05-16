//! Declarative-DSL scaffolding tools.
//!
//! `get_dsl_spec` returns the YAML vocabulary describing every DSL primitive.
//! `execute_code` parses a YAML doc and materializes the corresponding Dioxus
//! 0.7 source files in one shot.
//!
//! Single source of truth: each primitive has a colocated `&'static str` spec
//! block AND a Rust struct used both for serde deserialization and to drive
//! the per-primitive generator. The `spec_examples_round_trip` unit test
//! enforces that every spec example deserializes into its struct.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use heck::{ToPascalCase, ToSnakeCase};
use minijinja::{Environment, context};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::state::State;
use crate::tools::scaffold::{
    self, ArgSpec, CreateRouteParams, CreateServerFnParams, ModUpsert, PropSpec, ScaffoldResult,
    upsert_mod_entry,
};

mod types;
pub use types::*;

mod specs;
use specs::*;

mod templates;
use templates::*;

mod spec;
pub use spec::*;

// ===========================================================================
// `execute_code`
// ===========================================================================

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

mod cargo_patch;
use cargo_patch::*;

/// Order-preserving dedup. `files_modified` in particular accumulates one
/// entry per route/component insertion (e.g. src/main.rs and src/components/mod.rs
/// show up dozens of times in a resource scaffold); deduping keeps the response
/// scannable.
fn dedup_paths(v: &mut Vec<std::path::PathBuf>) {
    let mut seen: BTreeSet<std::path::PathBuf> = BTreeSet::new();
    v.retain(|p| seen.insert(p.clone()));
}

/// Return the unique set of top-level src/{module}/ subdirs that received at
/// least one emitted file. Used to drive crate-root `pub mod` injection.
fn top_level_modules_touched(result: &ScaffoldResult, crate_root: &Path) -> Vec<String> {
    let src = crate_root.join("src");
    let mut out: BTreeSet<String> = BTreeSet::new();
    let scan = |paths: &Vec<std::path::PathBuf>, out: &mut BTreeSet<String>| {
        for p in paths {
            let Ok(rel) = p.strip_prefix(&src) else {
                continue;
            };
            let mut comps = rel.components();
            let Some(first) = comps.next() else { continue };
            // Only count entries that are *inside* a subdir (i.e. there's
            // another component after the first) — a bare `src/main.rs` edit
            // isn't a module subdir.
            if comps.next().is_none() {
                continue;
            }
            if let std::path::Component::Normal(name) = first
                && let Some(n) = name.to_str()
            {
                out.insert(n.to_string());
            }
        }
    };
    scan(&result.files_created, &mut out);
    scan(&result.files_modified, &mut out);
    out.into_iter().collect()
}

fn has_extra_documents(yaml: &str) -> bool {
    // A leading "---" is a valid single-document marker; multiple "---" lines
    // (or any "---" after non-whitespace content) means multi-document.
    let mut seen_content = false;
    for line in yaml.lines() {
        let trimmed = line.trim_end();
        if trimmed == "---" {
            if seen_content {
                return true;
            }
        } else if !line.trim().is_empty() && !line.trim_start().starts_with('#') {
            seen_content = true;
        }
    }
    false
}

fn merge(into: &mut ScaffoldResult, from: ScaffoldResult) {
    into.files_created.extend(from.files_created);
    into.files_modified.extend(from.files_modified);
    into.next_steps.extend(from.next_steps);
    into.collisions.extend(from.collisions);
    into.would_create.extend(from.would_create);
    into.would_modify.extend(from.would_modify);
}

pub(super) fn leaf_for(crate_root: &Path, subdir: &str, name: &str) -> std::path::PathBuf {
    let snake = name.to_snake_case();
    crate_root.join(subdir).join(format!("{snake}.rs"))
}

/// If `target` is in the skip set, record it as a collision and return true.
fn skip_or_record(
    skip: &BTreeSet<std::path::PathBuf>,
    result: &mut ScaffoldResult,
    target: std::path::PathBuf,
) -> bool {
    if skip.contains(&target) {
        result.collisions.push(target);
        true
    } else {
        false
    }
}

/// Walk the doc and return the set of leaf files that already exist on disk —
/// the primitives whose target file should be skipped in `if_missing` mode.
fn skip_set(
    doc: &DslDoc,
    synth_server_fns: &[SynthServerFn],
    crate_root: &Path,
) -> BTreeSet<std::path::PathBuf> {
    let mut s = BTreeSet::new();
    let mut maybe_add = |subdir: &str, name: &str| {
        let p = leaf_for(crate_root, subdir, name);
        if p.exists() {
            s.insert(p);
        }
    };
    for c in &doc.components {
        maybe_add("src/components", &c.name);
    }
    for f in &doc.forms {
        maybe_add("src/components", &f.name);
    }
    for l in &doc.lists {
        maybe_add("src/components", &l.name);
    }
    for t in &doc.tables {
        maybe_add("src/components", &t.name);
    }
    for f in &doc.feeds {
        maybe_add("src/components", &f.name);
    }
    for ls in &doc.login_screens {
        maybe_add("src/components", &ls.name);
    }
    for pr in &doc.protected_routes {
        maybe_add("src/components", &pr.name);
    }
    for sc in &doc.screens {
        maybe_add("src/components", &sc.name);
    }
    for sf in &doc.server_fns {
        maybe_add("src/server", &sf.name);
    }
    for sig in &doc.signals {
        maybe_add("src/signals", &sig.name);
    }
    for sk in &doc.sockets {
        maybe_add("src/sockets", &sk.name);
    }
    for ss in &doc.session_states {
        maybe_add("src/auth", &ss.name);
    }
    for m in &doc.models {
        maybe_add("src/model", &m.name);
    }
    for st in &doc.stores {
        maybe_add("src/state", &st.name);
    }
    for cs in &doc.client_stores {
        maybe_add("src/state", &cs.name);
    }
    for sf in synth_server_fns {
        maybe_add("src/server", &sf.name);
    }
    s
}

/// Compute the would-be plan for a dry-run: for every primitive in `doc`,
/// classify its leaf file as `would_create` (path is free) or `collisions`
/// (path already exists), and classify the parent `mod.rs` plus any touched
/// router file as `would_create` / `would_modify`.
fn plan_dsl(doc: &DslDoc, synth_server_fns: &[SynthServerFn], crate_root: &Path) -> ScaffoldResult {
    let mut out = ScaffoldResult {
        dry_run: true,
        ..Default::default()
    };
    let mut mods_touched: BTreeSet<std::path::PathBuf> = BTreeSet::new();

    let leaf = |out: &mut ScaffoldResult,
                mods_touched: &mut BTreeSet<std::path::PathBuf>,
                subdir: &str,
                name: &str| {
        let leaf_path = leaf_for(crate_root, subdir, name);
        if leaf_path.exists() {
            out.collisions.push(leaf_path);
        } else {
            out.would_create.push(leaf_path);
        }
        let mod_path = crate_root.join(subdir).join("mod.rs");
        if mods_touched.insert(mod_path.clone()) {
            if mod_path.exists() {
                out.would_modify.push(mod_path);
            } else {
                out.would_create.push(mod_path);
            }
        }
    };

    for c in &doc.components {
        leaf(&mut out, &mut mods_touched, "src/components", &c.name);
    }
    for f in &doc.forms {
        leaf(&mut out, &mut mods_touched, "src/components", &f.name);
    }
    for l in &doc.lists {
        leaf(&mut out, &mut mods_touched, "src/components", &l.name);
    }
    for t in &doc.tables {
        leaf(&mut out, &mut mods_touched, "src/components", &t.name);
    }
    for f in &doc.feeds {
        leaf(&mut out, &mut mods_touched, "src/components", &f.name);
    }
    for ls in &doc.login_screens {
        leaf(&mut out, &mut mods_touched, "src/components", &ls.name);
    }
    for pr in &doc.protected_routes {
        leaf(&mut out, &mut mods_touched, "src/components", &pr.name);
    }
    for sc in &doc.screens {
        leaf(&mut out, &mut mods_touched, "src/components", &sc.name);
    }
    for sf in &doc.server_fns {
        leaf(&mut out, &mut mods_touched, "src/server", &sf.name);
    }
    for sig in &doc.signals {
        leaf(&mut out, &mut mods_touched, "src/signals", &sig.name);
    }
    for sk in &doc.sockets {
        leaf(&mut out, &mut mods_touched, "src/sockets", &sk.name);
    }
    for ss in &doc.session_states {
        leaf(&mut out, &mut mods_touched, "src/auth", &ss.name);
    }
    for m in &doc.models {
        leaf(&mut out, &mut mods_touched, "src/model", &m.name);
    }
    for st in &doc.stores {
        leaf(&mut out, &mut mods_touched, "src/state", &st.name);
    }
    for cs in &doc.client_stores {
        leaf(&mut out, &mut mods_touched, "src/state", &cs.name);
    }
    for sf in synth_server_fns {
        leaf(&mut out, &mut mods_touched, "src/server", &sf.name);
    }

    // Router file: modified when there are routed primitives (screens or login_screens).
    if (!doc.screens.is_empty() || !doc.login_screens.is_empty())
        && let Some(router) = scaffold::find_routable(crate_root)
    {
        out.would_modify.push(router);
    }

    // `modify:` entries — classify each target as would_modify (file present)
    // or collisions (missing, would error or be skipped in if_missing mode).
    for m in &doc.modify {
        let target_path = modify_target_path(m, crate_root);
        if target_path.exists() {
            if !out.would_modify.iter().any(|p| p == &target_path) {
                out.would_modify.push(target_path);
            }
        } else {
            out.collisions.push(target_path);
        }
    }

    dedup_paths(&mut out.would_create);
    dedup_paths(&mut out.would_modify);
    dedup_paths(&mut out.collisions);
    out
}

/// Canonicalize a route-path string for collision detection. Strip a trailing
/// slash (except for the root "/") and replace any `:param` segments with a
/// `:` placeholder so `/users/:id` and `/users/:user_id` collide as the user
/// intends (same shape, different param name). Stays purely textual — no
/// `Routable`-style nesting awareness needed for pre-flight.
pub(super) fn normalize_route_path(path: &str) -> String {
    let trimmed = if path.len() > 1 {
        path.trim_end_matches('/')
    } else {
        path
    };
    trimmed
        .split('/')
        .map(|seg| if seg.starts_with(':') { ":" } else { seg })
        .collect::<Vec<_>>()
        .join("/")
}

fn modify_target_path(m: &DslModify, crate_root: &Path) -> std::path::PathBuf {
    match m {
        DslModify::AddModelField { model, .. } | DslModify::RemoveModelField { model, .. } => {
            leaf_for(crate_root, "src/model", model)
        }
        DslModify::AddComponentProp { component, .. }
        | DslModify::RemoveComponentProp { component, .. } => {
            leaf_for(crate_root, "src/components", component)
        }
        DslModify::AddServerFnArg { server_fn, .. } => {
            leaf_for(crate_root, "src/server", server_fn)
        }
    }
}

// ---------- pre-flight ----------

pub(super) fn preflight(
    doc: &DslDoc,
    synth_server_fns: &[SynthServerFn],
    crate_root: &Path,
    if_missing: bool,
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

    // 2. Verify cross-references exist within the doc.
    for f in &doc.feeds {
        if !sock_names.contains(&f.socket.to_snake_case()) {
            return Err(format!(
                "feed {:?} references unknown socket {:?}",
                f.name, f.socket
            ));
        }
    }
    for l in &doc.lists {
        if !srv_names.contains(&l.endpoint.to_snake_case()) {
            return Err(format!(
                "list {:?} references unknown server_fn {:?}; declare it under server_fns",
                l.name, l.endpoint
            ));
        }
    }
    for t in &doc.tables {
        if !srv_names.contains(&t.endpoint.to_snake_case()) {
            return Err(format!(
                "table {:?} references unknown server_fn {:?}; declare it under server_fns",
                t.name, t.endpoint
            ));
        }
    }
    let list_names: BTreeSet<String> = doc.lists.iter().map(|l| l.name.to_snake_case()).collect();
    for f in &doc.forms {
        if let Some(target) = &f.feeds_into
            && !list_names.contains(&target.to_snake_case())
        {
            return Err(format!(
                "form {:?} feeds_into unknown list {:?}; declare it under lists",
                f.name, target
            ));
        }
    }
    for pr in &doc.protected_routes {
        if let Some(req) = &pr.requires
            && !sess_names.contains(&req.to_snake_case())
        {
            return Err(format!(
                "protected_route {:?} requires unknown session_state {:?}; declare it under session_states",
                pr.name, req
            ));
        }
    }
    for s in &doc.stores {
        if !model_names.contains(&s.resource.to_snake_case()) {
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
            if !client_store_names.contains(&store.to_snake_case()) {
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

/// If the doc declares any routable primitive (Screen, LoginScreen) and no
/// Routable enum exists anywhere under src/, write a minimal `src/router.rs`
/// seeded with every declared route, and inject `pub mod router;` into the
/// crate root. Makes `dx new` → `execute_code` runnable in one call instead
/// of erroring on the first screen with "no Routable enum on disk".
///
/// Returns the list of paths created/modified by the bootstrap (caller merges
/// these into the top-level result so the response stays honest).
fn bootstrap_router_if_needed(doc: &DslDoc, crate_root: &Path) -> Result<BootstrapRouter, String> {
    if scaffold::find_routable(crate_root).is_some() {
        return Ok(BootstrapRouter::default());
    }
    // Order matches declaration order in the doc: login_screens first (so the
    // login route lands before any post-auth screens), then screens.
    struct SeedRoute {
        variant: String,
        path: String,
        params: Vec<(String, String)>,
    }
    let mut entries: Vec<SeedRoute> = Vec::new();
    for ls in &doc.login_screens {
        entries.push(SeedRoute {
            variant: ls.name.to_pascal_case(),
            path: ls.route.clone(),
            params: Vec::new(),
        });
    }
    for sc in &doc.screens {
        entries.push(SeedRoute {
            variant: sc.name.to_pascal_case(),
            path: sc.route.clone(),
            params: sc.route_params.clone(),
        });
    }
    if entries.is_empty() {
        return Ok(BootstrapRouter::default());
    }
    let mut body = String::from("use dioxus::prelude::*;\n");
    // Routable's derive expands each variant to `ComponentName(props)` — the
    // identifier must be in scope at the enum's site. Wildcard-importing the
    // components module covers every screen we emit (Screen / LoginScreen /
    // crud-generated *NewScreen etc.) without needing to enumerate names
    // here, and matches the mod.rs wildcard re-export pattern.
    body.push_str("use crate::components::*;\n\n");
    body.push_str("#[derive(Routable, Clone, PartialEq)]\n");
    body.push_str("pub enum Route {\n");
    for SeedRoute {
        variant,
        path,
        params,
    } in &entries
    {
        let field_inner = if params.is_empty() {
            String::new()
        } else {
            format!(
                " {} ",
                params
                    .iter()
                    .map(|(n, t)| format!("{n}: {t}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        body.push_str(&format!("    #[route(\"{path}\")]\n"));
        body.push_str(&format!("    {variant} {{{field_inner}}},\n"));
    }
    body.push_str("}\n");

    let router_path = crate_root.join("src/router.rs");
    if let Some(parent) = router_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&router_path, body).map_err(|e| e.to_string())?;

    let mut out = BootstrapRouter {
        created: vec![router_path],
        modified: Vec::new(),
        next_step: Some(
            "auto-created `src/router.rs` with a Routable enum seeded from the declared screens — \
             mount it in your App component as `Router::<crate::router::Route> {}` (and make sure \
             your Cargo.toml's `dioxus` dep includes the `router` feature, which `dx new` enables \
             via `fullstack`)."
                .into(),
        ),
    };
    if let Some(p) = scaffold::upsert_crate_mod(crate_root, "router")? {
        out.modified.push(p);
    }
    Ok(out)
}

#[derive(Default)]
struct BootstrapRouter {
    created: Vec<std::path::PathBuf>,
    modified: Vec<std::path::PathBuf>,
    next_step: Option<String>,
}

/// Locate the file holding the `#[derive(Routable)]` enum so the response
/// can report where new route variants will land. Returns None when the doc
/// declares no routes (so no enum will be touched) or the project has no
/// Routable enum on disk yet (the router-bootstrap step will create one at
/// the canonical path; that path is already covered by `files_created`).
fn detected_routable_file(doc: &DslDoc, crate_root: &Path) -> Option<std::path::PathBuf> {
    if doc.screens.is_empty() && doc.login_screens.is_empty() {
        return None;
    }
    scaffold::find_routable(crate_root)
}

/// Surface a hint when the doc would mutate a Routable enum that lives
/// somewhere truly off-the-beaten-path. We don't refuse to act — host files
/// like `src/main.rs` or `src/lib.rs` are still patched via syn — but a
/// next_steps note tells the user where the edit landed.
///
/// `dx new` puts the Routable enum in `src/main.rs`, so that location is
/// treated as conventional too (along with `src/lib.rs`) — historically this
/// warning fired on every fresh starter, which was just noise. The warning
/// now only fires when the enum lives somewhere we genuinely didn't expect
/// (e.g. nested under a feature module).
///
/// Returns None when:
///   - the doc declares no routes (nothing to mutate), or
///   - we just created `src/router.rs` ourselves (conventional location), or
///   - the existing Routable lives at one of the conventional paths.
fn routable_location_warning(
    doc: &DslDoc,
    crate_root: &Path,
    bootstrap: &BootstrapRouter,
) -> Option<String> {
    if doc.screens.is_empty() && doc.login_screens.is_empty() {
        return None;
    }
    // If bootstrap created the router, it's at the canonical location by
    // construction — skip the warning.
    if !bootstrap.created.is_empty() {
        return None;
    }
    let path = scaffold::find_routable(crate_root)?;
    let rel = path.strip_prefix(crate_root).unwrap_or(&path);
    // Normalize the relative path with forward slashes so the warning text
    // is stable on Windows.
    let rel_str = rel.to_string_lossy().replace('\\', "/");
    // src/main.rs and src/lib.rs are crate roots — the `dx new` starter
    // ships the Routable enum in main.rs, so flagging it as "non-conventional"
    // misleads users on a clean scaffold. Treat them as conventional too.
    const CONVENTIONAL: &[&str] = &["src/router.rs", "src/route.rs", "src/main.rs", "src/lib.rs"];
    if CONVENTIONAL.iter().any(|p| *p == rel_str) {
        return None;
    }
    Some(format!(
        "Routable enum found in non-conventional location {rel_str:?} — \
         new route variants will be inserted there. For long-term \
         consistency consider moving the enum into `src/router.rs` and \
         re-exporting it from the host file."
    ))
}

#[derive(Default)]
struct WireApp {
    modified: Vec<std::path::PathBuf>,
    next_steps: Vec<String>,
}

/// Inject `Router::<crate::router::Route> {}` and any
/// `crate::state::{store_snake}::provide_{store_snake}()` calls into the
/// project's `App` component (in src/main.rs or src/lib.rs) the first time
/// a scaffold run emits a Screen / LoginScreen or a ClientStore. Idempotent
/// against re-runs: if a Router invocation or the specific provide_* call
/// is already textually present anywhere in the file, we skip it.
///
/// We rely on the `dx new` shape:
///     #[component]
///     fn App() -> Element {
///         rsx! { ... }
///     }
/// — found by scanning for `fn App(` and brace-balancing the body. If the
/// file doesn't match (no App fn, or rsx! macro not where expected) we fall
/// back to surfacing a next_steps hint so the user wires it manually.
fn wire_app_if_needed(doc: &DslDoc, crate_root: &Path) -> Result<WireApp, String> {
    let needs_router = !doc.screens.is_empty() || !doc.login_screens.is_empty();
    let store_snakes: Vec<String> = doc
        .client_stores
        .iter()
        .map(|cs| cs.name.to_snake_case())
        .collect();
    if !needs_router && store_snakes.is_empty() {
        return Ok(WireApp::default());
    }

    let Some(path) = scaffold::find_crate_root_file(crate_root) else {
        // No main.rs / lib.rs to wire — bootstrap_router_if_needed already
        // surfaces the Router mounting hint, so we add provide_* hints here.
        let mut out = WireApp::default();
        for s in &store_snakes {
            out.next_steps.push(format!(
                "(crate root: missing) — add a `fn App()` that calls `crate::state::{s}::provide_{s}()` before rendering any screen that uses `use_{s}()`"
            ));
        }
        return Ok(out);
    };
    let original = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let rel_path = relative_to_crate(crate_root, &path);

    let mut out = WireApp::default();
    let mut text = original.clone();

    // Locate `fn App(` and its body. If absent, fall back to hints — the
    // dx-new template always emits one, so absence means the user is in a
    // hand-rolled shape we shouldn't rewrite.
    let app_body_range = match find_fn_body_range(&text, "App") {
        Some(r) => r,
        None => {
            if needs_router {
                out.next_steps.push(format!(
                    "{rel_path}: no `fn App()` found — mount the router manually with `Router::<crate::router::Route> {{}}` in your top-level component"
                ));
            }
            for s in &store_snakes {
                out.next_steps.push(format!(
                    "{rel_path}: call `crate::state::{s}::provide_{s}()` in your App component before rendering any screen that uses `use_{s}()`"
                ));
            }
            return Ok(out);
        }
    };

    // 1. Inject any missing `provide_*` calls at the top of the App body.
    //    Idempotent: skip if the literal `provide_{snake}()` is anywhere in
    //    the file (App body or otherwise — user may have wired it manually).
    let mut to_provide: Vec<String> = Vec::new();
    for s in &store_snakes {
        if !text.contains(&format!("provide_{s}()")) {
            to_provide.push(s.clone());
        }
    }
    if !to_provide.is_empty() {
        // Indent matches the first non-empty line inside the body, or four
        // spaces as a fallback.
        let indent =
            detect_body_indent(&text, app_body_range.clone()).unwrap_or_else(|| "    ".into());
        let mut insertion = String::new();
        for s in &to_provide {
            insertion.push_str(&format!("{indent}crate::state::{s}::provide_{s}();\n"));
        }
        // Splice in just after the opening `{` of the App body. If the next
        // byte is a newline, insert *after* it so the let lands on its own
        // line; otherwise prepend a `\n` so the let doesn't glue onto the
        // same line as `{`.
        let after_brace = app_body_range.start + 1;
        let (insert_at, payload) = if text.as_bytes().get(after_brace).copied() == Some(b'\n') {
            (after_brace + 1, insertion)
        } else {
            (after_brace, format!("\n{insertion}"))
        };
        text.insert_str(insert_at, &payload);
    }

    // 2. Inject Router::<crate::router::Route> {} as the first child of the
    //    App body's rsx! block, if any. Skip when Router is already mounted.
    if needs_router && !text.contains("Router::<") {
        // Re-locate the body — its range may have shifted by `provide_*`
        // insertions above.
        if let Some(body) = find_fn_body_range(&text, "App")
            && let Some(rsx_inner) = find_rsx_inner_range(&text, body.clone())
        {
            let indent =
                detect_rsx_indent(&text, rsx_inner.clone()).unwrap_or_else(|| "        ".into());
            // rsx_inner.start is the byte index of the rsx body's opening
            // `{`. Insert AFTER it so the Router lands as a child of the
            // rsx block rather than between `rsx!` and its `{`.
            let payload = format!("\n{indent}Router::<crate::router::Route> {{}}");
            text.insert_str(rsx_inner.start + 1, &payload);
        } else if needs_router {
            // Best-effort line number of the App fn so the user can jump there.
            let app_line = app_line_number(&text, app_body_range.start);
            out.next_steps.push(format!(
                "{rel_path}:{app_line}: couldn't find an `rsx! {{ ... }}` block inside `fn App()` — mount the router manually with `Router::<crate::router::Route> {{}}`"
            ));
        }
    }

    if text != original {
        std::fs::write(&path, text).map_err(|e| e.to_string())?;
        out.modified.push(path);
    }
    Ok(out)
}

/// Render a path relative to the crate root with forward slashes — used in
/// `next_steps` strings so users can paste them directly into editors.
mod text_edit;
use text_edit::*;

// ---------- per-primitive generators ----------

fn render(name: &str, tpl: &str, ctx: minijinja::Value) -> Result<String, String> {
    let mut env = Environment::new();
    env.add_template(name, tpl).map_err(|e| e.to_string())?;
    env.get_template(name)
        .map_err(|e| e.to_string())?
        .render(ctx)
        .map_err(|e| e.to_string())
}

fn write_component_file(
    crate_root: &Path,
    snake: &str,
    body: String,
) -> Result<ScaffoldResult, String> {
    write_module_file(crate_root, "src/components", snake, body)
}

fn write_module_file(
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

fn write_module_file_with_cfg(
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
    // Components are referenced by name (`use crate::components::Foo;`), so
    // the wildcard re-export is always "used" — no need to shield with
    // `#![allow(unused_imports)]`. Server fns / state stores may have
    // alongside-the-fact items (delete_*, etc.) that aren't called yet, so
    // those keep the shield.
    let allow_unused = subdir != "src/components";
    let upsert = upsert_mod_entry(&mod_rs, snake, cfg_attr, allow_unused)?;
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

fn field_initial(ty: &str) -> &'static str {
    match ty {
        "checkbox" => "false",
        "number" => "0i64",
        _ => "String::new()",
    }
}

fn generate_form(crate_root: &Path, f: &DslForm) -> Result<ScaffoldResult, String> {
    let pascal = f.name.to_pascal_case();
    let snake = f.name.to_snake_case();

    let snake_field_names: Vec<String> =
        f.fields.iter().map(|fd| fd.name.to_snake_case()).collect();
    let snapshots = snake_field_names
        .iter()
        .map(|n| format!("                let {n}_v = {n}();"))
        .collect::<Vec<_>>()
        .join("\n");
    let arg_call = snake_field_names
        .iter()
        .map(|n| format!("{n}_v"))
        .collect::<Vec<_>>()
        .join(", ");
    let resets = f
        .fields
        .iter()
        .map(|fd| {
            let n = fd.name.to_snake_case();
            let init = field_initial(&fd.ty);
            format!("                        {n}.set({init});")
        })
        .collect::<Vec<_>>()
        .join("\n");

    let on_submit_body = match (&f.on_submit, &f.feeds_into) {
        (Some(h), Some(_)) => {
            let h = h.to_snake_case();
            let mut out = String::new();
            if !snapshots.is_empty() {
                out.push_str(&snapshots);
                out.push('\n');
            }
            out.push_str(&format!(
                "                spawn(async move {{\n                    if {h}({arg_call}).await.is_ok() {{\n"
            ));
            if !resets.is_empty() {
                out.push_str(&resets);
                out.push('\n');
            }
            out.push_str(
                "                        *version.write() += 1;\n                    }\n                });",
            );
            out
        }
        (Some(h), None) => {
            let h = h.to_snake_case();
            let mut out = String::new();
            if !snapshots.is_empty() {
                out.push_str(&snapshots);
                out.push('\n');
            }
            out.push_str(&format!(
                "                spawn(async move {{\n                    let _ = {h}({arg_call}).await;\n                }});"
            ));
            out
        }
        (None, Some(_)) => {
            "                // TODO submit handler\n                *version.write() += 1;"
                .to_string()
        }
        (None, None) => "                // TODO submit handler".to_string(),
    };

    let fields_ctx: Vec<_> = f
        .fields
        .iter()
        .map(|fd| {
            let initial = field_initial(&fd.ty);
            let input_type = match fd.ty.as_str() {
                "email" => "email",
                "password" => "password",
                "number" => "number",
                "checkbox" => "checkbox",
                "textarea" => "text",
                _ => "text",
            };
            let tag = if fd.ty == "textarea" {
                "textarea"
            } else {
                "input"
            };
            let validation = fd.validation.clone().unwrap_or_default();
            context! {
                name => fd.name.to_snake_case(),
                label => humanize(&fd.name),
                input_type => input_type,
                tag => tag,
                initial => initial,
                validation => validation,
            }
        })
        .collect();
    let feeds_into_snake = f.feeds_into.as_ref().map(|s| s.to_snake_case());
    let handler = f.on_submit.as_ref().map(|s| s.to_snake_case());
    let needs_handler_import = handler.is_some();
    let body = render(
        "form",
        FORM_TPL,
        context! {
            pascal => pascal.clone(),
            fields => fields_ctx,
            on_submit_body => on_submit_body,
            handler => handler,
            needs_handler_import => needs_handler_import,
            feeds_into_snake => feeds_into_snake,
        },
    )?;
    let mut r = write_component_file(crate_root, &snake, body)?;
    r.next_steps.push(format!(
        "import the form: `use crate::components::{pascal};`"
    ));
    if let Some(target) = &f.feeds_into {
        let t = target.to_snake_case();
        r.next_steps.push(format!(
            "render `{pascal}` inside the same parent that calls `provide_{t}_version()` so both share the version signal"
        ));
    }
    Ok(r)
}

fn generate_list(
    crate_root: &Path,
    l: &DslList,
    versioned: bool,
) -> Result<ScaffoldResult, String> {
    let pascal = l.name.to_pascal_case();
    let snake = l.name.to_snake_case();
    let endpoint = l.endpoint.to_snake_case();
    let body = render(
        "list",
        LIST_TPL,
        context! {
            pascal => pascal.clone(),
            snake => snake.clone(),
            endpoint => endpoint,
            item_type => l.item_type.clone(),
            versioned => versioned,
        },
    )?;
    let mut r = write_component_file(crate_root, &snake, body)?;
    if versioned {
        r.next_steps.push(format!(
            "call `crate::components::{snake}::provide_{snake}_version()` in the screen that hosts this list (and any forms feeding into it) before rendering them"
        ));
    }
    Ok(r)
}

fn generate_table(crate_root: &Path, t: &DslTable) -> Result<ScaffoldResult, String> {
    let pascal = t.name.to_pascal_case();
    let snake = t.name.to_snake_case();
    let endpoint = t.endpoint.to_snake_case();
    let cols: Vec<_> = t
        .columns
        .iter()
        .map(|c| {
            context! { name => c.name.clone(), label => c.label.clone() }
        })
        .collect();
    let body = render(
        "table",
        TABLE_TPL,
        context! {
            pascal => pascal.clone(),
            snake => snake.clone(),
            endpoint => endpoint,
            item_type => t.item_type.clone(),
            columns => cols,
        },
    )?;
    write_component_file(crate_root, &snake, body)
}

fn generate_signal(crate_root: &Path, s: &DslSignal) -> Result<ScaffoldResult, String> {
    let snake = s.name.to_snake_case();
    let body = render(
        "signal",
        SIGNAL_TPL,
        context! {
            snake => snake.clone(),
            ty => s.ty.clone(),
            initial => s.initial.clone(),
        },
    )?;
    write_module_file(crate_root, "src/signals", &snake, body)
}

fn generate_socket(crate_root: &Path, s: &DslSocket) -> Result<ScaffoldResult, String> {
    let snake = s.name.to_snake_case();
    let pascal = s.name.to_pascal_case();
    let upper = snake.to_uppercase();
    let body = render(
        "socket",
        SOCKET_TPL,
        context! {
            pascal => pascal,
            snake => snake.clone(),
            upper => upper,
            url => s.url.clone(),
        },
    )?;
    write_module_file(crate_root, "src/sockets", &snake, body)
}

fn generate_feed(crate_root: &Path, f: &DslFeed) -> Result<ScaffoldResult, String> {
    let pascal = f.name.to_pascal_case();
    let snake = f.name.to_snake_case();
    let socket_snake = f.socket.to_snake_case();
    let socket_pascal = f.socket.to_pascal_case();
    let body = render(
        "feed",
        FEED_TPL,
        context! {
            pascal => pascal,
            snake => snake.clone(),
            socket => socket_snake,
            socket_pascal => socket_pascal,
            item_type => f.item_type.clone(),
        },
    )?;
    write_component_file(crate_root, &snake, body)
}

fn generate_model(crate_root: &Path, m: &DslModel) -> Result<ScaffoldResult, String> {
    let pascal = m.name.to_pascal_case();
    let snake = m.name.to_snake_case();

    let defaults = ["Debug", "Clone", "PartialEq", "Serialize", "Deserialize"];
    let mut derives: Vec<String> = defaults.iter().map(|s| (*s).to_string()).collect();
    for extra in &m.derives {
        let t = extra.trim();
        if !t.is_empty() && !derives.iter().any(|d| d == t) {
            derives.push(t.to_string());
        }
    }
    let derives_str = derives.join(", ");

    let fields_ctx: Vec<_> = m
        .fields
        .iter()
        .map(|f| {
            context! {
                name => f.name.to_snake_case(),
                ty => f.ty.clone(),
                optional => f.optional,
            }
        })
        .collect();

    let body = render(
        "model",
        MODEL_TPL,
        context! {
            pascal => pascal,
            derives => derives_str,
            fields => fields_ctx,
        },
    )?;
    write_module_file(crate_root, "src/model", &snake, body)
}

fn generate_session(crate_root: &Path, s: &DslSessionState) -> Result<ScaffoldResult, String> {
    let snake = s.name.to_snake_case();
    let body = render(
        "session",
        SESSION_TPL,
        context! {
            snake => snake.clone(),
            user_type => s.user_type.clone(),
        },
    )?;
    write_module_file(crate_root, "src/auth", &snake, body)
}

async fn generate_login_screen(
    state: &Arc<State>,
    crate_root: &Path,
    ls: &DslLoginScreen,
    project_root: Option<&str>,
) -> Result<ScaffoldResult, String> {
    let pascal = ls.name.to_pascal_case();
    let snake = ls.name.to_snake_case();
    let body = render(
        "login",
        LOGIN_TPL,
        context! {
            pascal => pascal.clone(),
            redirect => ls.redirect_on_success.clone(),
        },
    )?;
    let mut r = write_component_file(crate_root, &snake, body)?;
    let route = scaffold::create_route(
        state,
        CreateRouteParams {
            path: ls.route.clone(),
            component: pascal.clone(),
            router_file: None,
            project_root: project_root.map(str::to_owned),
            params: Vec::new(),
            import_path: Some("crate::components".to_string()),
        },
    )
    .await?;
    merge(&mut r, route);
    Ok(r)
}

fn generate_protected_route(
    crate_root: &Path,
    pr: &DslProtectedRoute,
    session_names: &BTreeSet<String>,
) -> Result<ScaffoldResult, String> {
    let pascal = pr.name.to_pascal_case();
    let snake = pr.name.to_snake_case();
    let session_snake = match &pr.requires {
        Some(s) => Some(s.to_snake_case()),
        None => session_names.iter().next().cloned(),
    };
    let body = render(
        "protected",
        PROTECTED_TPL,
        context! {
            pascal => pascal,
            redirect_to => pr.redirect_to.clone(),
            session_snake => session_snake.clone(),
        },
    )?;
    let mut r = write_component_file(crate_root, &snake, body)?;
    if session_snake.is_some() {
        r.next_steps.push(
            "make sure the SessionState's `provide_*` is called above any route wrapped by this guard".into(),
        );
    } else {
        r.next_steps.push(
            "no SessionState in the doc — wire your own session signal where the guard reads it"
                .into(),
        );
    }
    Ok(r)
}

/// Render a screen's source body without writing. Shared between
/// `generate_screen` (which writes) and `plan_dsl` (which populates dry-run
/// previews so agents can inspect the output before committing).
fn build_screen_body(
    crate_root: &Path,
    sc: &DslScreen,
    client_stores: &[DslClientStore],
) -> Result<String, String> {
    let pascal = sc.name.to_pascal_case();
    let snake = sc.name.to_snake_case();
    let wrap_pascal = sc.wrap_with.as_ref().map(|w| w.to_pascal_case());
    match &sc.template {
        None => render(
            "screen",
            SCREEN_TPL,
            context! {
                pascal => pascal.clone(),
                snake => snake.clone(),
                wrap_pascal => wrap_pascal.clone(),
                root_class => default_screen_class(&snake),
                store_snake => None::<String>,
            },
        ),
        Some(t) => render_screen_template(
            crate_root,
            &pascal,
            &snake,
            wrap_pascal.as_deref(),
            client_stores,
            t,
        ),
    }
}

async fn generate_screen(
    state: &Arc<State>,
    crate_root: &Path,
    sc: &DslScreen,
    client_stores: &[DslClientStore],
    project_root: Option<&str>,
) -> Result<ScaffoldResult, String> {
    let pascal = sc.name.to_pascal_case();
    let snake = sc.name.to_snake_case();
    let wrap_pascal = sc.wrap_with.as_ref().map(|w| w.to_pascal_case());

    let body = build_screen_body(crate_root, sc, client_stores)?;
    // Locate the first `rsx!` macro in the generated body so the response can
    // point the agent straight at the markup it'll most likely want to edit.
    // The line number is computed pre-write and matches the on-disk file
    // because we never re-flow the body between here and the write.
    let rsx_line = first_rsx_line(&body);
    let mut r = write_component_file(crate_root, &snake, body)?;
    if let Some(line) = rsx_line {
        r.next_steps.push(format!(
            "customize the markup in `src/components/{snake}.rs:{line}` (rsx! block)"
        ));
    }
    if let Some(w) = &wrap_pascal {
        r.next_steps.push(format!(
            "ensure `{w}` is exported from `crate::components` (e.g. emitted by a `protected_routes` entry or a hand-written component)"
        ));
    }
    let route = scaffold::create_route(
        state,
        CreateRouteParams {
            path: sc.route.clone(),
            component: pascal,
            router_file: None,
            project_root: project_root.map(str::to_owned),
            params: sc.route_params.clone(),
            // Screens always live under `crate::components` — auto-add the
            // matching `use` so the Routable derive can resolve the variant.
            import_path: Some("crate::components".to_string()),
        },
    )
    .await?;
    merge(&mut r, route);
    Ok(r)
}

/// Default root-element class for a screen body: `"screen {snake}"`. The
/// helper exists so the literal string lives in one place; user-supplied
/// `template.class` overrides this verbatim.
fn default_screen_class(snake: &str) -> String {
    format!("screen {snake}")
}

/// Find the line number (1-based) of the first `rsx!` macro invocation in a
/// generated source body. Used to point the agent at the markup block in
/// next_steps hints. Returns None when the body has no rsx! (shouldn't happen
/// for a Screen template but kept as a guard).
fn first_rsx_line(body: &str) -> Option<usize> {
    body.lines()
        .enumerate()
        .find(|(_, l)| l.contains("rsx!"))
        .map(|(i, _)| i + 1)
}

fn render_screen_template(
    crate_root: &Path,
    pascal: &str,
    snake: &str,
    wrap_pascal: Option<&str>,
    client_stores: &[DslClientStore],
    t: &DslScreenTemplate,
) -> Result<String, String> {
    match t.kind.as_str() {
        "empty" => {
            // Wire the ClientStore context when the template names a store
            // (the body stays empty — the user fills it in).
            let store_snake = if let Some(store_ref) = &t.store {
                let snake_ref = store_ref.to_snake_case();
                let exists = client_stores
                    .iter()
                    .any(|cs| cs.name.to_snake_case() == snake_ref);
                if !exists {
                    return Err(format!(
                        "screen {pascal:?} kind=empty references unknown client_store {store_ref:?}; declare it under client_stores"
                    ));
                }
                Some(snake_ref)
            } else {
                None
            };
            let root_class = t
                .class
                .clone()
                .unwrap_or_else(|| default_screen_class(snake));
            let body_empty = match t.body.as_deref() {
                Some("empty") | Some("stub") => true,
                None => false,
                Some(other) => {
                    return Err(format!(
                        "screen {pascal:?} kind=empty: `body` must be \"empty\" or \"stub\" (or omitted), got {other:?}"
                    ));
                }
            };
            render(
                "screen",
                SCREEN_TPL,
                context! {
                    pascal => pascal,
                    snake => snake,
                    wrap_pascal => wrap_pascal,
                    root_class => root_class,
                    store_snake => store_snake,
                    body_empty => body_empty,
                },
            )
        }
        "resource_list" => {
            // When CRUD ctx is attached (resource-synthesized), emit the rich
            // table with edit/delete actions. Otherwise fall back to the
            // simple list ladder for user-authored cases.
            if let Some(crud) = &t.crud {
                return render_resource_crud_list(crate_root, pascal, snake, wrap_pascal, crud);
            }
            let endpoint = t
                .endpoint
                .as_ref()
                .ok_or_else(|| {
                    format!("screen {pascal:?} template kind=resource_list requires `endpoint`")
                })?
                .to_snake_case();
            render(
                "screen_resource_list",
                SCREEN_RESOURCE_LIST_TPL,
                context! {
                    pascal => pascal,
                    snake => snake,
                    wrap_pascal => wrap_pascal,
                    endpoint => endpoint,
                },
            )
        }
        "resource_edit_form" => {
            let crud = t.crud.as_ref().ok_or_else(|| {
                format!(
                    "screen {pascal:?} kind=resource_edit_form is an internal template kind \
                     emitted by `resources:`; it cannot be used directly from a user-authored screen"
                )
            })?;
            render_resource_edit_form(pascal, snake, wrap_pascal, t, crud)
        }
        "resource_form" => {
            let submit = t
                .on_submit
                .as_ref()
                .or(t.endpoint.as_ref())
                .ok_or_else(|| {
                    format!(
                        "screen {pascal:?} template kind=resource_form requires `on_submit` or `endpoint`"
                    )
                })?
                .to_snake_case();
            let fields_ctx: Vec<_> = t
                .fields
                .iter()
                .map(|fd| {
                    let is_bool = fd.ty == "checkbox" || fd.rust_type.as_deref() == Some("bool");
                    let initial = if is_bool {
                        "false".to_string()
                    } else {
                        "String::new()".to_string()
                    };
                    let input_type = match fd.ty.as_str() {
                        "email" => "email",
                        "password" => "password",
                        "number" => "number",
                        "checkbox" => "checkbox",
                        "textarea" => "text",
                        _ => "text",
                    };
                    let tag = if fd.ty == "textarea" {
                        "textarea"
                    } else {
                        "input"
                    };
                    context! {
                        name => fd.name.to_snake_case(),
                        label => humanize(&fd.name),
                        input_type => input_type,
                        tag => tag,
                        initial => initial,
                        is_bool => is_bool,
                    }
                })
                .collect();
            let submit_body = resource_form_submit_body(t, &submit);
            render(
                "screen_resource_form",
                SCREEN_RESOURCE_FORM_TPL,
                context! {
                    pascal => pascal,
                    snake => snake,
                    wrap_pascal => wrap_pascal,
                    submit => submit,
                    item_type => t.item_type.clone(),
                    fields => fields_ctx,
                    submit_body => submit_body,
                    redirect_to => t.redirect_to.clone(),
                },
            )
        }
        "client_crud" => render_client_crud_screen(pascal, snake, wrap_pascal, client_stores, t),
        other => Err(format!(
            "unknown screen template kind {other:?} (expected: empty, resource_list, resource_form, client_crud)"
        )),
    }
}

/// Bag of class strings / attribute snippets per design-system preset for the
/// `client_crud` template. Keeps the rendering loop above readable instead of
/// fanning out the same `if styled == "tailwind"` branch six times.
struct ClientCrudStyle {
    form_class: String,
    list_class_override: Option<String>,
    input_class: Option<&'static str>,
    submit_button_class: Option<&'static str>,
    checkbox_class: Option<&'static str>,
    delete_button_class: String,
    extra_h1_attrs: Option<&'static str>,
    extra_li_attrs: Option<&'static str>,
    extra_label_attrs: Option<&'static str>,
}

impl ClientCrudStyle {
    /// Historical unstyled markup: `class: "add"`, `class: "{snake}-items"`,
    /// `class: "delete"`. Kept as the default so existing apps don't change.
    fn default_unstyled(_snake: &str) -> Self {
        Self {
            form_class: "add".into(),
            list_class_override: None,
            input_class: None,
            submit_button_class: None,
            checkbox_class: None,
            delete_button_class: "delete".into(),
            extra_h1_attrs: None,
            extra_li_attrs: None,
            extra_label_attrs: None,
        }
    }

    /// Tailwind-classed defaults: small max-w container, neutral colors,
    /// hover/focus states. Deliberately conservative — should look intentional
    /// in any Tailwind project without committing to a theme.
    fn tailwind() -> Self {
        Self {
            form_class: "flex gap-2 mb-4".into(),
            list_class_override: Some("space-y-2".into()),
            input_class: Some(
                "flex-1 px-3 py-2 border border-gray-300 rounded-md focus:outline-none focus:ring-2 focus:ring-blue-500",
            ),
            submit_button_class: Some(
                "px-4 py-2 bg-blue-600 text-white rounded-md hover:bg-blue-700 focus:outline-none focus:ring-2 focus:ring-blue-500",
            ),
            checkbox_class: Some("h-4 w-4 text-blue-600 rounded border-gray-300"),
            delete_button_class: "text-red-600 hover:text-red-800 text-sm font-medium".into(),
            extra_h1_attrs: Some("class: \"text-2xl font-semibold mb-4\", "),
            extra_li_attrs: Some(
                " class: \"flex items-center gap-3 p-2 bg-white border border-gray-200 rounded-md\",",
            ),
            extra_label_attrs: Some("class: \"flex-1\", "),
        }
    }

    fn h1_attrs(&self) -> &str {
        self.extra_h1_attrs.unwrap_or("")
    }

    fn li_attrs(&self) -> &str {
        self.extra_li_attrs.unwrap_or("")
    }

    fn label_span_attrs(&self) -> &str {
        self.extra_label_attrs.unwrap_or("")
    }

    fn list_class(&self, snake: &str) -> String {
        match &self.list_class_override {
            Some(s) => s.clone(),
            None => format!("{snake}-items"),
        }
    }
}

fn render_client_crud_screen(
    pascal: &str,
    snake: &str,
    wrap_pascal: Option<&str>,
    client_stores: &[DslClientStore],
    t: &DslScreenTemplate,
) -> Result<String, String> {
    let store_ref = t.store.as_deref().ok_or_else(|| {
        format!("screen {pascal:?} kind=client_crud requires `store:` (a client_stores entry name)")
    })?;
    let store_snake = store_ref.to_snake_case();
    let store_cfg = client_stores
        .iter()
        .find(|cs| cs.name.to_snake_case() == store_snake)
        .ok_or_else(|| {
            format!(
                "screen {pascal:?} references unknown client_store {store_ref:?}; declare it under client_stores"
            )
        })?;
    let item_type = t
        .item_type
        .clone()
        .or_else(|| Some(store_cfg.item_type.clone()))
        .ok_or_else(|| format!("screen {pascal:?} kind=client_crud requires `item_type`"))?;
    let label_field = t
        .label_field
        .as_deref()
        .ok_or_else(|| format!("screen {pascal:?} kind=client_crud requires `label_field`"))?
        .to_snake_case();
    let checkbox_field = t.checkbox_field.as_deref().map(|s| s.to_snake_case());
    let id_field = store_cfg
        .id_field
        .as_deref()
        .ok_or_else(|| {
            format!(
                "screen {pascal:?} kind=client_crud requires the referenced client_store {store_ref:?} to declare `id_field` (delete/checkbox actions key off it)"
            )
        })?
        .to_snake_case();
    let id_type = store_cfg.id_type.clone().unwrap_or_else(|| "i64".into());
    let auto_id = store_cfg.auto_id.unwrap_or(false);
    // For integer ids we emit `1i64` etc. so the type of `next_id` is fixed
    // even before the first push. Non-integer id types fall back to bare `1`.
    let id_type_suffix = match id_type.as_str() {
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128"
        | "usize" => id_type.to_string(),
        _ => String::new(),
    };
    // With auto_id on, the store owns the allocator — the screen doesn't need
    // its own next_id signal. Without it (and with a primitive integer id),
    // the screen falls back to the historical local-allocator scaffold.
    let has_id = !id_type_suffix.is_empty() && !auto_id;
    let needs_model_import = store_cfg.item_type.to_snake_case() == item_type.to_snake_case();
    let humanized = humanize(&item_type);

    // Pick the styled preset (currently only `tailwind`). Unknown values
    // are rejected so users find typos here rather than at the markup level.
    let style = match t.styled.as_deref() {
        None => ClientCrudStyle::default_unstyled(snake),
        Some("tailwind") => ClientCrudStyle::tailwind(),
        Some(other) => {
            return Err(format!(
                "screen {pascal:?} kind=client_crud: unknown `template.styled` value {other:?} (expected: \"tailwind\" or omit)"
            ));
        }
    };

    // Render the inner rsx body programmatically — the surrounding wrapper
    // (h1 / wrap_with / div) is filled in by CLIENT_CRUD_SCREEN_TPL.
    let mut body = String::new();
    let ind = if wrap_pascal.is_some() {
        "                "
    } else {
        "            "
    };
    body.push_str(&format!(
        "{ind}h1 {{ {h1_attrs}\"{pascal}\" }}\n",
        h1_attrs = style.h1_attrs()
    ));
    // "Add" form
    body.push_str(&format!(
        "{ind}form {{ class: \"{form_cls}\",\n",
        form_cls = style.form_class
    ));
    body.push_str(&format!("{ind}    onsubmit: move |evt: FormEvent| {{\n"));
    body.push_str(&format!("{ind}        evt.prevent_default();\n"));
    body.push_str(&format!("{ind}        let value = draft();\n"));
    body.push_str(&format!("{ind}        if value.is_empty() {{ return; }}\n"));
    if has_id {
        body.push_str(&format!("{ind}        let id = next_id();\n"));
        body.push_str(&format!("{ind}        *next_id.write() += 1;\n"));
    }
    let push_call = if auto_id { "push_new" } else { "push" };
    body.push_str(&format!("{ind}        store.{push_call}({item_type} {{\n"));
    if has_id {
        body.push_str(&format!("{ind}            {id_field}: id,\n"));
    }
    body.push_str(&format!("{ind}            {label_field}: value,\n"));
    body.push_str(&format!("{ind}            ..Default::default()\n"));
    body.push_str(&format!("{ind}        }});\n"));
    body.push_str(&format!("{ind}        draft.set(String::new());\n"));
    body.push_str(&format!("{ind}    }},\n"));
    body.push_str(&format!("{ind}    input {{\n"));
    body.push_str(&format!("{ind}        r#type: \"text\",\n"));
    if let Some(cls) = style.input_class {
        body.push_str(&format!("{ind}        class: \"{cls}\",\n"));
    }
    body.push_str(&format!("{ind}        value: \"{{draft()}}\",\n"));
    body.push_str(&format!("{ind}        placeholder: \"New {humanized}\",\n"));
    body.push_str(&format!(
        "{ind}        oninput: move |e| draft.set(e.value()),\n"
    ));
    body.push_str(&format!("{ind}    }}\n"));
    if let Some(cls) = style.submit_button_class {
        body.push_str(&format!(
            "{ind}    button {{ r#type: \"submit\", class: \"{cls}\", \"Add\" }}\n"
        ));
    } else {
        body.push_str(&format!(
            "{ind}    button {{ r#type: \"submit\", \"Add\" }}\n"
        ));
    }
    body.push_str(&format!("{ind}}}\n"));
    // List
    body.push_str(&format!(
        "{ind}ul {{ class: \"{list_cls}\",\n",
        list_cls = style.list_class(snake)
    ));
    body.push_str(&format!(
        "{ind}    for item in store.items.read().iter() {{\n"
    ));
    body.push_str(&format!(
        "{ind}        li {{ key: \"{{item.{id_field}}}\",{li_attrs}\n",
        li_attrs = style.li_attrs(),
    ));
    if let Some(cb) = &checkbox_field {
        body.push_str(&format!("{ind}            input {{\n"));
        body.push_str(&format!("{ind}                r#type: \"checkbox\",\n"));
        if let Some(cls) = style.checkbox_class {
            body.push_str(&format!("{ind}                class: \"{cls}\",\n"));
        }
        // Idiomatic Dioxus 0.7 boolean attribute: bind the bool field
        // directly, not its formatted-string form.
        body.push_str(&format!("{ind}                checked: item.{cb},\n"));
        body.push_str(&format!("{ind}                oninput: {{\n"));
        body.push_str(&format!(
            "{ind}                    let id = item.{id_field}.clone();\n"
        ));
        body.push_str(&format!("{ind}                    move |_| {{\n"));
        body.push_str(&format!(
            "{ind}                        let id = id.clone();\n"
        ));
        body.push_str(&format!(
            "{ind}                        store.update_by_id(id, |t| t.{cb} = !t.{cb});\n"
        ));
        body.push_str(&format!("{ind}                    }}\n"));
        body.push_str(&format!("{ind}                }},\n"));
        body.push_str(&format!("{ind}            }}\n"));
    }
    body.push_str(&format!(
        "{ind}            span {{ {span_attrs}\"{{item.{label_field}}}\" }}\n",
        span_attrs = style.label_span_attrs(),
    ));
    body.push_str(&format!(
        "{ind}            button {{ class: \"{del_cls}\",\n",
        del_cls = style.delete_button_class,
    ));
    body.push_str(&format!("{ind}                onclick: {{\n"));
    body.push_str(&format!(
        "{ind}                    let id = item.{id_field}.clone();\n"
    ));
    body.push_str(&format!("{ind}                    move |_| {{\n"));
    body.push_str(&format!(
        "{ind}                        let id = id.clone();\n"
    ));
    body.push_str(&format!(
        "{ind}                        store.remove_by_id(id);\n"
    ));
    body.push_str(&format!("{ind}                    }}\n"));
    body.push_str(&format!("{ind}                }},\n"));
    body.push_str(&format!("{ind}                \"Delete\"\n"));
    body.push_str(&format!("{ind}            }}\n"));
    body.push_str(&format!("{ind}        }}\n"));
    body.push_str(&format!("{ind}    }}\n"));
    body.push_str(&format!("{ind}}}"));

    render(
        "client_crud_screen",
        CLIENT_CRUD_SCREEN_TPL,
        context! {
            pascal => pascal,
            snake => snake,
            wrap_pascal => wrap_pascal,
            store_snake => store_snake,
            item_type => item_type,
            needs_model_import => needs_model_import,
            has_id => has_id,
            id_type_suffix => id_type_suffix,
            body => body,
        },
    )
}

/// Locate the Routable enum on disk and return the import path callers can use
/// from a sibling component file (e.g. "crate::Route" when the enum is in
/// main.rs / lib.rs; "crate::router::Route" when in src/router.rs). Returns
/// None when no Routable enum is found, in which case the list template falls
/// back to plain `<a href>` links to avoid emitting un-compilable code.
fn detect_route_import(crate_root: &Path) -> Option<(String, String)> {
    let path = scaffold::find_routable(crate_root)?;
    let src_rel = path.strip_prefix(crate_root.join("src")).ok()?;
    let src = std::fs::read_to_string(&path).ok()?;
    let file = syn::parse_file(&src).ok()?;
    let enum_name = file.items.iter().find_map(|it| match it {
        syn::Item::Enum(e) => {
            let has_routable = e.attrs.iter().any(|a| {
                if !a.path().is_ident("derive") {
                    return false;
                }
                let mut found = false;
                let _ = a.parse_nested_meta(|m| {
                    if m.path.is_ident("Routable") {
                        found = true;
                    }
                    Ok(())
                });
                found
            });
            if has_routable {
                Some(e.ident.to_string())
            } else {
                None
            }
        }
        _ => None,
    })?;
    // Module path from crate root: drop the trailing `.rs`, treat `main` /
    // `lib` as the crate root (no module prefix), otherwise build
    // `crate::a::b::Enum` from the parent dirs + filename stem.
    let stem = src_rel.file_stem()?.to_str()?;
    let parent_components: Vec<String> = src_rel
        .parent()
        .into_iter()
        .flat_map(|p| p.components())
        .filter_map(|c| match c {
            std::path::Component::Normal(n) => n.to_str().map(String::from),
            _ => None,
        })
        .collect();
    let import = if (stem == "main" || stem == "lib") && parent_components.is_empty() {
        format!("crate::{enum_name}")
    } else {
        let mut segs = parent_components;
        segs.push(stem.to_string());
        format!("crate::{}::{}", segs.join("::"), enum_name)
    };
    Some((import, enum_name))
}

fn render_resource_crud_list(
    crate_root: &Path,
    pascal: &str,
    snake: &str,
    wrap_pascal: Option<&str>,
    crud: &CrudCtx,
) -> Result<String, String> {
    let columns: Vec<_> = crud
        .model_fields
        .iter()
        .map(|f| {
            let inner = strip_option(&f.ty).unwrap_or(&f.ty);
            let optional = f.optional || strip_option(&f.ty).is_some();
            // Non-Display fallback: custom types may not impl Display, so use
            // Debug. Users can post-edit if they want a different format.
            let is_primitive = matches!(
                inner,
                "String"
                    | "bool"
                    | "i8"
                    | "i16"
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
                    | "f32"
                    | "f64"
                    | "char"
            );
            let name = f.name.to_snake_case();
            // For Option<T> we want a *value* in the cell, not `Some(...)` /
            // `None` (Debug formatting); reach into the Option and render the
            // inner via Display (or empty string for None).
            let cell = if optional {
                if is_primitive {
                    format!("{{row.{name}.as_ref().map(|v| v.to_string()).unwrap_or_default()}}")
                } else {
                    // Non-Display inner — fall back to Debug of the inner value,
                    // still avoiding the Some(..)/None wrapper.
                    format!("{{row.{name}.as_ref().map(|v| format!(\"{{:?}}\", v)).unwrap_or_default()}}")
                }
            } else if is_primitive {
                format!("{{row.{name}}}")
            } else {
                format!("{{row.{name}:?}}")
            };
            context! {
                name => name,
                label => humanize(&f.name),
                cell => cell,
            }
        })
        .collect();
    // Build SPA-friendly Link expressions when we can resolve the Route enum
    // import path. Fall back to plain `a { href: ... }` when no Routable enum
    // is on disk (no router file yet) — that's at least correct.
    let route_link = detect_route_import(crate_root).map(|(import_path, enum_name)| {
        let new_variant = format!("{}NewScreen", crud.model_pascal);
        let edit_variant = format!("{}EditScreen", crud.model_pascal);
        context! {
            import_path => import_path,
            enum_name => enum_name,
            new_variant => new_variant,
            edit_variant => edit_variant,
            id_field => crud.id_field.clone(),
        }
    });

    render(
        "screen_resource_crud_list",
        SCREEN_RESOURCE_CRUD_LIST_TPL,
        context! {
            pascal => pascal,
            snake => snake,
            wrap_pascal => wrap_pascal,
            list_endpoint => crud.list_endpoint.clone(),
            delete_endpoint => crud.delete_endpoint.clone(),
            new_route => crud.new_route.clone(),
            list_route => crud.list_route.clone(),
            id_field => crud.id_field.clone(),
            humanized => humanize(&crud.model_pascal),
            columns => columns,
            route_link => route_link,
        },
    )
}

fn render_resource_edit_form(
    pascal: &str,
    snake: &str,
    wrap_pascal: Option<&str>,
    t: &DslScreenTemplate,
    crud: &CrudCtx,
) -> Result<String, String> {
    let fields_ctx: Vec<_> = t
        .fields
        .iter()
        .map(|fd| {
            let is_bool = fd.ty == "checkbox" || fd.rust_type.as_deref() == Some("bool");
            let input_type = match fd.ty.as_str() {
                "email" => "email",
                "password" => "password",
                "number" => "number",
                "checkbox" => "checkbox",
                "textarea" => "text",
                _ => "text",
            };
            let tag = if fd.ty == "textarea" {
                "textarea"
            } else {
                "input"
            };
            let signal_init_from_item = signal_init_from_item(fd);
            context! {
                name => fd.name.to_snake_case(),
                label => humanize(&fd.name),
                input_type => input_type,
                tag => tag,
                is_bool => is_bool,
                signal_init_from_item => signal_init_from_item,
            }
        })
        .collect();

    let submit_body = resource_edit_form_submit_body(t, crud);

    render(
        "screen_resource_edit_form",
        SCREEN_RESOURCE_EDIT_FORM_TPL,
        context! {
            pascal => pascal,
            snake => snake,
            wrap_pascal => wrap_pascal,
            model_pascal => crud.model_pascal.clone(),
            id_field => crud.id_field.clone(),
            id_type => crud.id_type.clone(),
            get_endpoint => crud.get_endpoint.clone(),
            update_endpoint => crud.update_endpoint.clone(),
            fields => fields_ctx,
            submit_body => submit_body,
        },
    )
}

/// Build the `use_signal(|| ...)` initializer expression for an edit-form
/// signal pre-populated from a loaded `item: Model`. Branches on the field's
/// rust_type + optional metadata.
fn signal_init_from_item(f: &DslFieldDef) -> String {
    let rust_ty = f.rust_type.as_deref().unwrap_or("String");
    let inner = strip_option(rust_ty).unwrap_or(rust_ty);
    let optional = f.optional || strip_option(rust_ty).is_some();
    let field_name = f.name.to_snake_case();
    let is_bool = inner == "bool";
    let is_string = inner == "String";

    if is_bool {
        return if optional {
            format!("item.{field_name}.unwrap_or(false)")
        } else {
            format!("item.{field_name}")
        };
    }
    if is_string {
        return if optional {
            format!("item.{field_name}.clone().unwrap_or_default()")
        } else {
            format!("item.{field_name}.clone()")
        };
    }
    // Numeric (or unknown): store as String so the input is editable.
    if optional {
        format!("item.{field_name}.map(|v| v.to_string()).unwrap_or_default()")
    } else {
        format!("item.{field_name}.to_string()")
    }
}

/// Build the submit body for the edit form. Preserves the original id and
/// calls the update_* server fn. Navigates to the list route on success.
fn resource_edit_form_submit_body(t: &DslScreenTemplate, crud: &CrudCtx) -> String {
    let indent = "                ";
    let mut out = String::new();
    for f in &t.fields {
        let n = f.name.to_snake_case();
        out.push_str(&format!("{indent}let {n}_v = {n}();\n"));
    }
    out.push_str(&format!("{indent}let id_v = original_id.clone();\n"));
    out.push_str(&format!("{indent}let item = {} {{\n", crud.model_pascal));
    out.push_str(&format!("{indent}    {}: id_v,\n", crud.id_field));
    for f in &t.fields {
        let n = f.name.to_snake_case();
        let val = field_submit_expr(f, &format!("{n}_v"));
        out.push_str(&format!("{indent}    {n}: {val},\n"));
    }
    out.push_str(&format!("{indent}    ..Default::default()\n"));
    out.push_str(&format!("{indent}}};\n"));
    let nav_line = format!("{indent}        nav.push(\"{}\");\n", crud.list_route);
    out.push_str(&format!(
        "{indent}spawn(async move {{\n{indent}    if {}(item).await.is_ok() {{\n{nav_line}{indent}    }}\n{indent}}});",
        crud.update_endpoint
    ));
    out
}

/// "stock_movement" or "StockMovement" → "Stock movement". Used for h1 / link
/// text on the synthesized CRUD screens.
fn humanize(s: &str) -> String {
    let snake = s.to_snake_case();
    let mut out = String::with_capacity(snake.len());
    for (i, ch) in snake.chars().enumerate() {
        if ch == '_' {
            out.push(' ');
        } else if i == 0 {
            for u in ch.to_uppercase() {
                out.push(u);
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Build the rust body that runs inside the form's onsubmit handler.
/// When `item_type` is set we attempt to construct it from the field signals
/// and call the submit fn with it. Otherwise we emit a TODO body.
///
/// Each field's submit-side expression is computed from its
/// `rust_type` + `optional` metadata (populated by `expand_resources` from the
/// source model). This produces compiling code for `String`, `Option<String>`,
/// integer/float (parsed from the String-backed signal), their Option variants,
/// and `bool`.
fn resource_form_submit_body(t: &DslScreenTemplate, submit: &str) -> String {
    let indent = "                ";
    let mut out = String::new();
    let has_item = t.item_type.is_some() && !t.fields.is_empty();

    if !t.fields.is_empty() {
        for f in &t.fields {
            let n = f.name.to_snake_case();
            out.push_str(&format!("{indent}let {n}_v = {n}();\n"));
        }
    }

    if has_item {
        let item_type = t.item_type.as_deref().unwrap();
        out.push_str(&format!("{indent}let item = {item_type} {{\n"));
        // Field assignment driven by the original Rust type when known.
        for f in &t.fields {
            let n = f.name.to_snake_case();
            let val = field_submit_expr(f, &format!("{n}_v"));
            out.push_str(&format!("{indent}    {n}: {val},\n"));
        }
        out.push_str(&format!("{indent}    ..Default::default()\n"));
        out.push_str(&format!("{indent}}};\n"));
        let nav_line = match &t.redirect_to {
            Some(r) => format!("{indent}        nav.push(\"{r}\");\n"),
            None => String::new(),
        };
        out.push_str(&format!(
            "{indent}spawn(async move {{\n{indent}    if {submit}(item).await.is_ok() {{\n{nav_line}{indent}    }}\n{indent}}});"
        ));
    } else if !t.fields.is_empty() {
        let arg_call = t
            .fields
            .iter()
            .map(|f| format!("{}_v", f.name.to_snake_case()))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "{indent}spawn(async move {{\n{indent}    let _ = {submit}({arg_call}).await;\n{indent}}});"
        ));
    } else {
        out.push_str(&format!(
            "{indent}// TODO call {submit}(...). Add `fields:` to the template to scaffold signals + inputs."
        ));
    }

    out
}

/// Build the Rust expression that converts a String-backed (or bool-backed)
/// signal snapshot into the model field's actual type. `signal_var` is the
/// local that already holds the snapshot (e.g. `"name_v"`).
fn field_submit_expr(f: &DslFieldDef, signal_var: &str) -> String {
    let rust_ty = f.rust_type.as_deref().unwrap_or("String");
    let inner = strip_option(rust_ty).unwrap_or(rust_ty);
    let is_numeric = matches!(
        inner,
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
            | "f32"
            | "f64"
    );
    let is_bool = inner == "bool";
    let is_string = inner == "String";

    let optional = f.optional || strip_option(rust_ty).is_some();

    if is_bool {
        // bool-backed signal already holds a bool — no parsing needed.
        return if optional {
            format!("Some({signal_var})")
        } else {
            signal_var.to_string()
        };
    }

    if is_numeric {
        let parse_expr = format!("{signal_var}.parse::<{inner}>().unwrap_or_default()");
        return if optional {
            format!(
                "if {signal_var}.is_empty() {{ None }} else {{ {signal_var}.parse::<{inner}>().ok() }}"
            )
        } else {
            parse_expr
        };
    }

    if is_string {
        return if optional {
            format!("if {signal_var}.is_empty() {{ None }} else {{ Some({signal_var}) }}")
        } else {
            signal_var.to_string()
        };
    }

    // Unknown type — fall back to a parse attempt for non-optional, or a TODO
    // wrapper for optional. The generated file is meant to be edited if the
    // model uses a custom type.
    if optional {
        format!(
            "if {signal_var}.is_empty() {{ None }} else {{ {signal_var}.parse::<{inner}>().ok() }}"
        )
    } else {
        format!("{signal_var}.parse::<{inner}>().unwrap_or_default()")
    }
}

/// If `ty` is an `Option<T>` (textually, with optional whitespace) returns `Some("T")`;
/// otherwise returns `None`. Naive, but adequate for the type strings we emit
/// from models (e.g. `Option<String>`, `Option<i64>`).
fn strip_option(ty: &str) -> Option<&str> {
    let t = ty.trim();
    let inner = t.strip_prefix("Option<")?.strip_suffix('>')?;
    Some(inner.trim())
}

// ---------- store + resource ----------

fn generate_store(crate_root: &Path, store: &DslStore) -> Result<ScaffoldResult, String> {
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

fn generate_client_store(
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

fn is_primitive_integer_ty(ty: &str) -> bool {
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

#[derive(Debug, Clone)]
pub(super) struct SynthServerFn {
    name: String,
    args: Vec<(String, String)>,
    return_type: String,
    method: &'static str,
    path: String,
    body: String,
}

/// For every `screens:` entry with `template.kind: client_crud`, find the
/// model the screen will construct (via the referenced client_store's
/// `item_type`) and ensure `Default` is in its `derives:` list. The generated
/// body uses `..Default::default()` on the rest of the struct, which silently
/// breaks compilation when the user-authored model only derives the usual
/// `Debug, Clone, Serialize, Deserialize, PartialEq` set.
///
/// Case-insensitive dedup so users who already typed `derives: [Default]`
/// don't end up with `derives: [Default, Default]`.
fn ensure_default_on_client_crud_models(doc: &mut DslDoc) {
    if doc.screens.is_empty() {
        return;
    }
    // Collect (item_type) names from client_crud screens that resolve through
    // a known client_store. Iterate immutably first so we can mutate `models`
    // afterwards without aliasing.
    let mut needs_default: BTreeSet<String> = BTreeSet::new();
    for sc in &doc.screens {
        let Some(t) = sc.template.as_ref() else {
            continue;
        };
        if t.kind != "client_crud" {
            continue;
        }
        let item_type = t.item_type.clone().or_else(|| {
            t.store.as_ref().and_then(|store_ref| {
                let key = store_ref.to_snake_case();
                doc.client_stores
                    .iter()
                    .find(|cs| cs.name.to_snake_case() == key)
                    .map(|cs| cs.item_type.clone())
            })
        });
        if let Some(it) = item_type {
            needs_default.insert(it.to_snake_case());
        }
    }
    for m in &mut doc.models {
        if !needs_default.contains(&m.name.to_snake_case()) {
            continue;
        }
        let has_default = m.derives.iter().any(|d| d.eq_ignore_ascii_case("Default"));
        if !has_default {
            m.derives.push("Default".to_string());
        }
    }
}

/// Companion to [`ensure_default_on_client_crud_models`] for the on-disk case:
/// when a `client_crud` Screen references a model that is *not* declared in
/// the same doc but already exists at `src/model/{snake}.rs`, patch its
/// `#[derive(...)]` line to include `Default`. Returns the list of files
/// modified (empty when no patching was needed).
///
/// Idempotent. Never touches a file outside the conventional model path —
/// callers using a non-standard model layout still need to hand-edit, but
/// the next_steps surface a hint elsewhere in the response.
fn patch_on_disk_models_for_client_crud_default(
    doc: &DslDoc,
    crate_root: &Path,
) -> Result<Vec<std::path::PathBuf>, String> {
    if doc.screens.is_empty() {
        return Ok(Vec::new());
    }
    // Same shape as ensure_default_on_client_crud_models: collect every model
    // type-name a client_crud screen will construct.
    let mut needed: BTreeSet<String> = BTreeSet::new();
    for sc in &doc.screens {
        let Some(t) = sc.template.as_ref() else {
            continue;
        };
        if t.kind != "client_crud" {
            continue;
        }
        let item_type = t.item_type.clone().or_else(|| {
            t.store.as_ref().and_then(|store_ref| {
                let key = store_ref.to_snake_case();
                doc.client_stores
                    .iter()
                    .find(|cs| cs.name.to_snake_case() == key)
                    .map(|cs| cs.item_type.clone())
            })
        });
        if let Some(it) = item_type {
            needed.insert(it);
        }
    }
    // Drop names that the doc itself declares — the in-doc pre-pass already
    // handles those, and re-deriving on a freshly-generated file would just
    // double-fire.
    let in_doc: BTreeSet<String> = doc.models.iter().map(|m| m.name.clone()).collect();
    needed.retain(|n| {
        !in_doc
            .iter()
            .any(|m| m.to_snake_case() == n.to_snake_case())
    });

    let mut modified: Vec<std::path::PathBuf> = Vec::new();
    for type_name in &needed {
        let snake = type_name.to_snake_case();
        let path = crate_root.join(format!("src/model/{snake}.rs"));
        if !path.exists() {
            // Not at the conventional location — leave it alone; the user
            // either keeps the model elsewhere or hasn't authored it yet.
            continue;
        }
        let src =
            std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        if let Some(new_src) = add_default_to_derive(&src, type_name) {
            std::fs::write(&path, &new_src)
                .map_err(|e| format!("write {}: {e}", path.display()))?;
            modified.push(path);
        }
    }
    Ok(modified)
}

/// Locate `#[derive(...)]` on `struct {type_name}` in `src` and append
/// `Default` to its derive list if it isn't already there. Returns the
/// modified source, or `None` when no change is needed (Default is already
/// derived, or the file doesn't carry a matching struct).
///
/// Uses textual splicing on the first `#[derive(...)]` line that precedes
/// the target struct definition. Robust enough for hand-authored model files
/// and the shape we emit ourselves; bails out (and reports no change) if the
/// struct sits without a derive attribute or the file fails to parse.
fn add_default_to_derive(src: &str, type_name: &str) -> Option<String> {
    let file = syn::parse_file(src).ok()?;
    let target = type_name.to_pascal_case();
    let item = file.items.iter().find_map(|it| match it {
        syn::Item::Struct(s) if s.ident == target => Some(s),
        _ => None,
    })?;
    let derive = item.attrs.iter().find(|a| a.path().is_ident("derive"))?;
    let mut has_default = false;
    let _ = derive.parse_nested_meta(|m| {
        if m.path.is_ident("Default") {
            has_default = true;
        }
        Ok(())
    });
    if has_default {
        return None;
    }

    // Splice textually: find the `#[derive(` opener nearest the struct's
    // span (so multi-derive files don't cross-match), then locate the
    // matching `)]` and insert `, Default` before it.
    let struct_line = item.ident.span().start().line;
    // Scan `#[derive(` occurrences and keep the one whose line is closest to
    // (but not after) the struct definition.
    let needle = "#[derive(";
    let mut chosen: Option<usize> = None;
    let mut cursor = 0;
    while let Some(off) = src[cursor..].find(needle) {
        let abs = cursor + off;
        // Line of `abs` byte: count newlines up to abs.
        let line = src[..abs].bytes().filter(|&b| b == b'\n').count() + 1;
        if line <= struct_line {
            chosen = Some(abs);
        } else {
            break;
        }
        cursor = abs + needle.len();
    }
    let open = chosen?;
    // Find the matching `)` for this derive — track paren depth so we don't
    // get fooled by `derive(Foo<Bar>)`.
    let after_open = open + needle.len();
    let mut depth = 1usize;
    let mut close: Option<usize> = None;
    for (i, ch) in src[after_open..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(after_open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let mut out = String::with_capacity(src.len() + 10);
    out.push_str(&src[..close]);
    let trimmed_before = src[..close].trim_end();
    if trimmed_before.ends_with('(') {
        out.push_str("Default");
    } else {
        out.push_str(", Default");
    }
    out.push_str(&src[close..]);
    Some(out)
}

/// Expand each `resources:` entry into the equivalent model + store + 5 server
/// fns + 2 screens. Synth server fns are returned separately because they
/// carry custom bodies that the standard server-fn generator can't emit.
fn expand_resources(doc: &mut DslDoc) -> Result<Vec<SynthServerFn>, String> {
    let resources = std::mem::take(&mut doc.resources);
    let mut synth = Vec::new();
    let mut existing_models: BTreeSet<String> =
        doc.models.iter().map(|m| m.name.to_snake_case()).collect();
    let mut existing_stores: BTreeSet<String> =
        doc.stores.iter().map(|s| s.name.to_snake_case()).collect();

    for r in &resources {
        let res_pascal = r.name.to_pascal_case();
        let res_snake = r.name.to_snake_case();
        let id_field = r.id_field.as_deref().unwrap_or("id").to_snake_case();
        if !r.fields.iter().any(|f| f.name.to_snake_case() == id_field) {
            return Err(format!(
                "resource {:?} must declare its id field {id_field:?} in `fields`",
                r.name
            ));
        }
        let id_type = r
            .fields
            .iter()
            .find(|f| f.name.to_snake_case() == id_field)
            .map(|f| f.ty.clone())
            .unwrap_or_else(|| "i64".into());
        // Explicit override wins; otherwise fall back to the built-in
        // pluralizer. Snake-case the override too so irregular forms still
        // produce valid URL slugs / fn names.
        let plural = r
            .plural
            .as_deref()
            .map(|p| p.to_snake_case())
            .unwrap_or_else(|| pluralize(&res_snake));
        // Default URL slugs are kebab-case (web convention): a model named
        // `StockMovement` lands at `/stock-movements`, not `/stock_movements`.
        // User-supplied `route_base` is taken verbatim.
        let route_base = r
            .route_base
            .clone()
            .unwrap_or_else(|| format!("/{}", plural.replace('_', "-")));
        let store_pascal = format!("{res_pascal}Store");
        let store_snake = format!("{res_snake}_store");

        // 1. Model — synthesize unless already declared. Default is forced
        // (here AND when patching an in-doc pre-declared model below) because
        // resource expansion turns on emit_tests for the store, and the
        // synthesized CRUD tests call `Model::default()`. Without this, tests
        // wouldn't compile.
        if existing_models.insert(res_snake.clone()) {
            let mut derives = r.derives.clone();
            if !derives.iter().any(|d| d == "Default") {
                derives.push("Default".into());
            }
            doc.models.push(DslModel {
                name: res_pascal.clone(),
                fields: r.fields.clone(),
                derives,
            });
        } else if let Some(m) = doc
            .models
            .iter_mut()
            .find(|m| m.name.to_snake_case() == res_snake)
            && !m.derives.iter().any(|d| d == "Default")
        {
            m.derives.push("Default".into());
        }

        // 2. Store — synthesize unless already declared.
        if existing_stores.insert(store_snake.clone()) {
            doc.stores.push(DslStore {
                name: store_pascal.clone(),
                resource: res_pascal.clone(),
                kind: Some("in_memory".into()),
                id_field: Some(id_field.clone()),
                id_type: Some(id_type.clone()),
                // Resource expansion forces Default on the synthesized model,
                // so the auto-generated CRUD tests will compile.
                emit_tests: Some(true),
            });
        }

        // 3. Server fns
        let store_path = format!("crate::state::{store_snake}::{store_pascal}");
        let list_name = format!("list_{plural}");
        let get_name = format!("get_{res_snake}");
        let create_name = format!("create_{res_snake}");
        let update_name = format!("update_{res_snake}");
        let delete_name = format!("delete_{res_snake}");

        let mk_body = |call: &str| {
            format!(
                "    #[cfg(feature = \"server\")]\n    {{\n        return Ok({call});\n    }}\n    #[cfg(not(feature = \"server\"))]\n    {{\n        unreachable!()\n    }}"
            )
        };

        synth.push(SynthServerFn {
            name: list_name.clone(),
            args: vec![],
            return_type: format!("Vec<crate::model::{res_pascal}>"),
            method: "get",
            path: format!("/api{route_base}"),
            body: mk_body(&format!("{store_path}::global().list()")),
        });
        synth.push(SynthServerFn {
            name: get_name.clone(),
            args: vec![("id".into(), id_type.clone())],
            return_type: format!("Option<crate::model::{res_pascal}>"),
            method: "post",
            path: format!("/api{route_base}/get"),
            body: mk_body(&format!("{store_path}::global().get(id)")),
        });
        synth.push(SynthServerFn {
            name: create_name.clone(),
            args: vec![("item".into(), format!("crate::model::{res_pascal}"))],
            return_type: format!("crate::model::{res_pascal}"),
            method: "post",
            path: format!("/api{route_base}"),
            body: mk_body(&format!("{store_path}::global().create(item)")),
        });
        synth.push(SynthServerFn {
            name: update_name.clone(),
            args: vec![("item".into(), format!("crate::model::{res_pascal}"))],
            return_type: format!("Option<crate::model::{res_pascal}>"),
            method: "post",
            path: format!("/api{route_base}/update"),
            body: mk_body(&format!("{store_path}::global().update(item)")),
        });
        synth.push(SynthServerFn {
            name: delete_name.clone(),
            args: vec![("id".into(), id_type.clone())],
            return_type: "bool".into(),
            method: "post",
            path: format!("/api{route_base}/delete"),
            body: mk_body(&format!("{store_path}::global().delete(id)")),
        });

        // 4. Screens: list + new + edit. The edit screen takes an `id`
        //    path-param so the Routable variant has `{ id: <id_type> }`.
        let list_screen = format!("{res_pascal}ListScreen");
        let new_screen = format!("{res_pascal}NewScreen");
        let edit_screen = format!("{res_pascal}EditScreen");
        let new_route = format!("{route_base}/new");
        let non_id_fields: Vec<DslFieldDef> = r
            .fields
            .iter()
            .filter(|f| f.name.to_snake_case() != id_field)
            .map(|f| DslFieldDef {
                name: f.name.clone(),
                ty: field_type_for_model_field(&f.ty),
                validation: None,
                rust_type: Some(f.ty.clone()),
                optional: f.optional,
            })
            .collect();

        let crud = CrudCtx {
            model_pascal: res_pascal.clone(),
            model_fields: r.fields.clone(),
            id_field: id_field.clone(),
            id_type: id_type.clone(),
            list_endpoint: list_name.clone(),
            get_endpoint: get_name.clone(),
            update_endpoint: update_name.clone(),
            delete_endpoint: delete_name.clone(),
            list_route: route_base.clone(),
            new_route: new_route.clone(),
        };

        doc.screens.push(DslScreen {
            name: list_screen,
            route: route_base.clone(),
            wrap_with: None,
            template: Some(DslScreenTemplate {
                kind: "resource_list".into(),
                endpoint: Some(list_name.clone()),
                item_type: Some(res_pascal.clone()),
                on_submit: None,
                redirect_to: None,
                fields: vec![],
                store: None,
                label_field: None,
                checkbox_field: None,
                class: None,
                body: None,
                styled: None,
                crud: Some(crud.clone()),
            }),
            route_params: Vec::new(),
            replace_route: false,
        });
        doc.screens.push(DslScreen {
            name: new_screen,
            route: new_route.clone(),
            wrap_with: None,
            template: Some(DslScreenTemplate {
                kind: "resource_form".into(),
                endpoint: Some(create_name.clone()),
                // Bare model name — the screen template emits the
                // `use crate::model::{item_type};` import itself.
                item_type: Some(res_pascal.clone()),
                on_submit: Some(create_name.clone()),
                redirect_to: Some(route_base.clone()),
                fields: non_id_fields.clone(),
                store: None,
                label_field: None,
                checkbox_field: None,
                class: None,
                body: None,
                styled: None,
                crud: Some(crud.clone()),
            }),
            route_params: Vec::new(),
            replace_route: false,
        });
        doc.screens.push(DslScreen {
            name: edit_screen,
            route: format!("{route_base}/:id/edit"),
            wrap_with: None,
            template: Some(DslScreenTemplate {
                kind: "resource_edit_form".into(),
                endpoint: Some(get_name.clone()),
                item_type: Some(res_pascal.clone()),
                on_submit: Some(update_name.clone()),
                redirect_to: Some(route_base.clone()),
                fields: non_id_fields,
                store: None,
                label_field: None,
                checkbox_field: None,
                class: None,
                body: None,
                styled: None,
                crud: Some(crud),
            }),
            route_params: vec![("id".to_string(), id_type.clone())],
            replace_route: false,
        });
    }
    Ok(synth)
}

/// Map a model field type onto the form-input kind used by the form template.
/// Anything non-trivial defaults to "text" — the user can post-edit.
fn field_type_for_model_field(ty: &str) -> String {
    match ty {
        "bool" => "checkbox".into(),
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize" | "isize" | "f32"
        | "f64" => "number".into(),
        _ => "text".into(),
    }
}

fn pluralize(snake: &str) -> String {
    if snake.ends_with('s')
        || snake.ends_with("sh")
        || snake.ends_with("ch")
        || snake.ends_with('x')
        || snake.ends_with('z')
    {
        format!("{snake}es")
    } else if snake.ends_with('y') {
        let chars: Vec<char> = snake.chars().collect();
        if chars.len() >= 2 && !"aeiou".contains(chars[chars.len() - 2]) {
            let mut s = snake.to_string();
            s.pop();
            s.push_str("ies");
            return s;
        }
        format!("{snake}s")
    } else {
        format!("{snake}s")
    }
}

async fn generate_synth_server_fn(
    state: &Arc<State>,
    crate_root: &Path,
    sf: &SynthServerFn,
    project_root: Option<&str>,
) -> Result<ScaffoldResult, String> {
    // Reuse the fullstack-capable check by detecting through ProjectInfo.
    let project = match project_root {
        Some(root) => crate::project::ProjectInfo::detect(std::path::Path::new(root)),
        None => state.project.lock().await.clone(),
    };
    let active = &project.dioxus_features;
    let fullstack_capable = active.iter().any(|f| f == "fullstack")
        || (active.iter().any(|f| f == "server") && active.iter().any(|f| f == "web"));
    if !fullstack_capable {
        return Err(
            "this project does not have `fullstack` (or `web`+`server`) enabled on the dioxus dep; \
             resource: server fns require a fullstack setup. Run audit_feature_flags for guidance."
                .into(),
        );
    }

    let snake = sf.name.to_snake_case();
    let server_dir = crate_root.join("src/server");
    std::fs::create_dir_all(&server_dir).map_err(|e| e.to_string())?;
    let target = server_dir.join(format!("{snake}.rs"));
    if target.exists() {
        return Err(format!("{} already exists", target.display()));
    }
    let body = render(
        "server_fn_body",
        SERVER_FN_WITH_BODY_TPL,
        context! {
            snake => snake.clone(),
            ret => sf.return_type.clone(),
            method => sf.method,
            path => sf.path.clone(),
            args => sf.args.iter().map(|(n, t)| context!{ name => n.clone(), ty => t.clone() }).collect::<Vec<_>>(),
            body => sf.body.clone(),
            extra_uses => Vec::<String>::new(),
        },
    )?;
    std::fs::write(&target, body).map_err(|e| e.to_string())?;
    let mod_rs = server_dir.join("mod.rs");
    let upsert = upsert_mod_entry(&mod_rs, &snake, None, true)?;
    let (files_created, files_modified) = match upsert {
        ModUpsert::Created => (vec![target, mod_rs], vec![]),
        ModUpsert::Modified => (vec![target], vec![mod_rs]),
        ModUpsert::Unchanged => (vec![target], vec![]),
    };
    Ok(ScaffoldResult {
        files_created,
        files_modified,
        ..Default::default()
    })
}

mod remove;
use remove::*;

mod modify;
use modify::*;

#[cfg(test)]
mod tests;
