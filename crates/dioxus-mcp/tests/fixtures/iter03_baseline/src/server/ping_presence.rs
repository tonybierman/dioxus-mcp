use dioxus::prelude::*;

#[post("/api/ping", cookies: axum_extra::TypedHeader<axum_extra::headers::Cookie>)]
pub async fn ping_presence(name: String) -> Result<Vec<String>, ServerFnError> {
    #[cfg(feature = "server")]
    {
        use std::time::Instant;
        // auth_map: unwrap_or_default on a security header value.
        let sid = cookies.get("sid").unwrap_or_default().to_string();
        let mut presence = crate::server::state::PRESENCE.lock().unwrap();
        presence.insert(sid, (Instant::now(), name.clone()));
    }
    let _ = name;
    Ok(Vec::new())
}
