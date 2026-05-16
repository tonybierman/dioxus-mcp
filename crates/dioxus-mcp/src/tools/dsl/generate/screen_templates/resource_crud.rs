use std::path::Path;

use heck::ToSnakeCase;
use minijinja::context;

use crate::tools::scaffold;

use super::super::super::render::*;
use super::super::super::templates::*;
use super::super::super::types::*;
use super::super::humanize;

/// Locate the Routable enum on disk and return the import path callers can use
/// from a sibling component file (e.g. "crate::Route" when the enum is in
/// main.rs / lib.rs; "crate::router::Route" when in src/router.rs). Returns
/// None when no Routable enum is found, in which case the list template falls
/// back to plain `<a href>` links to avoid emitting un-compilable code.
pub(crate) fn detect_route_import(crate_root: &Path) -> Option<(String, String)> {
    let path = scaffold::find_routable(crate_root)?;
    let src_rel = path.strip_prefix(crate_root.join("src")).ok()?;
    let src = std::fs::read_to_string(&path).ok()?;
    let file = syn::parse_file(&src).ok()?;
    let enum_name = file.items.iter().find_map(|it| match it {
        syn::Item::Enum(e) => {
            let has_routable = e.attrs.iter().any(|a| {
                if !a.path().is_ident("derive") {
                    return false;
                }
                let mut found = false;
                let _ = a.parse_nested_meta(|m| {
                    if m.path.is_ident("Routable") {
                        found = true;
                    }
                    Ok(())
                });
                found
            });
            if has_routable {
                Some(e.ident.to_string())
            } else {
                None
            }
        }
        _ => None,
    })?;
    // Module path from crate root: drop the trailing `.rs`, treat `main` /
    // `lib` as the crate root (no module prefix), otherwise build
    // `crate::a::b::Enum` from the parent dirs + filename stem.
    let stem = src_rel.file_stem()?.to_str()?;
    let parent_components: Vec<String> = src_rel
        .parent()
        .into_iter()
        .flat_map(|p| p.components())
        .filter_map(|c| match c {
            std::path::Component::Normal(n) => n.to_str().map(String::from),
            _ => None,
        })
        .collect();
    let import = if (stem == "main" || stem == "lib") && parent_components.is_empty() {
        format!("crate::{enum_name}")
    } else {
        let mut segs = parent_components;
        segs.push(stem.to_string());
        format!("crate::{}::{}", segs.join("::"), enum_name)
    };
    Some((import, enum_name))
}

pub(crate) fn render_resource_crud_list(
    crate_root: &Path,
    pascal: &str,
    snake: &str,
    wrap_pascal: Option<&str>,
    crud: &CrudCtx,
) -> Result<String, String> {
    let columns: Vec<_> = crud
        .model_fields
        .iter()
        .map(|f| {
            let inner = strip_option(&f.ty).unwrap_or(&f.ty);
            let optional = f.optional || strip_option(&f.ty).is_some();
            // Non-Display fallback: custom types may not impl Display, so use
            // Debug. Users can post-edit if they want a different format.
            let is_primitive = matches!(
                inner,
                "String"
                    | "bool"
                    | "i8"
                    | "i16"
                    | "i32"
                    | "i64"
                    | "i128"
                    | "isize"
                    | "u8"
                    | "u16"
                    | "u32"
                    | "u64"
                    | "u128"
                    | "usize"
                    | "f32"
                    | "f64"
                    | "char"
            );
            let name = f.name.to_snake_case();
            // For Option<T> we want a *value* in the cell, not `Some(...)` /
            // `None` (Debug formatting); reach into the Option and render the
            // inner via Display (or empty string for None).
            let cell = if optional {
                if is_primitive {
                    format!("{{row.{name}.as_ref().map(|v| v.to_string()).unwrap_or_default()}}")
                } else {
                    // Non-Display inner — fall back to Debug of the inner value,
                    // still avoiding the Some(..)/None wrapper.
                    format!("{{row.{name}.as_ref().map(|v| format!(\"{{:?}}\", v)).unwrap_or_default()}}")
                }
            } else if is_primitive {
                format!("{{row.{name}}}")
            } else {
                format!("{{row.{name}:?}}")
            };
            context! {
                name => name,
                label => humanize(&f.name),
                cell => cell,
            }
        })
        .collect();
    // Build SPA-friendly Link expressions when we can resolve the Route enum
    // import path. Fall back to plain `a { href: ... }` when no Routable enum
    // is on disk (no router file yet) — that's at least correct.
    let route_link = detect_route_import(crate_root).map(|(import_path, enum_name)| {
        let new_variant = format!("{}NewScreen", crud.model_pascal);
        let edit_variant = format!("{}EditScreen", crud.model_pascal);
        context! {
            import_path => import_path,
            enum_name => enum_name,
            new_variant => new_variant,
            edit_variant => edit_variant,
            id_field => crud.id_field.clone(),
        }
    });

    render(
        "screen_resource_crud_list",
        SCREEN_RESOURCE_CRUD_LIST_TPL,
        context! {
            pascal => pascal,
            snake => snake,
            wrap_pascal => wrap_pascal,
            list_endpoint => crud.list_endpoint.clone(),
            delete_endpoint => crud.delete_endpoint.clone(),
            new_route => crud.new_route.clone(),
            list_route => crud.list_route.clone(),
            id_field => crud.id_field.clone(),
            humanized => humanize(&crud.model_pascal),
            columns => columns,
            route_link => route_link,
        },
    )
}

