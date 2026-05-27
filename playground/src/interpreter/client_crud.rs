//! Interactive `client_crud` preview. Because this kind is in-memory (no server
//! fn), the preview can wire add/toggle/delete against a local signal and
//! behave exactly like the generated screen would.

use dioxus::prelude::*;

use super::{humanize, to_snake};
use crate::model::ScreenTemplate;

#[derive(Clone, PartialEq)]
struct Item {
    id: i64,
    label: String,
    checked: bool,
}

#[component]
pub fn ClientCrud(template: ScreenTemplate, screen_name: String) -> Element {
    let mut items = use_signal(Vec::<Item>::new);
    let mut draft = use_signal(String::new);
    let mut next_id = use_signal(|| 1i64);

    let snake = to_snake(&screen_name);
    let has_checkbox = template.checkbox_field.is_some();
    let enter_only = template.compose_style.as_deref() == Some("enter_only");
    let placeholder = format!(
        "New {}",
        template.item_type.as_deref().map(humanize).unwrap_or_else(|| "Item".into())
    );

    rsx! {
        div { class: "screen {snake}",
            h1 { "{screen_name}" }
            form { class: "add",
                onsubmit: move |evt: FormEvent| {
                    evt.prevent_default();
                    let value = draft().trim().to_string();
                    if value.is_empty() {
                        return;
                    }
                    let id = next_id();
                    next_id += 1;
                    items.write().push(Item { id, label: value, checked: false });
                    draft.set(String::new());
                },
                input {
                    r#type: "text",
                    value: "{draft}",
                    placeholder,
                    oninput: move |e| draft.set(e.value()),
                }
                if !enter_only {
                    button { r#type: "submit", "Add" }
                }
            }
            ul { class: "{snake}-items",
                for item in items().iter() {
                    CrudRow {
                        key: "{item.id}",
                        id: item.id,
                        label: item.label.clone(),
                        checked: item.checked,
                        has_checkbox,
                        items,
                    }
                }
            }
            if items().is_empty() {
                p { class: "preview-hint", "Type above and press Add — this preview is live." }
            }
        }
    }
}

#[component]
fn CrudRow(
    id: i64,
    label: String,
    checked: bool,
    has_checkbox: bool,
    items: Signal<Vec<Item>>,
) -> Element {
    let mut items = items;
    rsx! {
        li {
            if has_checkbox {
                input {
                    r#type: "checkbox",
                    checked,
                    onchange: move |_| {
                        let mut guard = items.write();
                        if let Some(it) = guard.iter_mut().find(|i| i.id == id) {
                            it.checked = !it.checked;
                        }
                    },
                }
            }
            span { class: if checked { "done" } else { "" }, "{label}" }
            button {
                class: "delete",
                aria_label: "Delete {label}",
                onclick: move |_| {
                    items.write().retain(|i| i.id != id);
                },
                "×"
            }
        }
    }
}
