//! The interpreter: render an approximate live preview of a DSL `Screen` by
//! tree-walking its `kind` into equivalent DOM. This never parses RSX — it
//! mirrors the same bounded set of template kinds the server generates.
//!
//! Fidelity by kind: `client_crud` is fully interactive (in-memory, no server),
//! `resource_form` renders real inputs, `empty` is a placeholder, and
//! `resource_list` renders chrome + mock rows (the real rows come from a server
//! fn that only exists after a compile — M3 will feed it a server render-model).

mod client_crud;
mod empty;
mod navigator;
mod render_model;
mod resource_form;
mod resource_list;

pub use navigator::{build_groups, PreviewNav, ScreenNavigator};
pub use render_model::RenderModelView;

use dioxus::prelude::*;

use crate::model::Screen;

/// Render one screen's preview, dispatching on its template kind.
#[component]
pub fn ScreenPreview(screen: Screen) -> Element {
    let name = screen.name.clone();
    let template = screen.template.clone().unwrap_or_default();
    let kind = if screen.template.is_some() {
        template.kind.as_str()
    } else {
        "empty"
    };

    match kind {
        "client_crud" => rsx! { client_crud::ClientCrud { template, screen_name: name } },
        "resource_form" => rsx! { resource_form::ResourceForm { template, screen_name: name } },
        "resource_list" | "resource_edit_form" => {
            rsx! { resource_list::ResourceList { template, screen_name: name } }
        }
        "empty" => rsx! { empty::EmptyScreen { template, screen_name: name } },
        other => rsx! {
            div { class: "preview-unknown", "Unsupported screen kind: \"{other}\"" }
        },
    }
}

/// Convert a PascalCase name to snake_case (e.g. `TodoScreen` → `todo_screen`),
/// matching the generator's leaf naming for the `screen {snake}` root class.
pub(crate) fn to_snake(s: &str) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Turn a snake_case field name into a human label (`first_name` → `First Name`).
pub(crate) fn humanize(s: &str) -> String {
    s.split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Map a DSL field type to an HTML `<input type>` value.
pub(crate) fn input_type(ty: &str) -> &'static str {
    match ty {
        "email" => "email",
        "password" => "password",
        "number" => "number",
        "checkbox" => "checkbox",
        _ => "text",
    }
}