pub(crate) fn render_resource_edit_form(
    pascal: &str,
    snake: &str,
    wrap_pascal: Option<&str>,
    t: &DslScreenTemplate,
    crud: &CrudCtx,
) -> Result<String, String> {
    let fields_ctx: Vec<_> = t
        .fields
        .iter()
        .map(|fd| {
            let is_bool = fd.ty == "checkbox" || fd.rust_type.as_deref() == Some("bool");
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
            let signal_init_from_item = signal_init_from_item(fd);
            context! {
                name => fd.name.to_snake_case(),
                label => humanize(&fd.name),
                input_type => input_type,
                tag => tag,
                is_bool => is_bool,
                signal_init_from_item => signal_init_from_item,
            }
        })
        .collect();

    let submit_body = resource_edit_form_submit_body(t, crud);

    render(
        "screen_resource_edit_form",
        SCREEN_RESOURCE_EDIT_FORM_TPL,
        context! {
            pascal => pascal,
            snake => snake,
            wrap_pascal => wrap_pascal,
            model_pascal => crud.model_pascal.clone(),
            id_field => crud.id_field.clone(),
            id_type => crud.id_type.clone(),
            get_endpoint => crud.get_endpoint.clone(),
            update_endpoint => crud.update_endpoint.clone(),
            fields => fields_ctx,
            submit_body => submit_body,
        },
    )
}

/// Build the `use_signal(|| ...)` initializer expression for an edit-form
/// signal pre-populated from a loaded `item: Model`. Branches on the field's
/// rust_type + optional metadata.
pub(crate) fn signal_init_from_item(f: &DslFieldDef) -> String {
    let rust_ty = f.rust_type.as_deref().unwrap_or("String");
    let inner = strip_option(rust_ty).unwrap_or(rust_ty);
    let optional = f.optional || strip_option(rust_ty).is_some();
    let field_name = f.name.to_snake_case();
    let is_bool = inner == "bool";
    let is_string = inner == "String";

    if is_bool {
        return if optional {
            format!("item.{field_name}.unwrap_or(false)")
        } else {
            format!("item.{field_name}")
        };
    }
    if is_string {
        return if optional {
            format!("item.{field_name}.clone().unwrap_or_default()")
        } else {
            format!("item.{field_name}.clone()")
        };
    }
    // Numeric (or unknown): store as String so the input is editable.
    if optional {
        format!("item.{field_name}.map(|v| v.to_string()).unwrap_or_default()")
    } else {
        format!("item.{field_name}.to_string()")
    }
}

/// Build the submit body for the edit form. Preserves the original id and
/// calls the update_* server fn. Navigates to the list route on success.
pub(crate) fn resource_edit_form_submit_body(t: &DslScreenTemplate, crud: &CrudCtx) -> String {
    let indent = "                ";
    let mut out = String::new();
    for f in &t.fields {
        let n = f.name.to_snake_case();
        out.push_str(&format!("{indent}let {n}_v = {n}();\n"));
    }
    out.push_str(&format!("{indent}let id_v = original_id.clone();\n"));
    out.push_str(&format!("{indent}let item = {} {{\n", crud.model_pascal));
    out.push_str(&format!("{indent}    {}: id_v,\n", crud.id_field));
    for f in &t.fields {
        let n = f.name.to_snake_case();
        let val = field_submit_expr(f, &format!("{n}_v"));
        out.push_str(&format!("{indent}    {n}: {val},\n"));
    }
    out.push_str(&format!("{indent}    ..Default::default()\n"));
    out.push_str(&format!("{indent}}};\n"));
    let nav_line = format!("{indent}        nav.push(\"{}\");\n", crud.list_route);
    out.push_str(&format!(
        "{indent}spawn(async move {{\n{indent}    if {}(item).await.is_ok() {{\n{nav_line}{indent}    }}\n{indent}}});",
        crud.update_endpoint
    ));
    out
}

