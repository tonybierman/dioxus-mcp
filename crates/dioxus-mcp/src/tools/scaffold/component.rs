use std::sync::Arc;

use heck::{ToPascalCase, ToSnakeCase};
use minijinja::{Environment, context};

use crate::state::State;

use super::mod_tree::upsert_mod_entry;
use super::types::{CreateComponentParams, ModUpsert, ScaffoldResult};

pub(crate) const COMPONENT_TPL: &str = r#"use dioxus::prelude::*;

{%- if has_props %}

#[derive(Props, PartialEq, Clone)]
pub struct {{ pascal }}Props {
{%- for p in props %}
    {%- if p.optional %}
    #[props(default)]
    pub {{ p.name }}: Option<{{ p.ty }}>,
    {%- else %}
    pub {{ p.name }}: {{ p.ty }},
    {%- endif %}
{%- endfor %}
}
{%- endif %}

#[component]
pub fn {{ pascal }}({% if has_props %}props: {{ pascal }}Props{% endif %}) -> Element {
    rsx! {
{{ body }}
    }
}
"#;

/// Body skeletons selectable via `template:`. Indentation is calibrated to slot
/// in two-spaces under the `rsx! {` block in `COMPONENT_TPL`.
const COMPONENT_BODY_EMPTY: &str = r#"        div { class: "{{ snake }}",
            "{{ pascal }} component"
        }"#;

const COMPONENT_BODY_FORM: &str = r#"        form { class: "{{ snake }}",
            onsubmit: move |evt: Event<FormData>| {
                evt.prevent_default();
                // TODO: read form values and submit
            },
            div { class: "field",
                label { "Field" }
                input { r#type: "text", name: "field" }
            }
            button { r#type: "submit", "Submit" }
        }"#;

const COMPONENT_BODY_LIST: &str = r#"        div { class: "{{ snake }}",
            h2 { "{{ pascal }}" }
            // TODO: replace with real items, e.g. `for item in items.iter()`
            ul { class: "{{ snake }}-items",
                li { "Empty list" }
            }
        }"#;

const COMPONENT_BODY_CRUD_TABLE: &str = r#"        div { class: "{{ snake }}",
            div { class: "toolbar",
                button { "New" }
            }
            table { class: "{{ snake }}-table",
                thead {
                    tr {
                        th { "Id" }
                        th { "Name" }
                        th { class: "actions", "Actions" }
                    }
                }
                tbody {
                    // TODO: `for row in rows.iter() { tr { key: "{row.id}", ... } }`
                    tr {
                        td { "—" }
                        td { "No rows" }
                        td {}
                    }
                }
            }
        }"#;

const COMPONENT_BODY_RESOURCE_VIEW: &str = r#"        article { class: "{{ snake }}",
            header {
                h2 { "{{ pascal }}" }
            }
            dl { class: "{{ snake }}-fields",
                dt { "Field" }
                dd { "—" }
            }
            footer { class: "actions",
                button { "Edit" }
                button { class: "danger", "Delete" }
            }
        }"#;

fn component_body_for(template: &str) -> Result<&'static str, String> {
    match template {
        "empty" => Ok(COMPONENT_BODY_EMPTY),
        "form" => Ok(COMPONENT_BODY_FORM),
        "list" => Ok(COMPONENT_BODY_LIST),
        "crud_table" => Ok(COMPONENT_BODY_CRUD_TABLE),
        "resource_view" => Ok(COMPONENT_BODY_RESOURCE_VIEW),
        other => Err(format!(
            "create_component: unknown template {other:?}; valid: empty, form, list, crud_table, resource_view"
        )),
    }
}

