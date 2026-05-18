use dioxus::prelude::*;

#[post("/api/login")]
pub async fn login_user(name: String) -> Result<(), ServerFnError> {
    #[cfg(feature = "server")]
    {
        let mut sessions = crate::server::state::SESSIONS.lock().unwrap();
        sessions.insert("sid".into(), name.clone());
    }
    let _ = name;
    if let Some(ctx) = dioxus::fullstack::FullstackContext::current() {
        // insecure_set_cookie: SameSite=Lax + Secure on a session cookie —
        // triggers `samesite_lax_session_hint`.
        let value = http::HeaderValue::from_static(
            "sid=abc123; Path=/; HttpOnly; SameSite=Lax; Secure; Max-Age=86400",
        );
        ctx.add_response_header(http::header::SET_COOKIE, value);
    }
    Ok(())
}
