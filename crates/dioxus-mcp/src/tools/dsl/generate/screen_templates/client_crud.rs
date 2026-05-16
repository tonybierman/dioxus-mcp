use heck::ToSnakeCase;
use minijinja::context;

use super::super::super::render::*;
use super::super::super::templates::*;
use super::super::super::types::*;
use super::super::humanize;

/// Bag of class strings / attribute snippets per design-system preset for the
/// `client_crud` template. Keeps the rendering loop above readable instead of
/// fanning out the same `if styled == "tailwind"` branch six times.
struct ClientCrudStyle {
    form_class: String,
    list_class_override: Option<String>,
    input_class: Option<&'static str>,
    submit_button_class: Option<&'static str>,
    checkbox_class: Option<&'static str>,
    delete_button_class: String,
    extra_h1_attrs: Option<&'static str>,
    extra_li_attrs: Option<&'static str>,
    extra_label_attrs: Option<&'static str>,
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
                "flex-1 px-3 py-2 border border-gray-300 rounded-md focus:outline-none focus:ring-2 focus:ring-blue-500",
            ),
            submit_button_class: Some(
                "px-4 py-2 bg-blue-600 text-white rounded-md hover:bg-blue-700 focus:outline-none focus:ring-2 focus:ring-blue-500",
            ),
            checkbox_class: Some("h-4 w-4 text-blue-600 rounded border-gray-300"),
            delete_button_class: "text-red-600 hover:text-red-800 text-sm font-medium".into(),
            extra_h1_attrs: Some("class: \"text-2xl font-semibold mb-4\", "),
            extra_li_attrs: Some(
                " class: \"flex items-center gap-3 p-2 bg-white border border-gray-200 rounded-md\",",
            ),
            extra_label_attrs: Some("class: \"flex-1\", "),
        }
    }

    fn h1_attrs(&self) -> &str {
        self.extra_h1_attrs.unwrap_or("")
    }

    fn li_attrs(&self) -> &str {
        self.extra_li_attrs.unwrap_or("")
    }

    fn label_span_attrs(&self) -> &str {
        self.extra_label_attrs.unwrap_or("")
    }

    fn list_class(&self, snake: &str) -> String {
        match &self.list_class_override {
            Some(s) => s.clone(),
            None => format!("{snake}-items"),
        }
    }
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
    // With auto_id on, the store owns the allocator — the screen doesn't need
    // its own next_id signal. Without it (and with a primitive integer id),
    // the screen falls back to the historical local-allocator scaffold.
    let has_id = !id_type_suffix.is_empty() && !auto_id;
    let needs_model_import = store_cfg.item_type.to_snake_case() == item_type.to_snake_case();
    let humanized = humanize(&item_type);

    // Pick the styled preset (currently only `tailwind`). Unknown values
    // are rejected so users find typos here rather than at the markup level.
    let style = match t.styled.as_deref() {
        None => ClientCrudStyle::default_unstyled(snake),
        Some("tailwind") => ClientCrudStyle::tailwind(),
        Some(other) => {
            return Err(format!(
                "screen {pascal:?} kind=client_crud: unknown `template.styled` value {other:?} (expected: \"tailwind\" or omit)"
            ));
        }
    };

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
    if let Some(cls) = style.input_class {
        body.push_str(&format!("{ind}        class: \"{cls}\",\n"));
    }
    body.push_str(&format!("{ind}        value: \"{{draft()}}\",\n"));
    body.push_str(&format!("{ind}        placeholder: \"New {humanized}\",\n"));
    body.push_str(&format!(
        "{ind}        oninput: move |e| draft.set(e.value()),\n"
    ));
    body.push_str(&format!("{ind}    }}\n"));
    if let Some(cls) = style.submit_button_class {
        body.push_str(&format!(
            "{ind}    button {{ r#type: \"submit\", class: \"{cls}\", \"Add\" }}\n"
        ));
    } else {
        body.push_str(&format!(
            "{ind}    button {{ r#type: \"submit\", \"Add\" }}\n"
        ));
    }
    body.push_str(&format!("{ind}}}\n"));
    // List
    body.push_str(&format!(
        "{ind}ul {{ class: \"{list_cls}\",\n",
        list_cls = style.list_class(snake)
    ));
    body.push_str(&format!(
        "{ind}    for item in store.items.read().iter() {{\n"
    ));
    body.push_str(&format!(
        "{ind}        li {{ key: \"{{item.{id_field}}}\",{li_attrs}\n",
        li_attrs = style.li_attrs(),
    ));
    if let Some(cb) = &checkbox_field {
        body.push_str(&format!("{ind}            input {{\n"));
        body.push_str(&format!("{ind}                r#type: \"checkbox\",\n"));
        if let Some(cls) = style.checkbox_class {
            body.push_str(&format!("{ind}                class: \"{cls}\",\n"));
        }
        // Idiomatic Dioxus 0.7 boolean attribute: bind the bool field
        // directly, not its formatted-string form.
        body.push_str(&format!("{ind}                checked: item.{cb},\n"));
        body.push_str(&format!("{ind}                oninput: {{\n"));
        body.push_str(&format!(
            "{ind}                    let id = item.{id_field}.clone();\n"
        ));
        body.push_str(&format!("{ind}                    move |_| {{\n"));
        body.push_str(&format!(
            "{ind}                        let id = id.clone();\n"
        ));
        body.push_str(&format!(
            "{ind}                        store.update_by_id(id, |t| t.{cb} = !t.{cb});\n"
        ));
        body.push_str(&format!("{ind}                    }}\n"));
        body.push_str(&format!("{ind}                }},\n"));
        body.push_str(&format!("{ind}            }}\n"));
    }
    body.push_str(&format!(
        "{ind}            span {{ {span_attrs}\"{{item.{label_field}}}\" }}\n",
        span_attrs = style.label_span_attrs(),
    ));
    body.push_str(&format!(
        "{ind}            button {{ class: \"{del_cls}\",\n",
        del_cls = style.delete_button_class,
    ));
    body.push_str(&format!("{ind}                onclick: {{\n"));
    body.push_str(&format!(
        "{ind}                    let id = item.{id_field}.clone();\n"
    ));
    body.push_str(&format!("{ind}                    move |_| {{\n"));
    body.push_str(&format!(
        "{ind}                        let id = id.clone();\n"
    ));
    body.push_str(&format!(
        "{ind}                        store.remove_by_id(id);\n"
    ));
    body.push_str(&format!("{ind}                    }}\n"));
    body.push_str(&format!("{ind}                }},\n"));
    body.push_str(&format!("{ind}                \"Delete\"\n"));
    body.push_str(&format!("{ind}            }}\n"));
    body.push_str(&format!("{ind}        }}\n"));
    body.push_str(&format!("{ind}    }}\n"));
    body.push_str(&format!("{ind}}}"));

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
        },
    )
}
