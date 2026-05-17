use std::path::Path;

use heck::{ToPascalCase, ToSnakeCase};
use minijinja::context;

use crate::tools::scaffold::ScaffoldResult;

use super::super::render::*;
use super::super::types::*;

const VIEW_STATE_TPL: &str = r#"use dioxus::prelude::*;
{%- if emit_enum %}

/// Unit-variant enum auto-generated for the `{{ pascal }}` view state.
/// Variants are Copy so the signal can return them by value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum {{ ty }} {
{%- for v in variants %}
    {{ v }},
{%- endfor %}
}
{%- endif %}

pub fn provide_{{ snake }}() -> Signal<{{ ty }}> {
    use_context_provider(|| Signal::new({{ initial }}))
}

pub fn use_{{ snake }}() -> Signal<{{ ty }}> {
    use_context::<Signal<{{ ty }}>>()
}
"#;

pub(crate) fn generate_view_state(
    crate_root: &Path,
    v: &DslViewState,
) -> Result<ScaffoldResult, String> {
    if v.initial.trim().is_empty() {
        return Err(format!(
            "view_state {:?}: `initial` is required (Rust expression for the starting value)",
            v.name
        ));
    }
    let snake = v.name.to_snake_case();
    let pascal = v.name.to_pascal_case();
    let ty = v.ty.trim().to_string();
    if ty.is_empty() {
        return Err(format!(
            "view_state {:?}: `type` is required",
            v.name
        ));
    }
    let emit_enum = !v.enum_variants.is_empty();
    if emit_enum {
        // Reject empty / whitespace-only variants up-front so the generated
        // enum can't end up with a `,` followed by nothing useful.
        for variant in &v.enum_variants {
            if variant.trim().is_empty() {
                return Err(format!(
                    "view_state {:?}: `enum_variants` contains an empty entry",
                    v.name
                ));
            }
        }
    }
    let variants: Vec<String> = v.enum_variants.iter().map(|s| s.to_pascal_case()).collect();
    let body = render(
        "view_state",
        VIEW_STATE_TPL,
        context! {
            snake => snake.clone(),
            pascal => pascal,
            ty => ty,
            initial => v.initial.clone(),
            emit_enum => emit_enum,
            variants => variants,
        },
    )?;
    // ViewState files live alongside ClientStore under src/state but are NOT
    // server-gated — they're plain client-side state.
    write_module_file_with_cfg(crate_root, "src/state", &snake, body, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_state_with_enum_variants_emits_enum_and_hooks() {
        let dir = tempfile::TempDir::new().unwrap();
        let v = DslViewState {
            name: "TodoFilter".into(),
            ty: "TodoFilter".into(),
            initial: "TodoFilter::All".into(),
            enum_variants: vec!["All".into(), "Active".into(), "Done".into()],
        };
        let r = generate_view_state(dir.path(), &v).unwrap();
        let file = r
            .files_created
            .iter()
            .find(|p| p.ends_with("todo_filter.rs"))
            .expect("view_state file");
        let body = std::fs::read_to_string(file).unwrap();
        assert!(body.contains("pub enum TodoFilter {"));
        assert!(body.contains("    All,"));
        assert!(body.contains("    Active,"));
        assert!(body.contains("    Done,"));
        assert!(body.contains("pub fn provide_todo_filter() -> Signal<TodoFilter>"));
        assert!(body.contains("pub fn use_todo_filter() -> Signal<TodoFilter>"));
        assert!(body.contains("Signal::new(TodoFilter::All)"));
    }

    #[test]
    fn view_state_without_enum_variants_skips_enum_decl() {
        let dir = tempfile::TempDir::new().unwrap();
        let v = DslViewState {
            name: "search_query".into(),
            ty: "String".into(),
            initial: "String::new()".into(),
            enum_variants: vec![],
        };
        let r = generate_view_state(dir.path(), &v).unwrap();
        let file = r
            .files_created
            .iter()
            .find(|p| p.ends_with("search_query.rs"))
            .expect("view_state file");
        let body = std::fs::read_to_string(file).unwrap();
        assert!(!body.contains("pub enum"));
        assert!(body.contains("pub fn provide_search_query() -> Signal<String>"));
        assert!(body.contains("Signal::new(String::new())"));
    }

    #[test]
    fn view_state_rejects_empty_initial() {
        let dir = tempfile::TempDir::new().unwrap();
        let v = DslViewState {
            name: "x".into(),
            ty: "i32".into(),
            initial: "".into(),
            enum_variants: vec![],
        };
        let err = generate_view_state(dir.path(), &v).unwrap_err();
        assert!(err.contains("initial"), "got: {err}");
    }
}
