//! Render a server-resolved [`RenderModel`] — the resource-synthesized screens
//! (list/new/edit) that don't exist in the user's local `screens:` and so can't
//! be interpreted from the local parse. `resource_list` gets a real table with
//! the model's columns + type-appropriate mock rows; the forms render real
//! inputs from the resolved fields.

use dioxus::prelude::*;
use dioxus_mcp_registry::RenderNode;

use super::{input_type, PreviewNav};
use crate::model::{RenderField, RenderModel};

#[component]
pub fn RenderModelView(model: RenderModel) -> Element {
    match model.kind.as_str() {
        "resource_list" => rsx! { ResourceListModel { model } },
        "resource_form" | "resource_edit_form" => rsx! { ResourceFormModel { model } },
        // Runtime layouts (registry-defined, no bespoke component) arrive with a
        // server-instantiated generic node tree. Render it directly. Built-in
        // kinds never carry `nodes`, so this only fires for runtime layouts.
        other => {
            if model.nodes.is_empty() {
                rsx! { div { class: "preview-unknown", "render model kind: {other}" } }
            } else {
                let class = model.root_class.clone().unwrap_or_else(|| "screen".into());
                let html = nodes_to_html(&model.nodes);
                rsx! { div { class: "{class}", dangerous_inner_html: "{html}" } }
            }
        }
    }
}

/// Serialize a generic preview node tree to an HTML string. We go through
/// `dangerous_inner_html` because Dioxus rsx! can't take a dynamic element tag,
/// and a runtime-layout preview is approximate/static — interactivity isn't
/// expected here (the `Compiled` tab is the real thing).
fn nodes_to_html(nodes: &[RenderNode]) -> String {
    let mut s = String::new();
    for n in nodes {
        node_html(n, &mut s);
    }
    s
}

fn node_html(node: &RenderNode, out: &mut String) {
    match node {
        RenderNode::Text { text } => out.push_str(&escape_text(text)),
        RenderNode::Element {
            tag,
            class,
            attrs,
            children,
        } => {
            out.push('<');
            out.push_str(tag);
            if let Some(c) = class {
                out.push_str(" class=\"");
                out.push_str(&escape_attr(c));
                out.push('"');
            }
            for (k, v) in attrs {
                out.push(' ');
                out.push_str(k);
                out.push_str("=\"");
                out.push_str(&escape_attr(v));
                out.push('"');
            }
            out.push('>');
            for c in children {
                node_html(c, out);
            }
            out.push_str("</");
            out.push_str(tag);
            out.push('>');
        }
        // Live, behavior-driven content (e.g. a client_crud list). Static
        // preview placeholder for now — a fuller behavior port is follow-up.
        RenderNode::Slot { .. } => {
            out.push_str("<p class=\"preview-hint\">live content — compile to see</p>")
        }
    }
}

fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn escape_attr(s: &str) -> String {
    escape_text(s).replace('"', "&quot;")
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
