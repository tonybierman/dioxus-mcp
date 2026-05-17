//! Per-primitive generators. Each sub-module owns the rendering for one
//! DSL primitive (a Form, a Screen, a Store, …); the orchestrator in
//! `execute.rs` dispatches to them and merges their `ScaffoldResult`s.
//!
//! Small cross-cutting helpers (`field_initial`, `humanize`) live here
//! because they're shared between several siblings.

mod feed;
mod form;
mod list;
mod login_screen;
mod model;
mod protected_route;
mod screen;
mod screen_templates;
mod session;
mod signal;
mod socket;
mod store;
mod table;
mod view_state;

pub(crate) use feed::*;
pub(crate) use form::*;
pub(crate) use list::*;
pub(crate) use login_screen::*;
pub(crate) use model::*;
pub(crate) use protected_route::*;
pub(crate) use screen::*;
#[allow(unused_imports)]
pub(crate) use screen_templates::*;
pub(crate) use session::*;
pub(crate) use signal::*;
pub(crate) use socket::*;
pub(crate) use store::*;
pub(crate) use table::*;
pub(crate) use view_state::*;

/// Default initial-value expression for a form field, keyed off the DSL `ty`
/// keyword. Shared between `generate_form` and the `resource_form` template.
pub(super) fn field_initial(ty: &str) -> &'static str {
    match ty {
        "checkbox" => "false",
        "number" => "0i64",
        _ => "String::new()",
    }
}

/// "stock_movement" or "StockMovement" → "Stock movement". Used for h1 / link
/// text on synthesized CRUD screens and humanized form labels.
pub(super) fn humanize(s: &str) -> String {
    use heck::ToSnakeCase;
    let snake = s.to_snake_case();
    let mut out = String::with_capacity(snake.len());
    for (i, ch) in snake.chars().enumerate() {
        if ch == '_' {
            out.push(' ');
        } else if i == 0 {
            for u in ch.to_uppercase() {
                out.push(u);
            }
        } else {
            out.push(ch);
        }
    }
    out
}
