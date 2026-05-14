// Demonstrates: project_index (zero-prop component), route_map (used as layout)
use dioxus::prelude::*;

#[component]
pub fn NavBar() -> Element {
    rsx! { nav { "navbar" } }
}
