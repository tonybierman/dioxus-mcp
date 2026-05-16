use std::path::Path;

use heck::ToSnakeCase;
use minijinja::context;

use crate::tools::scaffold::ScaffoldResult;

use super::super::render::*;
use super::super::templates::*;
use super::super::types::*;

pub(crate) fn generate_session(
    crate_root: &Path,
    s: &DslSessionState,
) -> Result<ScaffoldResult, String> {
    let snake = s.name.to_snake_case();
    let body = render(
        "session",
        SESSION_TPL,
        context! {
            snake => snake.clone(),
            user_type => s.user_type.clone(),
        },
    )?;
    write_module_file(crate_root, "src/auth", &snake, body)
}
