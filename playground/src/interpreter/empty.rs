//! `empty` preview: the generator emits a placeholder `div { h1 {..} }`, or a
//! bare root when `body: empty`/`stub`.

use dioxus::prelude::*;

use super::to_snake;
use crate::model::ScreenTemplate;

#[component]
pub fn EmptyScreen(template: ScreenTemplate, screen_name: String) -> Element {
    let snake = to_snake(&screen_name);
    let is_stub = matches!(template.body.as_deref(), Some("empty") | Some("stub"));

    rsx! {
        div { class: "screen {snake}",
            if !is_stub {
                h1 { "{screen_name}" }
            }
        }
    }
}
