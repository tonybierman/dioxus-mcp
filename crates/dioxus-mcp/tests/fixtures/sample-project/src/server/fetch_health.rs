// Demonstrates: project_index / openapi_spec attribute-form detection
// (#[get("/health")] is Dioxus 0.7's idiomatic style).
use dioxus::prelude::*;

#[get("/health")]
pub async fn fetch_health() -> Result<String, ServerFnError> {
    Ok("ok".into())
}
