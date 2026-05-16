use std::path::Path;
use std::sync::Arc;

use heck::{ToPascalCase, ToSnakeCase};
use minijinja::context;

use crate::state::State;
use crate::tools::scaffold::{self, CreateRouteParams, ScaffoldResult};

use super::super::render::*;
use super::super::templates::*;
use super::super::types::*;
use super::super::util::merge;

pub(crate) async fn generate_login_screen(
    state: &Arc<State>,
    crate_root: &Path,
    ls: &DslLoginScreen,
    project_root: Option<&str>,
) -> Result<ScaffoldResult, String> {
    let pascal = ls.name.to_pascal_case();
    let snake = ls.name.to_snake_case();
    let body = render(
        "login",
        LOGIN_TPL,
        context! {
            pascal => pascal.clone(),
            redirect => ls.redirect_on_success.clone(),
        },
    )?;
    let mut r = write_component_file(crate_root, &snake, body)?;
    let route = scaffold::create_route(
        state,
        CreateRouteParams {
            path: ls.route.clone(),
            component: pascal.clone(),
            router_file: None,
            project_root: project_root.map(str::to_owned),
            params: Vec::new(),
            import_path: Some("crate::components".to_string()),
        },
    )
    .await?;
    merge(&mut r, route);
    Ok(r)
}
