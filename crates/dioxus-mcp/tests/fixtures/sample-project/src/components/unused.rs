// Demonstrates: dead_components (no rsx! invocation anywhere references this)
use dioxus::prelude::*;

#[component]
pub fn Unused() -> Element {
    rsx! { div { "nobody renders me" } }
}
