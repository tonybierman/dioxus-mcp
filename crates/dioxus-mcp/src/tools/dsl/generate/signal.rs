use std::path::Path;

use heck::ToSnakeCase;
use minijinja::context;

use crate::tools::scaffold::ScaffoldResult;

use super::super::render::*;
use super::super::templates::*;
use super::super::types::*;

pub(crate) fn generate_signal(crate_root: &Path, s: &DslSignal) -> Result<ScaffoldResult, String> {
    let snake = s.name.to_snake_case();
    let body = render(
        "signal",
        SIGNAL_TPL,
        context! {
            snake => snake.clone(),
            ty => s.ty.clone(),
            initial => s.initial.clone(),
        },
    )?;
    write_module_file(crate_root, "src/signals", &snake, body)
}
