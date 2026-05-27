use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use heck::{ToPascalCase, ToSnakeCase};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::state::State;
use crate::tools::scaffold::{
    self, ArgSpec, CreateRouteParams, CreateServerFnParams, PropSpec, ScaffoldResult,
};

use super::cargo::{run_cargo_check, run_cargo_fmt};
use super::cargo_patch::*;
use super::dx_components::{install_dx_components, surface_dx_components_hints};
use super::generate::*;
use super::modify::apply_modify;
use super::plan::plan_dsl;
use super::preflight::{preflight_fullstack, surface_feature_gap_hints};
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
    // `prune_dx_new_starter: true` synthesizes removes for `dx new`'s
    // baseline demo (Hero component + Home variant). Same shape as
    // synthesize_replace_route_removes — keeps doc.remove the single source
    // of truth feeding preflight / dry_run / apply_removes.
    synthesize_dx_new_starter_removes(&mut doc, &crate_root);

    // Pre-compute the set of leaf files `remove:` will delete. Preflight
    // collision checks skip these so a single doc can "remove demo Hero;
    // create my Hero" in one call.
    let to_be_removed = removed_leaf_paths(&doc, &crate_root);

    // Dry-run must never write, so collisions belong in the plan output, not
    // as fatal preflight errors. Force `if_missing` semantics for the dry-run
    // FS check — existing leaves become `collisions` in the plan instead of
    // aborting the call. `disk_aware: true` (dry_run) also relaxes cross-ref
    // checks so a Screen can reference an already-scaffolded store/model/etc.
    // without redeclaring it in the YAML.
    preflight_with_removes(
        &doc,
        &synth_server_fns,
        &crate_root,
        p.if_missing || p.dry_run,
        &to_be_removed,
        p.dry_run,
    )?;

    // Fullstack gate: server-fn writes used to fail half-way through the
    // primitive loop (after models / state / etc. had already been written
    // to disk). Lift the check up here so the call is atomic — if the doc
    // declares any server fn but the project's Cargo.toml doesn't enable
    // fullstack, abort before touching the filesystem. Dry-runs are exempt
    // (no writes happen anyway, and surfacing the issue as a `next_steps`
    // hint via the existing audit path is more useful for plans).
    if !p.dry_run {
        preflight_fullstack(&doc, &synth_server_fns, &crate_root)?;
    }

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
        // Snapshot the registry once for this dry-run (it's loaded fresh from
        // disk so runtime layouts are current).
        let registry = state.registry();
        for sc in &doc.screens {
            let snake = sc.name.to_snake_case();
            let leaf = leaf_for(&crate_root, "src/components", &snake);
            if collision_set.contains(&leaf) {
                continue;
            }
            if let Ok(body) =
                build_screen_body(&crate_root, sc, &doc.client_stores, &registry.layouts)
            {
                plan.previews.insert(leaf, body);
            }
        }
        // `dx_components:` install hints are surfaced in dry-run too so
        // callers can preview the install plan before committing.
        surface_dx_components_hints(&doc, &crate_root, &mut plan);
        // Structured render models for the server-synthesized resource screens
        // (list/new/edit) so a browser client can preview a `resources:` slice
        // it can't reconstruct from the raw doc.
        plan.render_models = super::render_model::build_render_models(&doc, &registry.layouts);
        // Surface the doc-level theme stylesheet in the dry-run, so a proposal
        // review shows the generated themed CSS alongside the screens.
        if let Some(theme_id) = &doc.theme
            && let Some(css) = registry.themes.get(theme_id).and_then(theme_stylesheet)
        {
            let css_path = crate_root.join("assets/theme.css");
            plan.previews.insert(css_path.clone(), css);
            plan.would_create.push(css_path);
        }
        return Ok(plan);
    }

    // Apply removes first so the create steps below don't trip on the files
    // they're about to replace. Errors stop the run before any creates land.
    let mut result = ScaffoldResult::default();
    apply_removes(&doc, &crate_root, &mut result)?;

    // Surface any lingering references to the dx-new demo (`Hero`, the
    // `Home` component def, …) so the caller doesn't ship a project that
    // fails `cargo check` immediately after the prune. Only fires when
    // `prune_dx_new_starter: true` was set; otherwise a silent no-op.
    surface_dx_new_orphans(&doc, &crate_root, &mut result);

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

    // Build a cross-import context once: every Model declared in this doc
    // (snake-case stem) plus every ViewState that materializes an enum. The
    // model generator uses this to auto-emit `use crate::model::{snake}::{Pascal};`
    // when a field type references another sibling type.
    let model_snakes: BTreeSet<String> =
        doc.models.iter().map(|m| m.name.to_snake_case()).collect();
    let view_state_enums: BTreeSet<(String, String)> = doc
        .view_states
        .iter()
        .filter(|vs| !vs.enum_variants.is_empty())
        .map(|vs| (vs.name.to_snake_case(), vs.ty.to_pascal_case()))
        .collect();

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
        // Drop the model's own snake from the cross-import set — a self-
        // reference should not emit a `use` line.
        let self_snake = m.name.to_snake_case();
        let mut models_for_lookup = model_snakes.clone();
        models_for_lookup.remove(&self_snake);
        let imports = crate::tools::dsl::generate::ModelImportCtx {
            models: models_for_lookup,
            view_state_enums: view_state_enums.clone(),
        };
        let r = generate_model(&crate_root, m, &imports)?;
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
                extractors: sf
                    .extractors
                    .iter()
                    .map(|a| ArgSpec {
                        name: a.name.clone(),
                        ty: a.ty.clone(),
                    })
                    .collect(),
                auth_required: sf.auth_required,
                session_cookie: sf.session_cookie.clone(),
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
    // For each client_store, find any client_crud Screen that references it
    // and harvest the `checkbox_field`. The first hit wins — multiple screens
    // pointing at the same store with different checkbox fields is unusual,
    // so we don't try to merge them; the lookup just gives the store its
    // canonical "toggleable bool" field name for the `clear_{field}` helper.
    let checkbox_field_for_store = |cs_name: &str| -> Option<String> {
        let cs_snake = cs_name.to_snake_case();
        for sc in &doc.screens {
            let Some(tpl) = sc.template.as_ref() else {
                continue;
            };
            if tpl.kind != "client_crud" {
                continue;
            }
            let Some(store_ref) = tpl.store.as_deref() else {
                continue;
            };
            if store_ref.to_snake_case() != cs_snake {
                continue;
            }
            if let Some(cb) = tpl.checkbox_field.as_deref() {
                return Some(cb.to_snake_case());
            }
        }
        None
    };
    for cs in &doc.client_stores {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/state", &cs.name),
        ) {
            continue;
        }
        let cb = checkbox_field_for_store(&cs.name);
        let r = generate_client_store(&crate_root, cs, &model_names_for_imports, cb.as_deref())?;
        merge(&mut result, r);
    }

    for vs in &doc.view_states {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/state", &vs.name),
        ) {
            continue;
        }
        let r = generate_view_state(&crate_root, vs)?;
        merge(&mut result, r);
    }

    for g in &doc.staleness_gates {
        // The gate file is `{snake}_gate.rs` — append the suffix to the
        // leaf check so an idempotent re-scaffold finds the right file.
        let gate_leaf = leaf_for(&crate_root, "src/state", &format!("{}_gate", g.name));
        if skip_or_record(&skip, &mut result, gate_leaf) {
            continue;
        }
        let r = generate_staleness_gate(&crate_root, g)?;
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

    if !doc.browser_persistence.is_empty() {
        let model_names: BTreeSet<String> =
            doc.models.iter().map(|m| m.name.to_snake_case()).collect();
        for bp in &doc.browser_persistence {
            if skip_or_record(
                &skip,
                &mut result,
                leaf_for(&crate_root, "src/storage", &bp.name),
            ) {
                continue;
            }
            let r = generate_browser_persistence(&crate_root, bp, &model_names)?;
            merge(&mut result, r);
        }
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

    // Doc-level theme: emit a token-driven stylesheet the scaffolded app mounts.
    if let Some(theme_id) = &doc.theme {
        let reg = state.registry();
        match reg.themes.get(theme_id).and_then(theme_stylesheet) {
            Some(css) => {
                let assets = crate_root.join("assets");
                std::fs::create_dir_all(&assets).map_err(|e| e.to_string())?;
                let css_path = assets.join("theme.css");
                let existed = css_path.exists();
                std::fs::write(&css_path, css).map_err(|e| e.to_string())?;
                if existed {
                    result.files_modified.push(css_path);
                } else {
                    result.files_created.push(css_path);
                }
                result.next_steps.push(format!(
                    "theme `{theme_id}`: mount the generated stylesheet in your App body — `document::Stylesheet {{ href: asset!(\"/assets/theme.css\") }}`"
                ));
            }
            None => result.next_steps.push(format!(
                "theme `{theme_id}` has no color tokens or isn't in the registry — no theme stylesheet emitted"
            )),
        }
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

    // Install any `dx_components:` entries inline: shell out to
    // `dx components add <name>` for each catalog-valid entry. On failure
    // (missing `dx`, network error, …) falls back to surfacing the install
    // command in `next_steps` so the caller still sees what to run.
    install_dx_components(&doc, &crate_root, &mut result).await;

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

/// Build a token-driven stylesheet for a doc-level `theme:` — a `:root` block of
/// CSS vars from the theme's color tokens plus a small baseline that styles the
/// common generated markup (body / .screen / table / inputs / buttons) through
/// those vars. Switching the doc's theme changes only the values, so the
/// scaffolded app recolors. `None` when the theme carries no color tokens.
fn theme_stylesheet(theme: &dioxus_mcp_registry::ThemeDescriptor) -> Option<String> {
    let c = &theme.tokens.color;
    if c.is_empty() {
        return None;
    }
    let v = |k: &str, d: &str| c.get(k).map(String::as_str).unwrap_or(d).to_string();
    Some(format!(
        ":root {{\n  --bg: {bg};\n  --panel: {panel};\n  --border: {border};\n  --text: {text};\n  --muted: {muted};\n  --accent: {accent};\n}}\n\
         body {{ background: var(--bg); color: var(--text); font-family: system-ui, sans-serif; margin: 0; }}\n\
         .screen {{ max-width: 760px; margin: 2rem auto; padding: 0 1rem; }}\n\
         table {{ width: 100%; border-collapse: collapse; }}\n\
         th, td {{ padding: 8px 10px; border-bottom: 1px solid var(--border); text-align: left; }}\n\
         th {{ color: var(--muted); }}\n\
         input, textarea, select {{ background: var(--panel); color: var(--text); border: 1px solid var(--border); border-radius: 6px; padding: 8px 10px; }}\n\
         button {{ background: var(--accent); color: #fff; border: none; border-radius: 6px; padding: 8px 14px; cursor: pointer; }}\n\
         a {{ color: var(--accent); }}\n",
        bg = v("bg", "#ffffff"),
        panel = v("panel", "#f4f6fa"),
        border = v("border", "#d4dae6"),
        text = v("text", "#1a1d24"),
        muted = v("muted", "#5a6172"),
        accent = v("accent", "#2f6fe0"),
    ))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_stylesheet_emits_root_vars_and_baseline() {
        let reg = crate::registry::builtin();
        let css =
            theme_stylesheet(reg.themes.get("dark").unwrap()).expect("dark theme has color tokens");
        assert!(css.contains(":root"));
        assert!(css.contains("--accent: #6aa9ff"), "tokens become CSS vars");
        assert!(
            css.contains("background: var(--bg)"),
            "baseline rules reference the vars"
        );
        // A styling-family theme with no color tokens emits nothing.
        assert!(theme_stylesheet(reg.themes.get("tailwind").unwrap()).is_none());
    }
}
