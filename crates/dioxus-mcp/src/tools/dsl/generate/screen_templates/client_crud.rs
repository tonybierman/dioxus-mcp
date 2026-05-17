use heck::ToSnakeCase;
use minijinja::context;

use super::super::super::render::*;
use super::super::super::templates::*;
use super::super::super::types::*;
use super::super::humanize;
use super::super::store::is_primitive_integer_ty;

/// Bag of class strings / attribute snippets per design-system preset for the
/// `client_crud` template. Keeps the rendering loop above readable instead of
/// fanning out the same `if styled == "tailwind"` branch six times.
struct ClientCrudStyle {
    form_class: String,
    list_class_override: Option<String>,
    input_class: Option<String>,
    submit_button_class: Option<String>,
    checkbox_class: Option<String>,
    delete_button_class: String,
    extra_h1_attrs: Option<String>,
    extra_li_attrs: Option<String>,
    extra_label_attrs: Option<String>,
}

impl ClientCrudStyle {
    /// Historical unstyled markup: `class: "add"`, `class: "{snake}-items"`,
    /// `class: "delete"`. Kept as the default so existing apps don't change.
    fn default_unstyled(_snake: &str) -> Self {
        Self {
            form_class: "add".into(),
            list_class_override: None,
            input_class: None,
            submit_button_class: None,
            checkbox_class: None,
            delete_button_class: "delete".into(),
            extra_h1_attrs: None,
            extra_li_attrs: None,
            extra_label_attrs: None,
        }
    }

    /// Tailwind-classed defaults: small max-w container, neutral colors,
    /// hover/focus states. Deliberately conservative — should look intentional
    /// in any Tailwind project without committing to a theme.
    fn tailwind() -> Self {
        Self {
            form_class: "flex gap-2 mb-4".into(),
            list_class_override: Some("space-y-2".into()),
            input_class: Some(
                "flex-1 px-3 py-2 border border-gray-300 rounded-md focus:outline-none focus:ring-2 focus:ring-blue-500".into(),
            ),
            submit_button_class: Some(
                "px-4 py-2 bg-blue-600 text-white rounded-md hover:bg-blue-700 focus:outline-none focus:ring-2 focus:ring-blue-500".into(),
            ),
            checkbox_class: Some("h-4 w-4 text-blue-600 rounded border-gray-300".into()),
            delete_button_class: "text-red-600 hover:text-red-800 text-sm font-medium".into(),
            extra_h1_attrs: Some("class: \"text-2xl font-semibold mb-4\", ".into()),
            extra_li_attrs: Some(
                " class: \"flex items-center gap-3 p-2 bg-white border border-gray-200 rounded-md\",".into(),
            ),
            extra_label_attrs: Some("class: \"flex-1\", ".into()),
        }
    }

    /// Vanilla-CSS preset: semantic-feeling class names matched to a starter
    /// stylesheet emitted alongside the screen. Gives the agent a styling
    /// surface to extend without inventing class names from scratch. The
    /// matching CSS file is written by the caller via
    /// `vanilla_css_starter_for`.
    fn vanilla_css(_snake: &str) -> Self {
        Self {
            form_class: "compose".into(),
            list_class_override: Some("list".into()),
            input_class: Some("field".into()),
            submit_button_class: Some("submit".into()),
            checkbox_class: Some("toggle".into()),
            delete_button_class: "delete".into(),
            extra_h1_attrs: Some("class: \"title\", ".into()),
            extra_li_attrs: Some(" class: \"row\",".into()),
            extra_label_attrs: Some("class: \"label\", ".into()),
        }
    }

    fn h1_attrs(&self) -> &str {
        self.extra_h1_attrs.as_deref().unwrap_or("")
    }

    fn li_attrs(&self) -> &str {
        self.extra_li_attrs.as_deref().unwrap_or("")
    }

    fn label_span_attrs(&self) -> &str {
        self.extra_label_attrs.as_deref().unwrap_or("")
    }

    fn list_class(&self, snake: &str) -> String {
        match &self.list_class_override {
            Some(s) => s.clone(),
            None => format!("{snake}-items"),
        }
    }
}

