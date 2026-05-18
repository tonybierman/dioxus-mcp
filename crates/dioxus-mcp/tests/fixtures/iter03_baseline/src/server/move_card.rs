use dioxus::prelude::*;

// shared_enum_validation: pattern matches the client's COLUMNS const.
#[post("/api/move")]
pub async fn move_card(
    id: String,
    column: String,
    position: i32,
) -> Result<(), ServerFnError> {
    match column.as_str() {
        "todo" | "doing" | "done" => Ok(()),
        _ => Err(ServerFnError::ServerError("bad column".into())),
    }
    .map(|_| {
        let _ = (id, position);
    })
}
