use std::path::Path;
use std::sync::Arc;

use heck::{ToPascalCase, ToSnakeCase};
use minijinja::context;

use crate::state::State;
use crate::tools::scaffold::{self, CreateRouteParams, ScaffoldResult};

use super::super::render::*;
use super::super::templates::*;
use super::super::types::*;
use super::super::util::merge;
use super::screen_templates::{render_screen_template, vanilla_css_starter_for};

/// Render a screen's source body without writing. Shared between
/// `generate_screen` (which writes) and `plan_dsl` (which populates dry-run
/// previews so agents can inspect the output before committing).
pub(crate) fn build_screen_body(
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

pub(crate) async fn generate_screen(
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
    // `styled: vanilla-css` ships a starter CSS file alongside the screen so
    // the agent isn't staring at a blank `main.css` after scaffolding. The
    // class names in the sheet match the rsx! the template emits. Skipped
    // silently if the target already exists — we never overwrite.
    if let Some(t) = sc.template.as_ref()
        && let Some(css_body) = vanilla_css_starter_for(t, &snake)
    {
        let css_dir = crate_root.join("assets");
        let css_path = css_dir.join(format!("{snake}.css"));
        if !css_path.exists() {
            std::fs::create_dir_all(&css_dir).map_err(|e| e.to_string())?;
            std::fs::write(&css_path, css_body).map_err(|e| e.to_string())?;
            r.files_created.push(css_path.clone());
        }
        r.next_steps.push(format!(
            "mount the starter stylesheet in your App body: \
             `document::Stylesheet {{ href: asset!(\"/assets/{snake}.css\") }}` \
             (file at `assets/{snake}.css`)"
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
pub(crate) fn default_screen_class(snake: &str) -> String {
    format!("screen {snake}")
}

/// Find the line number (1-based) of the first `rsx!` macro invocation in a
/// generated source body. Used to point the agent at the markup block in
/// next_steps hints. Returns None when the body has no rsx! (shouldn't happen
/// for a Screen template but kept as a guard).
pub(crate) fn first_rsx_line(body: &str) -> Option<usize> {
    body.lines()
        .enumerate()
        .find(|(_, l)| l.contains("rsx!"))
        .map(|(i, _)| i + 1)
}