/// Return the starter-CSS content for a `client_crud` screen template that
/// requested `styled: vanilla-css`. Returns `None` for any other preset (or
/// no preset at all). Callers that write the screen to disk use this to
/// also write `assets/{snake}.css` alongside.
pub(crate) fn vanilla_css_starter_for(t: &DslScreenTemplate, snake: &str) -> Option<String> {
    if t.kind != "client_crud" {
        return None;
    }
    if t.styled.as_deref() == Some("vanilla-css") {
        Some(build_vanilla_starter_css(snake))
    } else {
        None
    }
}

/// Build the starter CSS sheet for the `vanilla-css` preset. Keys off
/// `.screen.{snake}` so multiple client_crud screens in the same project
/// stay isolated. Intentionally short — the agent can extend it; we just
/// want to avoid the blank-file cold-start.
fn build_vanilla_starter_css(snake: &str) -> String {
    format!(
        "/* Starter stylesheet for the {snake} client_crud screen.\n\
         * Generated by dioxus-mcp `styled: vanilla-css`. Override or extend\n\
         * as the design evolves — class names are stable across re-runs.\n\
         */\n\
        .screen.{snake} {{ max-width: 32rem; margin: 2rem auto; padding: 0 1rem; }}\n\
        .screen.{snake} .title {{ font-size: 1.5rem; margin: 0 0 1rem 0; }}\n\
        .screen.{snake} .compose {{ display: flex; gap: 0.5rem; margin-bottom: 1rem; }}\n\
        .screen.{snake} .compose .field {{ flex: 1; padding: 0.5rem 0.75rem; border: 1px solid #d1d5db; border-radius: 0.375rem; }}\n\
        .screen.{snake} .compose .submit {{ padding: 0.5rem 1rem; background: #2563eb; color: white; border: none; border-radius: 0.375rem; cursor: pointer; }}\n\
        .screen.{snake} .compose .submit:hover {{ background: #1d4ed8; }}\n\
        .screen.{snake} .list {{ list-style: none; padding: 0; margin: 0; display: flex; flex-direction: column; gap: 0.5rem; }}\n\
        .screen.{snake} .row {{ display: flex; align-items: center; gap: 0.75rem; padding: 0.5rem; background: white; border: 1px solid #e5e7eb; border-radius: 0.375rem; }}\n\
        .screen.{snake} .row .label {{ flex: 1; }}\n\
        .screen.{snake} .toggle {{ width: 1rem; height: 1rem; }}\n\
        .screen.{snake} .delete {{ background: transparent; border: none; color: #dc2626; font-size: 1.125rem; cursor: pointer; padding: 0 0.25rem; }}\n\
        .screen.{snake} .delete:hover {{ color: #991b1b; }}\n\
        .screen.{snake} .empty {{ color: #6b7280; padding: 1rem; text-align: center; }}\n"
    )
}

