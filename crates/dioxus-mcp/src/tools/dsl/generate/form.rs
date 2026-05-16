use std::path::Path;

use heck::{ToPascalCase, ToSnakeCase};
use minijinja::context;

use crate::tools::scaffold::ScaffoldResult;

use super::super::render::*;
use super::super::templates::*;
use super::super::types::*;
use super::{field_initial, humanize};

pub(crate) fn generate_form(crate_root: &Path, f: &DslForm) -> Result<ScaffoldResult, String> {
    let pascal = f.name.to_pascal_case();
    let snake = f.name.to_snake_case();

    let snake_field_names: Vec<String> =
        f.fields.iter().map(|fd| fd.name.to_snake_case()).collect();
    let snapshots = snake_field_names
        .iter()
        .map(|n| format!("                let {n}_v = {n}();"))
        .collect::<Vec<_>>()
        .join("\n");
    let arg_call = snake_field_names
        .iter()
        .map(|n| format!("{n}_v"))
        .collect::<Vec<_>>()
        .join(", ");
    let resets = f
        .fields
        .iter()
        .map(|fd| {
            let n = fd.name.to_snake_case();
            let init = field_initial(&fd.ty);
            format!("                        {n}.set({init});")
        })
        .collect::<Vec<_>>()
        .join("\n");

    let on_submit_body = match (&f.on_submit, &f.feeds_into) {
        (Some(h), Some(_)) => {
            let h = h.to_snake_case();
            let mut out = String::new();
            if !snapshots.is_empty() {
                out.push_str(&snapshots);
                out.push('\n');
            }
            out.push_str(&format!(
                "                spawn(async move {{\n                    if {h}({arg_call}).await.is_ok() {{\n"
            ));
            if !resets.is_empty() {
                out.push_str(&resets);
                out.push('\n');
            }
            out.push_str(
                "                        *version.write() += 1;\n                    }\n                });",
            );
            out
        }
        (Some(h), None) => {
            let h = h.to_snake_case();
            let mut out = String::new();
            if !snapshots.is_empty() {
                out.push_str(&snapshots);
                out.push('\n');
            }
            out.push_str(&format!(
                "                spawn(async move {{\n                    let _ = {h}({arg_call}).await;\n                }});"
            ));
            out
        }
        (None, Some(_)) => {
            "                // TODO submit handler\n                *version.write() += 1;"
                .to_string()
        }
        (None, None) => "                // TODO submit handler".to_string(),
    };

    let fields_ctx: Vec<_> = f
        .fields
        .iter()
        .map(|fd| {
            let initial = field_initial(&fd.ty);
            let input_type = match fd.ty.as_str() {
                "email" => "email",
                "password" => "password",
                "number" => "number",
                "checkbox" => "checkbox",
                "textarea" => "text",
                _ => "text",
            };
            let tag = if fd.ty == "textarea" {
                "textarea"
            } else {
                "input"
            };
            let validation = fd.validation.clone().unwrap_or_default();
            context! {
                name => fd.name.to_snake_case(),
                label => humanize(&fd.name),
                input_type => input_type,
                tag => tag,
                initial => initial,
                validation => validation,
            }
        })
        .collect();
    let feeds_into_snake = f.feeds_into.as_ref().map(|s| s.to_snake_case());
    let handler = f.on_submit.as_ref().map(|s| s.to_snake_case());
    let needs_handler_import = handler.is_some();
    let body = render(
        "form",
        FORM_TPL,
        context! {
            pascal => pascal.clone(),
            fields => fields_ctx,
            on_submit_body => on_submit_body,
            handler => handler,
            needs_handler_import => needs_handler_import,
            feeds_into_snake => feeds_into_snake,
        },
    )?;
    let mut r = write_component_file(crate_root, &snake, body)?;
    r.next_steps.push(format!(
        "import the form: `use crate::components::{pascal};`"
    ));
    if let Some(target) = &f.feeds_into {
        let t = target.to_snake_case();
        r.next_steps.push(format!(
            "render `{pascal}` inside the same parent that calls `provide_{t}_version()` so both share the version signal"
        ));
    }
    Ok(r)
}
