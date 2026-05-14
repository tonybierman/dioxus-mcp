// Demonstrates: asset_audit, project_index (App component)
use dioxus::prelude::*;

mod components;
mod router;
mod server;
mod lint_demo;

#[component]
pub fn App() -> Element {
    let _logo = asset!("/assets/logo.png");
    let _broken = asset!("/assets/missing.svg");
    rsx! { Router::<router::Route> {} }
}

fn main() {}