pub(crate) fn render_client_crud_screen(
    pascal: &str,
    snake: &str,
    wrap_pascal: Option<&str>,
    client_stores: &[DslClientStore],
    t: &DslScreenTemplate,
) -> Result<String, String> {
    let store_ref = t.store.as_deref().ok_or_else(|| {
        format!("screen {pascal:?} kind=client_crud requires `store:` (a client_stores entry name)")
    })?;
    let store_snake = store_ref.to_snake_case();
    let store_cfg = client_stores
        .iter()
        .find(|cs| cs.name.to_snake_case() == store_snake)
        .ok_or_else(|| {
            format!(
                "screen {pascal:?} references unknown client_store {store_ref:?}; declare it under client_stores"
            )
        })?;
    let item_type = t
        .item_type
        .clone()
        .or_else(|| Some(store_cfg.item_type.clone()))
        .ok_or_else(|| format!("screen {pascal:?} kind=client_crud requires `item_type`"))?;
    let label_field = t
        .label_field
        .as_deref()
        .ok_or_else(|| format!("screen {pascal:?} kind=client_crud requires `label_field`"))?
        .to_snake_case();
    let checkbox_field = t.checkbox_field.as_deref().map(|s| s.to_snake_case());
    let id_field = store_cfg
        .id_field
        .as_deref()
        .ok_or_else(|| {
            format!(
                "screen {pascal:?} kind=client_crud requires the referenced client_store {store_ref:?} to declare `id_field` (delete/checkbox actions key off it)"
            )
        })?
        .to_snake_case();
    let id_type = store_cfg.id_type.clone().unwrap_or_else(|| "i64".into());
    let auto_id = store_cfg.auto_id.unwrap_or(false);
    // For integer ids we emit `1i64` etc. so the type of `next_id` is fixed
    // even before the first push. Non-integer id types fall back to bare `1`.
    let id_type_suffix = match id_type.as_str() {
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128"
        | "usize" => id_type.to_string(),
        _ => String::new(),
    };
    // Copy ids (primitive integers) can be captured by `move` closures without
    // the inner `let id = id.clone();` shim — calling them repeatedly just
    // re-copies the id. Non-Copy ids (String, Uuid, ...) still need the clone
    // dance so the FnMut handler can fire more than once.
    let id_is_copy = is_primitive_integer_ty(&id_type);
    // With auto_id on, the store owns the allocator — the screen doesn't need
    // its own next_id signal. Without it (and with a primitive integer id),
    // the screen falls back to the historical local-allocator scaffold.
    let has_id = !id_type_suffix.is_empty() && !auto_id;
    let needs_model_import = store_cfg.item_type.to_snake_case() == item_type.to_snake_case();
    let humanized = humanize(&item_type);

    // Pick the styled preset. Unknown values are rejected so users find
    // typos here rather than at the markup level.
    let style = match t.styled.as_deref() {
        None => ClientCrudStyle::default_unstyled(snake),
        Some("tailwind") => ClientCrudStyle::tailwind(),
        Some("vanilla-css") => ClientCrudStyle::vanilla_css(snake),
        Some(other) => {
            return Err(format!(
                "screen {pascal:?} kind=client_crud: unknown `template.styled` value {other:?} (expected: \"tailwind\", \"vanilla-css\", or omit)"
            ));
        }
    };
    // Compose-form submit affordance. `submit_button` is the default (visible
    // "Add" button); `enter_only` drops the button so the row UX is
    // press-Enter-only (e.g. TodoMVC-shaped apps). Unknown values are rejected.
    let emit_submit_button = match t.compose_style.as_deref() {
        None | Some("submit_button") => true,
        Some("enter_only") => false,
        Some(other) => {
            return Err(format!(
                "screen {pascal:?} kind=client_crud: unknown `template.compose_style` value {other:?} (expected: \"submit_button\", \"enter_only\", or omit)"
            ));
        }
    };

    // Extract the per-row body into a sibling `{Pascal}Row` component when a
    // checkbox is in play — the row body has the most closure-capture noise
    // (the toggle / delete handlers and the optional non-Copy id clone shim)
    // and is the part agents most often rewrite. Decomposing it gives a clean
    // `{item_snake}: ItemType` prop boundary so the parent's `for` loop
    // collapses to a one-line component call. Without a checkbox the row body
    // is tiny enough to stay inline.
    let row_pascal = format!("{pascal}Row");
    let item_prop_snake = item_type.to_snake_case();
    let split_row = checkbox_field.is_some();

    // Render the inner rsx body programmatically — the surrounding wrapper
    // (h1 / wrap_with / div) is filled in by CLIENT_CRUD_SCREEN_TPL.
    let mut body = String::new();
    let ind = if wrap_pascal.is_some() {
        "                "
    } else {
        "            "
    };
    body.push_str(&format!(
        "{ind}h1 {{ {h1_attrs}\"{pascal}\" }}\n",
        h1_attrs = style.h1_attrs()
    ));
    // "Add" form
    body.push_str(&format!(
        "{ind}form {{ class: \"{form_cls}\",\n",
        form_cls = style.form_class
    ));
    body.push_str(&format!("{ind}    onsubmit: move |evt: FormEvent| {{\n"));
    body.push_str(&format!("{ind}        evt.prevent_default();\n"));
    body.push_str(&format!("{ind}        let value = draft();\n"));
    body.push_str(&format!("{ind}        if value.is_empty() {{ return; }}\n"));
    if has_id {
        body.push_str(&format!("{ind}        let id = next_id();\n"));
        body.push_str(&format!("{ind}        *next_id.write() += 1;\n"));
    }
    let push_call = if auto_id { "push_new" } else { "push" };
    body.push_str(&format!("{ind}        store.{push_call}({item_type} {{\n"));
    if has_id {
        body.push_str(&format!("{ind}            {id_field}: id,\n"));
    }
    body.push_str(&format!("{ind}            {label_field}: value,\n"));
    body.push_str(&format!("{ind}            ..Default::default()\n"));
    body.push_str(&format!("{ind}        }});\n"));
    body.push_str(&format!("{ind}        draft.set(String::new());\n"));
    body.push_str(&format!("{ind}    }},\n"));
    body.push_str(&format!("{ind}    input {{\n"));
    body.push_str(&format!("{ind}        r#type: \"text\",\n"));
    if let Some(cls) = style.input_class.as_deref() {
        body.push_str(&format!("{ind}        class: \"{cls}\",\n"));
    }
    body.push_str(&format!("{ind}        value: \"{{draft()}}\",\n"));
    body.push_str(&format!("{ind}        placeholder: \"New {humanized}\",\n"));
    body.push_str(&format!(
        "{ind}        oninput: move |e| draft.set(e.value()),\n"
    ));
    body.push_str(&format!("{ind}    }}\n"));
    if emit_submit_button {
        if let Some(cls) = style.submit_button_class.as_deref() {
            body.push_str(&format!(
                "{ind}    button {{ r#type: \"submit\", class: \"{cls}\", \"Add\" }}\n"
            ));
        } else {
            body.push_str(&format!(
                "{ind}    button {{ r#type: \"submit\", \"Add\" }}\n"
            ));
        }
    }
    body.push_str(&format!("{ind}}}\n"));
    // List
    body.push_str(&format!(
        "{ind}ul {{ class: \"{list_cls}\",\n",
        list_cls = style.list_class(snake)
    ));
    body.push_str(&format!(
        "{ind}    for item in store.items().read().iter() {{\n"
    ));
    if split_row {
        // Defer to the sibling component. `item.clone()` is required because
        // the prop takes ownership; the model derives Clone (it must, the
        // ClientStore wraps it in a Vec inside a Signal).
        body.push_str(&format!(
            "{ind}        {row_pascal} {{ key: \"{{item.{id_field}}}\", {item_prop_snake}: item.clone() }}\n"
        ));
    } else {
        body.push_str(&format!(
            "{ind}        li {{ key: \"{{item.{id_field}}}\",{li_attrs}\n",
            li_attrs = style.li_attrs(),
        ));
        body.push_str(&format!(
            "{ind}            span {{ {span_attrs}\"{{item.{label_field}}}\" }}\n",
            span_attrs = style.label_span_attrs(),
        ));
        write_delete_button(
            &mut body,
            ind,
            "            ",
            &style,
            &id_field,
            &label_field,
            "item",
            id_is_copy,
        );
        body.push_str(&format!("{ind}        }}\n"));
    }
    body.push_str(&format!("{ind}    }}\n"));
    body.push_str(&format!("{ind}}}"));

    // Render the optional sibling row component. Indented from column 0 (it
    // lives at the top level of the file, not inside the parent component).
    let row_component = if split_row {
        Some(build_row_component(
            &row_pascal,
            &item_prop_snake,
            &item_type,
            &store_snake,
            &id_field,
            &label_field,
            checkbox_field.as_deref(),
            id_is_copy,
            &style,
        ))
    } else {
        None
    };

    render(
        "client_crud_screen",
        CLIENT_CRUD_SCREEN_TPL,
        context! {
            pascal => pascal,
            snake => snake,
            wrap_pascal => wrap_pascal,
            store_snake => store_snake,
            item_type => item_type,
            needs_model_import => needs_model_import,
            has_id => has_id,
            id_type_suffix => id_type_suffix,
            body => body,
            row_component => row_component,
        },
    )
}