/// Build the rust body that runs inside the form's onsubmit handler.
/// When `item_type` is set we attempt to construct it from the field signals
/// and call the submit fn with it. Otherwise we emit a TODO body.
///
/// Each field's submit-side expression is computed from its
/// `rust_type` + `optional` metadata (populated by `expand_resources` from the
/// source model). This produces compiling code for `String`, `Option<String>`,
/// integer/float (parsed from the String-backed signal), their Option variants,
/// and `bool`.
pub(crate) fn resource_form_submit_body(t: &DslScreenTemplate, submit: &str) -> String {
    let indent = "                ";
    let mut out = String::new();
    let has_item = t.item_type.is_some() && !t.fields.is_empty();

    if !t.fields.is_empty() {
        for f in &t.fields {
            let n = f.name.to_snake_case();
            out.push_str(&format!("{indent}let {n}_v = {n}();\n"));
        }
    }

    if has_item {
        let item_type = t.item_type.as_deref().unwrap();
        out.push_str(&format!("{indent}let item = {item_type} {{\n"));
        // Field assignment driven by the original Rust type when known.
        for f in &t.fields {
            let n = f.name.to_snake_case();
            let val = field_submit_expr(f, &format!("{n}_v"));
            out.push_str(&format!("{indent}    {n}: {val},\n"));
        }
        out.push_str(&format!("{indent}    ..Default::default()\n"));
        out.push_str(&format!("{indent}}};\n"));
        let nav_line = match &t.redirect_to {
            Some(r) => format!("{indent}        nav.push(\"{r}\");\n"),
            None => String::new(),
        };
        out.push_str(&format!(
            "{indent}spawn(async move {{\n{indent}    if {submit}(item).await.is_ok() {{\n{nav_line}{indent}    }}\n{indent}}});"
        ));
    } else if !t.fields.is_empty() {
        let arg_call = t
            .fields
            .iter()
            .map(|f| format!("{}_v", f.name.to_snake_case()))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "{indent}spawn(async move {{\n{indent}    let _ = {submit}({arg_call}).await;\n{indent}}});"
        ));
    } else {
        out.push_str(&format!(
            "{indent}// TODO call {submit}(...). Add `fields:` to the template to scaffold signals + inputs."
        ));
    }

    out
}

/// Build the Rust expression that converts a String-backed (or bool-backed)
/// signal snapshot into the model field's actual type. `signal_var` is the
/// local that already holds the snapshot (e.g. `"name_v"`).
pub(crate) fn field_submit_expr(f: &DslFieldDef, signal_var: &str) -> String {
    let rust_ty = f.rust_type.as_deref().unwrap_or("String");
    let inner = strip_option(rust_ty).unwrap_or(rust_ty);
    let is_numeric = matches!(
        inner,
        "i8" | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f32"
            | "f64"
    );
    let is_bool = inner == "bool";
    let is_string = inner == "String";

    let optional = f.optional || strip_option(rust_ty).is_some();

    if is_bool {
        // bool-backed signal already holds a bool — no parsing needed.
        return if optional {
            format!("Some({signal_var})")
        } else {
            signal_var.to_string()
        };
    }

    if is_numeric {
        let parse_expr = format!("{signal_var}.parse::<{inner}>().unwrap_or_default()");
        return if optional {
            format!(
                "if {signal_var}.is_empty() {{ None }} else {{ {signal_var}.parse::<{inner}>().ok() }}"
            )
        } else {
            parse_expr
        };
    }

    if is_string {
        return if optional {
            format!("if {signal_var}.is_empty() {{ None }} else {{ Some({signal_var}) }}")
        } else {
            signal_var.to_string()
        };
    }

    // Unknown type — fall back to a parse attempt for non-optional, or a TODO
    // wrapper for optional. The generated file is meant to be edited if the
    // model uses a custom type.
    if optional {
        format!(
            "if {signal_var}.is_empty() {{ None }} else {{ {signal_var}.parse::<{inner}>().ok() }}"
        )
    } else {
        format!("{signal_var}.parse::<{inner}>().unwrap_or_default()")
    }
}

/// If `ty` is an `Option<T>` (textually, with optional whitespace) returns `Some("T")`;
/// otherwise returns `None`. Naive, but adequate for the type strings we emit
/// from models (e.g. `Option<String>`, `Option<i64>`).
pub(crate) fn strip_option(ty: &str) -> Option<&str> {
    let t = ty.trim();
    let inner = t.strip_prefix("Option<")?.strip_suffix('>')?;
    Some(inner.trim())
}
