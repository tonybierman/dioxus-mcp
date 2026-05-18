use dioxus::prelude::*;
use crate::model::card::Card;

// shared_enum_validation: client COLUMNS array matches server pattern.
#[post("/api/create", cookies: axum_extra::TypedHeader<axum_extra::headers::Cookie>)]
pub async fn create_card(title: String, column: String) -> Result<Card, ServerFnError> {
    let _user = crate::server::state::user_from_cookies(&cookies)
        .ok_or_else(|| ServerFnError::new("not logged in"))?;
    match column.as_str() {
        "todo" | "doing" | "done" => Ok(Card {
            id: "1".into(),
            title,
            column,
            position: 0,
            author: "you".into(),
        }),
        _ => Err(ServerFnError::ServerError("bad column".into())),
    }
}
