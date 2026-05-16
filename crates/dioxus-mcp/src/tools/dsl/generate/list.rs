use std::path::Path;

use heck::{ToPascalCase, ToSnakeCase};
use minijinja::context;

use crate::tools::scaffold::ScaffoldResult;

use super::super::render::*;
use super::super::templates::*;
use super::super::types::*;

pub(crate) fn generate_list(
    crate_root: &Path,
    l: &DslList,
    versioned: bool,
) -> Result<ScaffoldResult, String> {
    let pascal = l.name.to_pascal_case();
    let snake = l.name.to_snake_case();
    let endpoint = l.endpoint.to_snake_case();
    let body = render(
        "list",
        LIST_TPL,
        context! {
            pascal => pascal.clone(),
            snake => snake.clone(),
            endpoint => endpoint,
            item_type => l.item_type.clone(),
            versioned => versioned,
        },
    )?;
    let mut r = write_component_file(crate_root, &snake, body)?;
    if versioned {
        r.next_steps.push(format!(
            "call `crate::components::{snake}::provide_{snake}_version()` in the screen that hosts this list (and any forms feeding into it) before rendering them"
        ));
    }
    Ok(r)
}
