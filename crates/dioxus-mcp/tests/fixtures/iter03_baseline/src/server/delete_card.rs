use dioxus::prelude::*;

#[post("/api/delete", cookies: axum_extra::TypedHeader<axum_extra::headers::Cookie>)]
pub async fn delete_card(id: String) -> Result<(), ServerFnError> {
    if crate::server::state::user_from_cookies(&cookies).is_none() {
        return Err(ServerFnError::new("not logged in"));
    }
    let _ = id;
    Ok(())
}
