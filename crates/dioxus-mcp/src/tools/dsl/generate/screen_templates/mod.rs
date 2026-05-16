//! Screen-body renderers split by template `kind`. The dispatcher in
//! `plain.rs::render_screen_template` routes to `client_crud`/`resource_crud`
//! when the doc names those kinds; otherwise it renders the plain SCREEN_TPL
//! inline.

mod client_crud;
mod plain;
mod resource_crud;

pub(crate) use plain::*;
