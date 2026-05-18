use dioxus::prelude::*;

#[get("/api/me")]
pub async fn who_am_i() -> Result<Option<String>, ServerFnError> {
    Ok(None)
}
