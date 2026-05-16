use std::path::Path;

use heck::{ToPascalCase, ToSnakeCase};
use minijinja::context;

use crate::tools::scaffold::ScaffoldResult;

use super::super::render::*;
use super::super::templates::*;
use super::super::types::*;

pub(crate) fn generate_socket(crate_root: &Path, s: &DslSocket) -> Result<ScaffoldResult, String> {
    let snake = s.name.to_snake_case();
    let pascal = s.name.to_pascal_case();
    let upper = snake.to_uppercase();
    let body = render(
        "socket",
        SOCKET_TPL,
        context! {
            pascal => pascal,
            snake => snake.clone(),
            upper => upper,
            url => s.url.clone(),
        },
    )?;
    write_module_file(crate_root, "src/sockets", &snake, body)
}
