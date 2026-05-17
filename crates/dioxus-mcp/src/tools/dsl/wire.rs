use std::path::Path;

use heck::{ToPascalCase, ToSnakeCase};

use crate::tools::scaffold;

use super::text_edit::*;
use super::types::*;

/// If the doc declares any routable primitive (Screen, LoginScreen) and no
/// Routable enum exists anywhere under src/, write a minimal `src/router.rs`
/// seeded with every declared route, and inject `pub mod router;` into the
/// crate root. Makes `dx new` → `execute_code` runnable in one call instead
/// of erroring on the first screen with "no Routable enum on disk".
///
/// Returns the list of paths created/modified by the bootstrap (caller merges
/// these into the top-level result so the response stays honest).
pub(super) fn bootstrap_router_if_needed(
    doc: &DslDoc,
    crate_root: &Path,
) -> Result<BootstrapRouter, String> {
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
pub(super) struct BootstrapRouter {
    pub(super) created: Vec<std::path::PathBuf>,
    pub(super) modified: Vec<std::path::PathBuf>,
    pub(super) next_step: Option<String>,
}

/// Locate the file holding the `#[derive(Routable)]` enum so the response
/// can report where new route variants will land. Returns None when the doc
/// declares no routes (so no enum will be touched) or the project has no
/// Routable enum on disk yet (the router-bootstrap step will create one at
/// the canonical path; that path is already covered by `files_created`).
pub(super) fn detected_routable_file(
    doc: &DslDoc,
    crate_root: &Path,
) -> Option<std::path::PathBuf> {
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
pub(super) fn routable_location_warning(
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
pub(super) struct WireApp {
    pub(super) modified: Vec<std::path::PathBuf>,
    pub(super) next_steps: Vec<String>,
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
pub(super) fn wire_app_if_needed(doc: &DslDoc, crate_root: &Path) -> Result<WireApp, String> {
    let needs_router = !doc.screens.is_empty() || !doc.login_screens.is_empty();
    // ClientStores + ViewStates both expose a `provide_{snake}()` under
    // `crate::state::{snake}` and need the same App-body splice. Merge both
    // lists so the wiring loop below stays a single pass.
    let mut store_snakes: Vec<String> = doc
        .client_stores
        .iter()
        .map(|cs| cs.name.to_snake_case())
        .collect();
    store_snakes.extend(doc.view_states.iter().map(|vs| vs.name.to_snake_case()));
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
