// Demonstrates: server_fn_call_graph (no callers — appears in orphans)
use dioxus::prelude::*;

#[server(OrphanFn)]
pub async fn orphan_fn() -> ServerFnResult<()> {
    Ok(())
}
