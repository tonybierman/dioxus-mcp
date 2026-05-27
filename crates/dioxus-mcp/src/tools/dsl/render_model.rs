//! Builds structured [`RenderModel`]s for the server-synthesized resource
//! screens, so a browser client can render an approximate preview of a
//! `resources:` slice it can't otherwise see (those screens are produced by
//! [`expand_resources`](super::resources::expand_resources) and never appear in
//! the user's `screens:`).
//!
//! Only screens carrying a `crud` context (i.e. resource-synthesized) get a
//! model; explicit client_crud / resource_form / empty screens are left to the
//! client's own interpreter.

use heck::{ToSnakeCase, ToTitleCase};

use super::types::DslDoc;
use crate::tools::scaffold::{RenderField, RenderModel};

/// Produce one [`RenderModel`] per resource-synthesized screen in `doc`.
pub(super) fn build_render_models(doc: &DslDoc) -> Vec<RenderModel> {
    let mut models = Vec::new();

    for screen in &doc.screens {
        let Some(template) = &screen.template else {
            continue;
        };
        let Some(crud) = &template.crud else {
            // Only resource-synthesized screens carry a CrudCtx; everything
            // else the client renders from its own local parse.
            continue;
        };

        let mut model = RenderModel {
            screen: screen.name.clone(),
            kind: template.kind.clone(),
            route: screen.route.clone(),
            item_type: template.item_type.clone().unwrap_or_default(),
            root_class: Some(format!("screen {}", screen.name.to_snake_case())),
            ..Default::default()
        };

        match template.kind.as_str() {
            "resource_list" => {
                // Columns come from the model fields (the list screen itself
                // carries no `fields`). `ty` stays the Rust type so the client
                // can synthesize type-appropriate mock cells.
                model.columns = crud
                    .model_fields
                    .iter()
                    .map(|f| RenderField {
                        name: f.name.clone(),
                        label: f.name.to_title_case(),
                        ty: f.ty.clone(),
                    })
                    .collect();
                model.list_endpoint = Some(crud.list_endpoint.clone());
                model.new_route = Some(crud.new_route.clone());
            }
            "resource_form" | "resource_edit_form" => {
                // The form screens already carry their input fields (`ty` is the
                // HTML input kind here, not the Rust type).
                model.fields = template
                    .fields
                    .iter()
                    .map(|f| RenderField {
                        name: f.name.clone(),
                        label: f.name.to_title_case(),
                        ty: f.ty.clone(),
                    })
                    .collect();
            }
            _ => {}
        }

        models.push(model);
    }

    models
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_models_for_resource_slice() {
        let mut doc: DslDoc = serde_yml::from_str(
            r#"version: "1"
resources:
  - name: Product
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
      - {name: price, type: f64}
"#,
        )
        .unwrap();
        super::super::resources::expand_resources(&mut doc).unwrap();

        let models = build_render_models(&doc);

        let list = models
            .iter()
            .find(|m| m.kind == "resource_list")
            .expect("a resource_list render model");
        // Columns come from the model fields, labels are Title Cased, Rust type
        // is preserved for mock-cell synthesis.
        let title = list
            .columns
            .iter()
            .find(|c| c.name == "title")
            .expect("title column");
        assert_eq!(title.label, "Title");
        assert_eq!(title.ty, "String");
        assert!(list.list_endpoint.is_some(), "list endpoint set");
        assert!(list.new_route.is_some(), "new route set");
        // root_class mirrors the generated screen's own `screen {snake}` class.
        let root = list.root_class.as_deref().unwrap();
        assert!(root.starts_with("screen product"), "root_class was {root:?}");

        // The form screen carries input fields, with the id field dropped.
        let form = models
            .iter()
            .find(|m| m.kind == "resource_form")
            .expect("a resource_form render model");
        assert!(!form.fields.is_empty());
        assert!(form.fields.iter().all(|f| f.name != "id"));
    }
}
