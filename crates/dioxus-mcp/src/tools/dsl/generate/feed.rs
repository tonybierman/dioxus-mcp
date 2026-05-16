use std::path::Path;

use heck::{ToPascalCase, ToSnakeCase};
use minijinja::context;

use crate::tools::scaffold::ScaffoldResult;

use super::super::render::*;
use super::super::templates::*;
use super::super::types::*;

pub(crate) fn generate_feed(crate_root: &Path, f: &DslFeed) -> Result<ScaffoldResult, String> {
    let pascal = f.name.to_pascal_case();
    let snake = f.name.to_snake_case();
    let socket_snake = f.socket.to_snake_case();
    let socket_pascal = f.socket.to_pascal_case();
    let body = render(
        "feed",
        FEED_TPL,
        context! {
            pascal => pascal,
            snake => snake.clone(),
            socket => socket_snake,
            socket_pascal => socket_pascal,
            item_type => f.item_type.clone(),
        },
    )?;
    write_component_file(crate_root, &snake, body)
}
