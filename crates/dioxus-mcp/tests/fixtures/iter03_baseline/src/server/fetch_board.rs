use dioxus::prelude::*;
use crate::model::card::Card;

#[get("/api/board", cookies: axum_extra::TypedHeader<axum_extra::headers::Cookie>)]
pub async fn fetch_board() -> Result<Vec<Card>, ServerFnError> {
    // repeated_auth_extractor: identity check duplicated across 3+ server fns.
    if crate::server::state::user_from_cookies(&cookies).is_none() {
        return Err(ServerFnError::new("not logged in"));
    }
    Ok(Vec::new())
}
