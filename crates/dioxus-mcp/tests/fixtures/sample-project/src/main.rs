// Demonstrates: asset_audit, project_index (App component)
use dioxus::prelude::*;

mod components;
mod router;
mod server;
mod lint_demo;

#[component]
pub fn App() -> Element {
    rsx! {
        // asset!() nested inside rsx! — exercises the macro-token walker.
        // `logo.png` exists; `missing.svg` is the deliberate hole picked up
        // in `missing_assets`. (`orphan.css` is intentionally never
        // referenced — see the unreferenced_files assertion.)
        img { src: asset!("/assets/logo.png") }
        img { src: asset!("/assets/missing.svg") }
        Router::<router::Route> {}
    }
}

fn main() {}
