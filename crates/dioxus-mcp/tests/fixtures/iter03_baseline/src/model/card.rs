use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Eq)]
pub struct Card {
    pub id: String,
    pub title: String,
    pub column: String,
    pub position: i32,
    pub author: String,
}