pub async fn create_component(
    state: &Arc<State>,
    p: CreateComponentParams,
) -> Result<ScaffoldResult, String> {
    let crate_root = super::crate_root(state, p.project_root.as_deref()).await?;
    let components_dir = crate_root.join(p.path.as_deref().unwrap_or("src/components"));
    std::fs::create_dir_all(&components_dir).map_err(|e| e.to_string())?;

    let pascal = p.name.to_pascal_case();
    let snake = p.name.to_snake_case();
    let target = components_dir.join(format!("{snake}.rs"));
    if target.exists() {
        return Err(format!("{} already exists", target.display()));
    }

    let template_kind = p.template.as_deref().unwrap_or("empty");
    let body_tpl = component_body_for(template_kind)?;

    let mut env = Environment::new();
    env.add_template("component_body", body_tpl).unwrap();
    let body = env
        .get_template("component_body")
        .unwrap()
        .render(context! {
            pascal => pascal.clone(),
            snake => snake.clone(),
        })
        .map_err(|e| e.to_string())?;

    env.add_template("component", COMPONENT_TPL).unwrap();
    let tpl = env.get_template("component").unwrap();
    let rendered = tpl
        .render(context! {
            pascal => pascal.clone(),
            snake => snake.clone(),
            has_props => !p.props.is_empty(),
            props => p.props.iter().map(|p| context!{ name => p.name.clone(), ty => p.ty.clone(), optional => p.optional }).collect::<Vec<_>>(),
            body => body,
        })
        .map_err(|e| e.to_string())?;
    std::fs::write(&target, rendered).map_err(|e| e.to_string())?;

    // ensure mod.rs exports it
    let mod_rs = components_dir.join("mod.rs");
    let upsert = upsert_mod_entry(&mod_rs, &snake, None, false)?;
    let (files_created, files_modified) = match upsert {
        ModUpsert::Created => (vec![target, mod_rs], vec![]),
        ModUpsert::Modified => (vec![target], vec![mod_rs]),
        ModUpsert::Unchanged => (vec![target], vec![]),
    };

    let next_steps = vec![
        format!("`use crate::components::{pascal};` where you want to render it"),
        "wire `mod components;` into your crate root if it isn't already".into(),
    ];

    Ok(ScaffoldResult {
        files_created,
        files_modified,
        next_steps,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(template: &str) -> String {
        let body_tpl = component_body_for(template).expect("known template");
        let mut env = Environment::new();
        env.add_template("body", body_tpl).unwrap();
        let body = env
            .get_template("body")
            .unwrap()
            .render(context! {
                pascal => "ProductTable".to_string(),
                snake => "product_table".to_string(),
            })
            .unwrap();

        env.add_template("c", COMPONENT_TPL).unwrap();
        env.get_template("c")
            .unwrap()
            .render(context! {
                pascal => "ProductTable".to_string(),
                snake => "product_table".to_string(),
                has_props => false,
                props => Vec::<()>::new(),
                body => body,
            })
            .unwrap()
    }

    #[test]
    fn empty_template_matches_legacy_body() {
        let s = render("empty");
        assert!(s.contains(r#"div { class: "product_table","#));
        assert!(s.contains(r#""ProductTable component""#));
    }

    #[test]
    fn form_template_emits_form_with_submit_handler() {
        let s = render("form");
        assert!(s.contains("form { class: \"product_table\""));
        assert!(s.contains("onsubmit:"));
        assert!(s.contains("button { r#type: \"submit\""));
    }

    #[test]
    fn list_template_emits_ul_with_empty_state() {
        let s = render("list");
        assert!(s.contains("ul { class: \"product_table-items\""));
        assert!(s.contains("Empty list"));
    }

    #[test]
    fn crud_table_template_emits_table_skeleton() {
        let s = render("crud_table");
        assert!(s.contains("table { class: \"product_table-table\""));
        assert!(s.contains("thead {") && s.contains("tbody {"));
        assert!(s.contains("button { \"New\""));
    }

    #[test]
    fn resource_view_template_emits_article_with_actions() {
        let s = render("resource_view");
        assert!(s.contains("article { class: \"product_table\""));
        assert!(s.contains("dl { class: \"product_table-fields\""));
        assert!(s.contains("button { class: \"danger\", \"Delete\""));
    }

    #[test]
    fn unknown_template_is_rejected_with_helpful_message() {
        let err = component_body_for("dropdown").unwrap_err();
        assert!(err.contains("unknown template"));
        assert!(err.contains("crud_table"));
    }
}
