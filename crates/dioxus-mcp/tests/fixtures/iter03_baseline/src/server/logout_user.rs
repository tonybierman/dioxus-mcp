use dioxus::prelude::*;

#[post("/api/logout", cookies: axum_extra::TypedHeader<axum_extra::headers::Cookie>)]
pub async fn logout_user() -> Result<(), ServerFnError> {
    #[cfg(feature = "server")]
    {
        let sid = cookies.get("sid").unwrap_or_default().to_string();
        let mut sessions = crate::server::state::SESSIONS.lock().unwrap();
        sessions.remove(&sid);
    }
    Ok(())
}