/// Build the sibling `{Pascal}Row` component body. Lives at file scope (not
/// inside the parent component) and re-acquires the store via `use_*_store()`
/// — context lookups are cheap and avoid plumbing the store through a prop.
#[allow(clippy::too_many_arguments)]
fn build_row_component(
    row_pascal: &str,
    item_prop_snake: &str,
    item_type: &str,
    store_snake: &str,
    id_field: &str,
    label_field: &str,
    checkbox_field: Option<&str>,
    id_is_copy: bool,
    style: &ClientCrudStyle,
) -> String {
    let mut out = String::new();
    out.push_str("#[component]\n");
    out.push_str(&format!(
        "fn {row_pascal}({item_prop_snake}: {item_type}) -> Element {{\n"
    ));
    out.push_str(&format!("    let store = use_{store_snake}();\n"));
    out.push_str("    rsx! {\n");
    out.push_str(&format!(
        "        li {{{li_attrs}\n",
        li_attrs = style.li_attrs(),
    ));
    if let Some(cb) = checkbox_field {
        out.push_str("            input {\n");
        out.push_str("                r#type: \"checkbox\",\n");
        if let Some(cls) = style.checkbox_class.as_deref() {
            out.push_str(&format!("                class: \"{cls}\",\n"));
        }
        out.push_str(&format!(
            "                aria_label: \"Toggle {{{item_prop_snake}.{label_field}}}\",\n"
        ));
        out.push_str(&format!("                checked: {item_prop_snake}.{cb},\n"));
        if id_is_copy {
            out.push_str(&format!(
                "                onchange: move |_| store.update_by_id({item_prop_snake}.{id_field}, |t| t.{cb} = !t.{cb}),\n"
            ));
        } else {
            out.push_str("                onchange: {\n");
            out.push_str(&format!(
                "                    let id = {item_prop_snake}.{id_field}.clone();\n"
            ));
            out.push_str("                    move |_| {\n");
            out.push_str("                        let id = id.clone();\n");
            out.push_str(&format!(
                "                        store.update_by_id(id, |t| t.{cb} = !t.{cb});\n"
            ));
            out.push_str("                    }\n");
            out.push_str("                },\n");
        }
        out.push_str("            }\n");
    }
    out.push_str(&format!(
        "            span {{ {span_attrs}\"{{{item_prop_snake}.{label_field}}}\" }}\n",
        span_attrs = style.label_span_attrs(),
    ));
    write_delete_button(
        &mut out,
        "        ",
        "    ",
        style,
        id_field,
        label_field,
        item_prop_snake,
        id_is_copy,
    );
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("}\n");
    out
}

