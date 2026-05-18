use dioxus::prelude::*;
use crate::model::card::Card;

#[get("/api/board")]
pub async fn fetch_board() -> Result<Vec<Card>, ServerFnError> {
    Ok(Vec::new())
}
