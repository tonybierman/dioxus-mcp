// Demonstrates: check_rsx (missing key:, empty event handler)
use dioxus::prelude::*;

#[component]
pub fn LintDemo() -> Element {
    let items = vec![1, 2, 3];
    rsx! {
        div {
            for i in items.iter() {
                div { "no key — bad" }
            }
            button {
                onclick: move || {
                    // No closure params - real Dioxus expects an Event arg.
                },
                "click"
            }
        }
    }
}
