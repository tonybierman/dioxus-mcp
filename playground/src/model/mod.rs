//! Client-side mirror of the DSL subset the interpreter renders.
//!
//! These structs deserialize from the same YAML the user edits (and that the
//! server's `execute_code` consumes). We only model what's needed to render an
//! approximate preview of screens — unknown fields (models, client_stores,
//! resources, server fns, …) are ignored by serde, so a full doc parses fine.

use serde::Deserialize;

/// A DSL document. Only `screens` drives the preview; everything else in the
/// doc is ignored here (the server resolves the rest in `dry_run`).
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Doc {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub screens: Vec<Screen>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Screen {
    pub name: String,
    #[serde(default)]
    pub route: Option<String>,
    #[serde(default)]
    pub template: Option<ScreenTemplate>,
}

/// Mirror of `DslScreenTemplate` — the fields the interpreter consumes.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct ScreenTemplate {
    pub kind: String,
    #[serde(default)]
    pub item_type: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub on_submit: Option<String>,
    #[serde(default)]
    pub redirect_to: Option<String>,
    #[serde(default)]
    pub fields: Vec<FieldDef>,
    #[serde(default)]
    pub store: Option<String>,
    #[serde(default)]
    pub label_field: Option<String>,
    #[serde(default)]
    pub checkbox_field: Option<String>,
    #[serde(default)]
    pub class: Option<String>,
    #[serde(default)]
    pub styled: Option<String>,
    #[serde(default)]
    pub compose_style: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FieldDef {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub validation: Option<String>,
}

/// `RenderModel`/`RenderField` now live in the shared `dioxus-mcp-registry`
/// crate (one definition shared with the server, ending the hand-mirror);
/// re-exported here so existing `crate::model::RenderModel` paths keep working.
/// Arrives in `ScaffoldResult.render_models`.
pub use dioxus_mcp_registry::{RenderField, RenderModel};

/// Parse a DSL doc from YAML text. Errors are stringified for display in the
/// editor's error pane.
pub fn parse_doc(yaml: &str) -> Result<Doc, String> {
    serde_yaml::from_str::<Doc>(yaml).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_client_crud_and_ignores_unknown() {
        let doc = parse_doc(
            r#"version: "1"
models:
  - name: Todo
    fields: [{name: id, type: i64}]
screens:
  - name: TodoScreen
    route: /todos
    template:
      kind: client_crud
      store: TodoStore
      label_field: title
      checkbox_field: done
"#,
        )
        .expect("parse");
        assert_eq!(doc.screens.len(), 1);
        let t = doc.screens[0].template.as_ref().unwrap();
        assert_eq!(t.kind, "client_crud");
        assert_eq!(t.label_field.as_deref(), Some("title"));
        assert_eq!(t.checkbox_field.as_deref(), Some("done"));
    }

    #[test]
    fn parses_resource_form_fields_with_renamed_type() {
        let doc = parse_doc(
            r#"screens:
  - name: SignupForm
    template:
      kind: resource_form
      fields:
        - {name: email, type: email}
        - {name: bio, type: textarea}
"#,
        )
        .expect("parse");
        let t = doc.screens[0].template.as_ref().unwrap();
        assert_eq!(t.fields[0].ty, "email");
        assert_eq!(t.fields[1].ty, "textarea");
    }
}
