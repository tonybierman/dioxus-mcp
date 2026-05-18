use dioxus::prelude::*;

#[post("/api/delete")]
pub async fn delete_card(id: String) -> Result<(), ServerFnError> {
    let _ = id;
    Ok(())
}