/// Emit a delete `button { ... }` block. Shared between the inline branch
/// (no checkbox, body lives in the parent's `for` loop) and the
/// `{Pascal}Row` branch (checkbox present, body lives in the sibling
/// component). `outer_ind` is the indent of the enclosing `li` block; the
/// button is nested one level inside that with `inner_ind` extra spaces.
/// `binding` is the variable name carrying the item (`"item"` in the
/// for-loop case, `"todo"` / `"product"` etc. in the Row case).
#[allow(clippy::too_many_arguments)]
fn write_delete_button(
    out: &mut String,
    outer_ind: &str,
    inner_ind: &str,
    style: &ClientCrudStyle,
    id_field: &str,
    label_field: &str,
    binding: &str,
    id_is_copy: bool,
) {
    out.push_str(&format!(
        "{outer_ind}{inner_ind}button {{ class: \"{del_cls}\",\n",
        del_cls = style.delete_button_class,
    ));
    out.push_str(&format!(
        "{outer_ind}{inner_ind}    aria_label: \"Delete {{{binding}.{label_field}}}\",\n"
    ));
    if id_is_copy {
        out.push_str(&format!(
            "{outer_ind}{inner_ind}    onclick: move |_| {{ store.remove_by_id({binding}.{id_field}); }},\n"
        ));
    } else {
        out.push_str(&format!("{outer_ind}{inner_ind}    onclick: {{\n"));
        out.push_str(&format!(
            "{outer_ind}{inner_ind}        let id = {binding}.{id_field}.clone();\n"
        ));
        out.push_str(&format!("{outer_ind}{inner_ind}        move |_| {{\n"));
        out.push_str(&format!(
            "{outer_ind}{inner_ind}            let id = id.clone();\n"
        ));
        out.push_str(&format!(
            "{outer_ind}{inner_ind}            store.remove_by_id(id);\n"
        ));
        out.push_str(&format!("{outer_ind}{inner_ind}        }}\n"));
        out.push_str(&format!("{outer_ind}{inner_ind}    }},\n"));
    }
    // U+00D7 (×) — universal "delete this row" glyph. Aria-label above
    // carries the meaningful text; this is purely visual.
    out.push_str(&format!("{outer_ind}{inner_ind}    \"\u{00D7}\"\n"));
    out.push_str(&format!("{outer_ind}{inner_ind}}}\n"));
}
