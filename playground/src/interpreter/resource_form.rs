//! `resource_form` preview: a labeled input per field. Pure declarative markup,
//! so this is high fidelity — the inputs are real and typeable. Submit is a
//! no-op (the real one calls a server fn and navigates).

use dioxus::prelude::*;

use super::{humanize, input_type, to_snake, PreviewNav};
use crate::model::{FieldDef, ScreenTemplate};

#[component]
pub fn ResourceForm(template: ScreenTemplate, screen_name: String) -> Element {
    let snake = to_snake(&screen_name);
    let fields = template.fields.clone();

    // Best-effort fake-router: back link + post-submit return to a sibling list
    // screen if the doc hand-authored one in the same resource group.
    let nav = try_consume_context::<PreviewNav>();
    let item_type = template.item_type.clone();
    let back_target = item_type
        .as_deref()
        .map(|t| nav.map(|n| n.has_resource(t, "resource_list")).unwrap_or(false))
        .unwrap_or(false);

    rsx! {
        div { class: "screen {snake}",
            h1 { "{screen_name}" }
            if back_target {
                button {
                    class: "toolbar-link",
                    onclick: {
                        let item_type = item_type.clone();
                        move |_| {
                            if let (Some(nav), Some(t)) = (nav, item_type.as_deref()) {
                                nav.go_resource(t, "resource_list");
                            }
                        }
                    },
                    "← Back to list"
                }
            }
            form {
                class: "resource-form",
                onsubmit: {
                    let item_type = item_type.clone();
                    move |evt: FormEvent| {
                        evt.prevent_default();
                        if let (Some(nav), Some(t)) = (nav, item_type.as_deref()) {
                            nav.go_resource(t, "resource_list");
                        }
                    }
                },
                for field in fields.iter() {
                    FormField { key: "{field.name}", field: field.clone() }
                }
                button { r#type: "submit", "Submit" }
            }
            if let Some(target) = template.on_submit.clone().or(template.endpoint.clone()) {
                p { class: "preview-hint", "Submitting would call {target}()" }
            }
        }
    }
}

#[component]
fn FormField(field: FieldDef) -> Element {
    let label = humanize(&field.name);
    rsx! {
        label { "{label}" }
        match field.ty.as_str() {
            "textarea" => rsx! { textarea { name: "{field.name}" } },
            other => rsx! {
                input { r#type: input_type(other), name: "{field.name}" }
            },
        }
    }
}
