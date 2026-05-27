use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use dioxus_mcp_registry::LayoutDescriptor;
use heck::{ToPascalCase, ToSnakeCase};
use minijinja::context;

use crate::state::State;
use crate::tools::scaffold::{self, CreateRouteParams, ScaffoldResult};

use super::humanize;
use super::super::render::*;
use super::super::templates::*;
use super::super::types::*;
use super::super::util::merge;
use super::screen_templates::{is_builtin_layout_kind, render_screen_template, vanilla_css_starter_for};

/// Render a screen's source body without writing. Shared between
/// `generate_screen` (which writes) and `plan_dsl` (which populates dry-run
/// previews so agents can inspect the output before committing).
pub(crate) fn build_screen_body(
    crate_root: &Path,
    sc: &DslScreen,
    client_stores: &[DslClientStore],
    layouts: &BTreeMap<String, LayoutDescriptor>,
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
        // Built-in kinds keep their Rust renderers; the registry is the dispatch
        // table for everything else. A runtime-added simple layout (a descriptor
        // with a template and `complex: false`) is rendered from its template.
        Some(t) if is_builtin_layout_kind(&t.kind) => render_screen_template(
            crate_root,
            &pascal,
            &snake,
            wrap_pascal.as_deref(),
            client_stores,
            t,
        ),
        Some(t) => render_registry_layout(layouts, &pascal, &snake, wrap_pascal.as_deref(), t),
    }
}

/// Render a runtime-added layout from its registry descriptor's minijinja
/// template. v1 supports only `complex: false` layouts (no Rust sub-renderer);
/// complex runtime layouts are a documented v2 boundary. Unknown kinds error
/// with the set of layouts the registry actually knows.
fn render_registry_layout(
    layouts: &BTreeMap<String, LayoutDescriptor>,
    pascal: &str,
    snake: &str,
    wrap_pascal: Option<&str>,
    t: &DslScreenTemplate,
) -> Result<String, String> {
    let Some(layout) = layouts.get(&t.kind) else {
        let known: Vec<&str> = layouts.keys().map(String::as_str).collect();
        return Err(format!(
            "unknown screen template kind {:?} (known layouts: {})",
            t.kind,
            known.join(", ")
        ));
    };
    if layout.complex {
        return Err(format!(
            "layout {:?} is marked `complex` — complex runtime layouts need a Rust \
             sub-renderer and aren't supported yet; only `complex: false` template \
             layouts can be added at runtime",
            t.kind
        ));
    }
    let Some(template) = &layout.template else {
        return Err(format!(
            "layout {:?} has no codegen `template`; a runtime layout must provide one",
            t.kind
        ));
    };
    // Generic context available to runtime layout templates. Mirrors the shape
    // the built-in templates use so descriptors feel familiar.
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
                _ => "text",
            };
            context! {
                name => fd.name.to_snake_case(),
                label => humanize(&fd.name),
                input_type => input_type,
                tag => if fd.ty == "textarea" { "textarea" } else { "input" },
                is_bool => is_bool,
            }
        })
        .collect();
    render(
        "registry_layout",
        template,
        context! {
            pascal => pascal,
            snake => snake,
            wrap_pascal => wrap_pascal,
            root_class => t.class.clone().unwrap_or_else(|| default_screen_class(snake)),
            item_type => t.item_type.clone(),
            endpoint => t.endpoint.clone(),
            on_submit => t.on_submit.clone(),
            redirect_to => t.redirect_to.clone(),
            fields => fields_ctx,
        },
    )
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

    let registry = state.registry();
    let body = build_screen_body(crate_root, sc, client_stores, &registry.layouts)?;
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
            "ensure `{w}` is exported from `crate::components` (e.g. emitted by a `protected_routes` entry or a hand-written component); \
             the wrap is applied inside the rsx body — if you replace the body with hand-authored markup, keep `{w} {{ ... }}` as the outer element"
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

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(kind: &str) -> DslScreen {
        DslScreen {
            name: "BannerScreen".into(),
            route: "/banner".into(),
            wrap_with: None,
            template: Some(DslScreenTemplate {
                kind: kind.into(),
                endpoint: None,
                item_type: Some("Note".into()),
                on_submit: None,
                redirect_to: None,
                fields: vec![],
                store: None,
                label_field: None,
                checkbox_field: None,
                class: None,
                body: None,
                styled: None,
                compose_style: None,
                crud: None,
            }),
            replace_route: false,
            route_params: vec![],
        }
    }

    fn layouts_with(extra: LayoutDescriptor) -> BTreeMap<String, LayoutDescriptor> {
        let mut m = crate::registry::builtin().layouts;
        m.insert(extra.id.clone(), extra);
        m
    }

    #[test]
    fn renders_runtime_simple_layout_from_descriptor_template() {
        let layout = LayoutDescriptor {
            id: "banner".into(),
            label: "Banner".into(),
            nav_rank: 9,
            template: Some("// {{ pascal }} banner for {{ item_type }}".into()),
            complex: false,
            context_vars: vec![],
            preview: Default::default(),
        };
        let body = build_screen_body(
            std::env::temp_dir().as_path(),
            &screen("banner"),
            &[],
            &layouts_with(layout),
        )
        .unwrap();
        assert_eq!(body, "// BannerScreen banner for Note");
    }

    #[test]
    fn unknown_layout_kind_lists_known_layouts() {
        let err = build_screen_body(
            std::env::temp_dir().as_path(),
            &screen("nonexistent_kind"),
            &[],
            &crate::registry::builtin().layouts,
        )
        .unwrap_err();
        assert!(err.contains("unknown screen template kind"), "got: {err}");
        assert!(
            err.contains("resource_list"),
            "error should list known layouts, got: {err}"
        );
    }

    #[test]
    fn complex_runtime_layout_is_rejected_in_v1() {
        let layout = LayoutDescriptor {
            id: "fancy".into(),
            label: "Fancy".into(),
            nav_rank: 9,
            template: None,
            complex: true,
            context_vars: vec![],
            preview: Default::default(),
        };
        let err = build_screen_body(
            std::env::temp_dir().as_path(),
            &screen("fancy"),
            &[],
            &layouts_with(layout),
        )
        .unwrap_err();
        assert!(err.contains("complex"), "got: {err}");
    }
}
