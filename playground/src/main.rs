use dioxus::prelude::*;

mod app;
mod interpreter;
mod mcp_client;
mod model;

const MAIN_CSS: Asset = asset!("/assets/main.css");
const PREVIEW_CSS: Asset = asset!("/assets/preview.css");

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    rsx! {
        document::Link { rel: "stylesheet", href: MAIN_CSS }
        document::Link { rel: "stylesheet", href: PREVIEW_CSS }
        app::Cockpit {}
    }
}
