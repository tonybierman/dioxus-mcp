//! Render a server-resolved [`RenderModel`] — the resource-synthesized screens
//! (list/new/edit) that don't exist in the user's local `screens:` and so can't
//! be interpreted from the local parse. `resource_list` gets a real table with
//! the model's columns + type-appropriate mock rows; the forms render real
//! inputs from the resolved fields.

use dioxus::prelude::*;

use super::{input_type, PreviewNav};
use crate::model::{RenderField, RenderModel};

#[component]
pub fn RenderModelView(model: RenderModel) -> Element {
    match model.kind.as_str() {
        "resource_list" => rsx! { ResourceListModel { model } },
        "resource_form" | "resource_edit_form" => rsx! { ResourceFormModel { model } },
        other => rsx! { div { class: "preview-unknown", "render model kind: {other}" } },
    }
}

#[component]
fn ResourceListModel(model: RenderModel) -> Element {
    let class = model.root_class.clone().unwrap_or_else(|| "screen".into());
    // Fake router (provided by ScreenNavigator). Absent → affordances are inert.
    let nav = try_consume_context::<PreviewNav>();
    let item_type = model.item_type.clone();
    // A row click jumps to this resource's edit screen, when one is present.
    let row_target = nav
        .map(|n| n.has_resource(&item_type, "resource_edit_form"))
        .unwrap_or(false);
    rsx! {
        div { class: "{class}",
            h1 { "{model.screen}" }
            if let Some(new_route) = model.new_route.clone() {
                // A button, never an `<a href>` — real navigation would reload
                // the SPA. The fake router just switches the active screen.
                button {
                    class: "toolbar-link",
                    onclick: move |_| {
                        if let Some(nav) = nav {
                            nav.go_route(&new_route);
                        }
                    },
                    "+ New {model.item_type}"
                }
            }
            table { class: "preview-table",
                thead {
                    tr {
                        for col in model.columns.iter() {
                            th { "{col.label}" }
                        }
                    }
                }
                tbody {
                    for n in 1..=3i64 {
                        tr {
                            key: "{n}",
                            class: if row_target { "clickable-row" } else { "" },
                            onclick: {
                                let item_type = item_type.clone();
                                move |_| {
                                    if let Some(nav) = nav {
                                        nav.go_resource(&item_type, "resource_edit_form");
                                    }
                                }
                            },
                            for col in model.columns.iter() {
                                td { "{mock_cell(&col.ty, n)}" }
                            }
                        }
                    }
                }
            }
            if let Some(endpoint) = model.list_endpoint.clone() {
                p { class: "preview-banner",
                    "mock data — real rows come from "
                    code { "{endpoint}()" }
                    " after a compile"
                }
            }
        }
    }
}

#[component]
fn ResourceFormModel(model: RenderModel) -> Element {
    let class = model.root_class.clone().unwrap_or_else(|| "screen".into());
    let nav = try_consume_context::<PreviewNav>();
    let item_type = model.item_type.clone();
    // The form's list sibling, for the back link and the post-submit return.
    let back_target = nav
        .map(|n| n.has_resource(&item_type, "resource_list"))
        .unwrap_or(false);
    rsx! {
        div { class: "{class}",
            h1 { "{model.screen}" }
            if back_target {
                button {
                    class: "toolbar-link",
                    onclick: {
                        let item_type = item_type.clone();
                        move |_| {
                            if let Some(nav) = nav {
                                nav.go_resource(&item_type, "resource_list");
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
                        // Mimic the generated screen: a successful submit
                        // returns to the list.
                        if let Some(nav) = nav {
                            nav.go_resource(&item_type, "resource_list");
                        }
                    }
                },
                for field in model.fields.iter() {
                    ModelField { key: "{field.name}", field: field.clone() }
                }
                button { r#type: "submit", "Submit" }
            }
        }
    }
}

#[component]
fn ModelField(field: RenderField) -> Element {
    rsx! {
        label { "{field.label}" }
        match field.ty.as_str() {
            "textarea" => rsx! { textarea { name: "{field.name}" } },
            other => rsx! {
                input { r#type: input_type(other), name: "{field.name}" }
            },
        }
    }
}

/// A type-appropriate mock cell value (the real rows need a compile).
fn mock_cell(ty: &str, n: i64) -> String {
    let base = ty
        .trim_start_matches("Option<")
        .trim_start_matches("Option <")
        .trim_end_matches('>')
        .trim();
    match base {
        "i64" | "i32" | "u64" | "u32" | "usize" | "u8" | "i8" | "u16" | "i16" => n.to_string(),
        "f64" | "f32" => format!("{:.2}", n as f64 * 9.99),
        "bool" => if n % 2 == 0 { "true".into() } else { "false".into() },
        "String" | "str" | "&str" => format!("Sample {n}"),
        _ => format!("{base} {n}"),
    }
}
