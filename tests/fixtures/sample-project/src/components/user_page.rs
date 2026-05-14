// Demonstrates: server_fn_call_graph (UserPage calls fetch_user)
use dioxus::prelude::*;
use crate::server::fetch_user;

#[component]
pub fn UserPage(id: i32) -> Element {
    let _user = use_resource(move || fetch_user(id));
    rsx! { div { "user {id}" } }
}
