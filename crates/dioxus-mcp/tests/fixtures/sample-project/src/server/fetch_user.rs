// Demonstrates: project_index (server fn with arg + ServerFnResult<T>),
//                server_fn_call_graph (called from UserPage)
use dioxus::prelude::*;

#[server(FetchUser)]
pub async fn fetch_user(id: i32) -> ServerFnResult<String> {
    Ok(format!("user {id}"))
}
