use dioxus::prelude::*;
pub mod components;
pub mod model;
pub mod server;

fn main() {
    dioxus::launch(App);
}

// Bootstrap-gate shape: a `use_signal(|| false)` flipped to `true` after
// awaiting a server fn AND gating the whole rsx subtree. Triggers
// `signal_lint`'s `bootstrap_gate_signal` finding.
#[component]
fn App() -> Element {
    let mut bootstrapped = use_signal(|| false);

    use_effect(move || {
        spawn(async move {
            let _ = server::who_am_i().await;
            bootstrapped.set(true);
        });
    });

    rsx! {
        if bootstrapped() {
            components::board_screen::BoardScreen {}
        } else {
            div { class: "boot", "Loading..." }
        }
    }
}
