use dioxus::prelude::*;
use crate::model::card::Card;

// shared_enum_validation: client COLUMNS array matches server pattern.
#[post("/api/create")]
pub async fn create_card(title: String, column: String) -> Result<Card, ServerFnError> {
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
