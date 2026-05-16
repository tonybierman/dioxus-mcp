use std::collections::BTreeSet;
use std::path::Path;

use heck::{ToPascalCase, ToSnakeCase};
use minijinja::context;

use crate::tools::scaffold::ScaffoldResult;

use super::super::render::*;
use super::super::templates::*;
use super::super::types::*;

pub(crate) fn generate_protected_route(
    crate_root: &Path,
    pr: &DslProtectedRoute,
    session_names: &BTreeSet<String>,
) -> Result<ScaffoldResult, String> {
    let pascal = pr.name.to_pascal_case();
    let snake = pr.name.to_snake_case();
    let session_snake = match &pr.requires {
        Some(s) => Some(s.to_snake_case()),
        None => session_names.iter().next().cloned(),
    };
    let body = render(
        "protected",
        PROTECTED_TPL,
        context! {
            pascal => pascal,
            redirect_to => pr.redirect_to.clone(),
            session_snake => session_snake.clone(),
        },
    )?;
    let mut r = write_component_file(crate_root, &snake, body)?;
    if session_snake.is_some() {
        r.next_steps.push(
            "make sure the SessionState's `provide_*` is called above any route wrapped by this guard".into(),
        );
    } else {
        r.next_steps.push(
            "no SessionState in the doc — wire your own session signal where the guard reads it"
                .into(),
        );
    }
    Ok(r)
}
