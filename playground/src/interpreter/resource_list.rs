//! `resource_list` preview: render the list chrome with mock rows. The real
//! rows come from a server fn (`endpoint`) that only exists after a compile, so
//! the client can't fetch them. M3 will feed faithful columns + the resolved
//! CrudCtx via a server-side render-model; until then this is a stand-in.

use dioxus::prelude::*;

use super::{humanize, to_snake, PreviewNav};
use crate::model::ScreenTemplate;

#[component]
pub fn ResourceList(template: ScreenTemplate, screen_name: String) -> Element {
    let snake = to_snake(&screen_name);
    let item = template
        .item_type
        .as_deref()
        .map(humanize)
        .unwrap_or_else(|| "Item".into());

    // Best-effort fake-router: rows jump to a sibling edit screen if the doc
    // hand-authored one in the same resource group. Inert otherwise.
    let nav = try_consume_context::<PreviewNav>();
    let item_type = template.item_type.clone();
    let row_target = item_type
        .as_deref()
        .map(|t| nav.map(|n| n.has_resource(t, "resource_edit_form")).unwrap_or(false))
        .unwrap_or(false);

    rsx! {
        div { class: "screen {snake}",
            h1 { "{screen_name}" }
            p { class: "preview-banner",
                "mock data — real rows come from "
                code {
                    { template.endpoint.clone().unwrap_or_else(|| "the server fn".into()) }
                    "()"
                }
                " after a compile"
            }
            ul { class: "{snake}-items",
                for n in 1..=3 {
                    li {
                        key: "{n}",
                        class: if row_target { "clickable-row" } else { "" },
                        onclick: {
                            let item_type = item_type.clone();
                            move |_| {
                                if let (Some(nav), Some(t)) = (nav, item_type.as_deref()) {
                                    nav.go_resource(t, "resource_edit_form");
                                }
                            }
                        },
                        "Sample {item} {n}"
                    }
                }
            }
        }
    }
}
