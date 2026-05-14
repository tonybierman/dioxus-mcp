// Demonstrates: openapi_spec (struct request/response types resolved from local definitions)
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListPostsInput {
    pub limit: u32,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub body: String,
}

#[server(ListPosts)]
pub async fn list_posts(input: ListPostsInput) -> ServerFnResult<Vec<Post>> {
    let _ = input;
    Ok(Vec::new())
}
