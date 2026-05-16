use std::path::Path;

use heck::{ToPascalCase, ToSnakeCase};
use minijinja::context;

use crate::tools::scaffold::ScaffoldResult;

use super::super::render::*;
use super::super::templates::*;
use super::super::types::*;

pub(crate) fn generate_table(crate_root: &Path, t: &DslTable) -> Result<ScaffoldResult, String> {
    let pascal = t.name.to_pascal_case();
    let snake = t.name.to_snake_case();
    let endpoint = t.endpoint.to_snake_case();
    let cols: Vec<_> = t
        .columns
        .iter()
        .map(|c| {
            context! { name => c.name.clone(), label => c.label.clone() }
        })
        .collect();
    let body = render(
        "table",
        TABLE_TPL,
        context! {
            pascal => pascal.clone(),
            snake => snake.clone(),
            endpoint => endpoint,
            item_type => t.item_type.clone(),
            columns => cols,
        },
    )?;
    write_component_file(crate_root, &snake, body)
}
