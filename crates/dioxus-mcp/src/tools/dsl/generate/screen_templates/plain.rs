use std::path::Path;

use heck::ToSnakeCase;
use minijinja::context;

use super::super::super::render::*;
use super::super::super::templates::*;
use super::super::super::types::*;
use super::super::humanize;
use super::super::screen::default_screen_class;
use super::client_crud::render_client_crud_screen;
use super::resource_crud::{
    render_resource_crud_list, render_resource_edit_form, resource_form_submit_body,
};

pub(crate) fn render_screen_template(
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

#[cfg(test)]
mod tests {
    use super::super::super::super::render::render;
    use super::super::super::super::templates::SCREEN_TPL;
    use super::super::super::screen::default_screen_class;
    use super::*;

    #[test]
    fn screen_template_empty_with_store_emits_use_context() {
        let cs = DslClientStore {
            name: "TodoStore".into(),
            item_type: "Todo".into(),
            initial: None,
            id_field: Some("id".into()),
            id_type: None,
            auto_id: Some(true),
        };
        let t = DslScreenTemplate {
            kind: "empty".into(),
            endpoint: None,
            item_type: None,
            on_submit: None,
            redirect_to: None,
            fields: vec![],
            store: Some("TodoStore".into()),
            label_field: None,
            checkbox_field: None,
            class: Some("page-todo".into()),
            body: None,
            styled: None,
            crud: None,
        };
        let body = render_screen_template(
            std::env::temp_dir().as_path(),
            "TodoScreen",
            "todo_screen",
            None,
            &[cs],
            &t,
        )
        .unwrap();
        assert!(
            body.contains("use crate::state::todo_store::*;"),
            "expected store glob import (needed to bring the #[store(pub)] extension trait into scope), got:\n{body}"
        );
        assert!(
            body.contains("let _store = use_todo_store();"),
            "expected use_<store>() wiring, got:\n{body}"
        );
        assert!(
            body.contains("class: \"page-todo\""),
            "expected custom class override, got:\n{body}"
        );
        assert!(
            !body.contains("class: \"screen "),
            "default class should not appear when override is set, got:\n{body}"
        );
    }

    #[test]
    fn screen_template_empty_rejects_unknown_store() {
        let t = DslScreenTemplate {
            kind: "empty".into(),
            endpoint: None,
            item_type: None,
            on_submit: None,
            redirect_to: None,
            fields: vec![],
            store: Some("Nonexistent".into()),
            label_field: None,
            checkbox_field: None,
            class: None,
            body: None,
            styled: None,
            crud: None,
        };
        let err = render_screen_template(
            std::env::temp_dir().as_path(),
            "TodoScreen",
            "todo_screen",
            None,
            &[],
            &t,
        )
        .unwrap_err();
        assert!(err.contains("unknown client_store"), "got: {err}");
    }

    #[test]
    fn screen_template_empty_body_drops_placeholder() {
        let cs = DslClientStore {
            name: "TodoStore".into(),
            item_type: "Todo".into(),
            initial: None,
            id_field: Some("id".into()),
            id_type: None,
            auto_id: Some(true),
        };
        let t = DslScreenTemplate {
            kind: "empty".into(),
            endpoint: None,
            item_type: None,
            on_submit: None,
            redirect_to: None,
            fields: vec![],
            store: Some("TodoStore".into()),
            label_field: None,
            checkbox_field: None,
            class: None,
            body: Some("empty".into()),
            styled: None,
            crud: None,
        };
        let body = render_screen_template(
            std::env::temp_dir().as_path(),
            "TodoScreen",
            "todo_screen",
            None,
            std::slice::from_ref(&cs),
            &t,
        )
        .unwrap();
        assert!(
            body.contains("let _store = use_todo_store();"),
            "store wiring should remain, got:\n{body}"
        );
        assert!(
            body.contains("rsx! {}"),
            "expected bare `rsx! {{}}`, got:\n{body}"
        );
        assert!(
            !body.contains("h1 {"),
            "placeholder h1 should be dropped, got:\n{body}"
        );
        assert!(
            !body.contains("div { class:"),
            "placeholder div should be dropped, got:\n{body}"
        );

        // `body: stub` should behave the same as `body: empty`.
        let t_stub = DslScreenTemplate {
            body: Some("stub".into()),
            ..t.clone()
        };
        let body_stub = render_screen_template(
            std::env::temp_dir().as_path(),
            "TodoScreen",
            "todo_screen",
            None,
            &[cs],
            &t_stub,
        )
        .unwrap();
        assert!(
            body_stub.contains("rsx! {}"),
            "stub alias should also drop the placeholder, got:\n{body_stub}"
        );
    }

    #[test]
    fn screen_template_empty_body_rejects_unknown_value() {
        let t = DslScreenTemplate {
            kind: "empty".into(),
            endpoint: None,
            item_type: None,
            on_submit: None,
            redirect_to: None,
            fields: vec![],
            store: None,
            label_field: None,
            checkbox_field: None,
            class: None,
            body: Some("nope".into()),
            styled: None,
            crud: None,
        };
        let err = render_screen_template(
            std::env::temp_dir().as_path(),
            "TodoScreen",
            "todo_screen",
            None,
            &[],
            &t,
        )
        .unwrap_err();
        assert!(err.contains("\"empty\""), "got: {err}");
    }

    #[test]
    fn screen_template_wraps_with_when_set() {
        let out = render(
            "screen",
            SCREEN_TPL,
            minijinja::context! {
                pascal => "HomeScreen",
                snake => "home_screen",
                wrap_pascal => Some("Dashboard"),
                root_class => default_screen_class("home_screen"),
                store_snake => None::<String>,
            },
        )
        .unwrap();
        assert!(
            out.contains("use crate::components::Dashboard;"),
            "expected import for Dashboard, got:\n{out}"
        );
        assert!(
            out.contains("Dashboard {"),
            "expected Dashboard {{ ... }} wrapper, got:\n{out}"
        );
        let body_start = out.find("rsx!").unwrap();
        let body = &out[body_start..];
        let dash_pos = body.find("Dashboard {").unwrap();
        let div_pos = body.find("div {").unwrap();
        assert!(
            dash_pos < div_pos,
            "Dashboard wrapper must be outside the div, got:\n{out}"
        );
    }

    #[test]
    fn screen_template_omits_wrapper_when_unset() {
        let out = render(
            "screen",
            SCREEN_TPL,
            minijinja::context! {
                pascal => "HomeScreen",
                snake => "home_screen",
                wrap_pascal => None::<String>,
                root_class => default_screen_class("home_screen"),
                store_snake => None::<String>,
            },
        )
        .unwrap();
        assert!(
            !out.contains("Dashboard"),
            "expected no wrapper, got:\n{out}"
        );
        assert!(!out.contains("use crate::components::"));
    }
}
