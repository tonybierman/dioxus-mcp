//! Builds structured [`RenderModel`]s the browser client can render directly.
//!
//! Two sources get a model:
//! 1. **Resource-synthesized screens** (those carrying a `crud` context, from
//!    [`expand_resources`](super::resources::expand_resources)): the historical
//!    case — typed `columns`/`fields` for the built-in `resource_*` interpreter.
//! 2. **Runtime layouts** (a `kind` matching a registry [`LayoutDescriptor`]
//!    that isn't a built-in): the descriptor's [`PreviewSkeleton`] is
//!    instantiated into generic `nodes` + `behavior` so the cockpit's generic
//!    node-walker can preview a layout the playground has no bespoke code for.
//!
//! Built-in `client_crud`/`empty` handwritten screens still get no model — the
//! client renders those from its own local parse. `freeform` is the exception:
//! its inline RSX body can't be rendered in wasm, so it gets a static shell
//! model (a titled placeholder + hint) to stay visible in the navigator.

use std::collections::BTreeMap;

use dioxus_mcp_registry::{Behavior, LayoutDescriptor, PreviewSkeleton, RenderNode, Slot};
use heck::{ToSnakeCase, ToTitleCase};

use super::generate::is_builtin_layout_kind;
use super::types::{DslDoc, DslScreenTemplate};
use crate::tools::scaffold::{RenderField, RenderModel};

/// Produce one [`RenderModel`] per resource-synthesized screen and per
/// runtime-layout screen in `doc`.
pub(super) fn build_render_models(
    doc: &DslDoc,
    layouts: &BTreeMap<String, LayoutDescriptor>,
) -> Vec<RenderModel> {
    let mut models = Vec::new();

    for screen in &doc.screens {
        let Some(template) = &screen.template else {
            continue;
        };
        let root_class = format!("screen {}", screen.name.to_snake_case());

        if let Some(crud) = &template.crud {
            let mut model = RenderModel {
                screen: screen.name.clone(),
                kind: template.kind.clone(),
                route: screen.route.clone(),
                item_type: template.item_type.clone().unwrap_or_default(),
                root_class: Some(root_class),
                theme: doc.theme.clone(),
                ..Default::default()
            };
            match template.kind.as_str() {
                "resource_list" => {
                    // Columns come from the model fields (the list screen itself
                    // carries no `fields`). `ty` stays the Rust type so the
                    // client can synthesize type-appropriate mock cells.
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
                    // The form screens already carry their input fields (`ty` is
                    // the HTML input kind here, not the Rust type).
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
            continue;
        }

        // Freeform: the inline RSX body can't be rendered in the wasm preview,
        // so emit a static shell model — the screen still appears in the
        // navigator and renders a titled placeholder + a "see source" hint
        // rather than being absent or flagged unsupported.
        if template.kind == "freeform" {
            let fclass = template.class.clone().unwrap_or_else(|| root_class.clone());
            models.push(RenderModel {
                screen: screen.name.clone(),
                kind: "freeform".into(),
                layout: "freeform".into(),
                route: screen.route.clone(),
                item_type: template.item_type.clone().unwrap_or_default(),
                root_class: Some(fclass),
                theme: doc.theme.clone(),
                nodes: vec![
                    RenderNode::Element {
                        tag: "h1".into(),
                        class: None,
                        attrs: Default::default(),
                        children: vec![RenderNode::Text {
                            text: screen.name.clone(),
                        }],
                    },
                    RenderNode::Element {
                        tag: "p".into(),
                        class: Some("preview-hint".into()),
                        attrs: Default::default(),
                        children: vec![RenderNode::Text {
                            text: "freeform screen — see the generated source / Compiled tab"
                                .into(),
                        }],
                    },
                ],
                behavior: Some(Behavior::Static),
                ..Default::default()
            });
            continue;
        }

        // Runtime layout: a registered, non-built-in kind with a preview. The
        // generic node-walker on the client renders the instantiated skeleton.
        if !is_builtin_layout_kind(&template.kind)
            && let Some(layout) = layouts.get(&template.kind)
        {
            let fields = form_fields(template);
            models.push(RenderModel {
                screen: screen.name.clone(),
                kind: template.kind.clone(),
                layout: template.kind.clone(),
                route: screen.route.clone(),
                item_type: template.item_type.clone().unwrap_or_default(),
                root_class: Some(root_class),
                theme: doc.theme.clone(),
                fields: fields.clone(),
                nodes: instantiate(&layout.preview, &fields),
                behavior: layout.preview.behavior.clone(),
                ..Default::default()
            });
        }
    }

    models
}

/// A template's declared inputs as [`RenderField`]s (`ty` = HTML input kind).
fn form_fields(template: &DslScreenTemplate) -> Vec<RenderField> {
    template
        .fields
        .iter()
        .map(|f| RenderField {
            name: f.name.clone(),
            label: f.name.to_title_case(),
            ty: f.ty.clone(),
        })
        .collect()
}

/// Expand a layout's [`PreviewSkeleton`] into concrete `nodes`: the data-bound
/// [`Slot`]s (`FormFields`/`TableHeader`/`TableMockRows`) are filled from the
/// screen's resolved fields; `CrudList` is left in place for the client's
/// behavior to render live. Static element/text nodes pass through.
fn instantiate(skeleton: &PreviewSkeleton, fields: &[RenderField]) -> Vec<RenderNode> {
    skeleton
        .nodes
        .iter()
        .flat_map(|n| expand(n, fields))
        .collect()
}

fn expand(node: &RenderNode, fields: &[RenderField]) -> Vec<RenderNode> {
    match node {
        RenderNode::Element {
            tag,
            class,
            attrs,
            children,
        } => vec![RenderNode::Element {
            tag: tag.clone(),
            class: class.clone(),
            attrs: attrs.clone(),
            children: children.iter().flat_map(|c| expand(c, fields)).collect(),
        }],
        RenderNode::Text { .. } => vec![node.clone()],
        RenderNode::Slot { slot } => match slot {
            Slot::FormFields => fields
                .iter()
                .map(|f| RenderNode::Element {
                    tag: "label".into(),
                    class: None,
                    attrs: Default::default(),
                    children: vec![
                        RenderNode::Text {
                            text: f.label.clone(),
                        },
                        RenderNode::Element {
                            tag: "input".into(),
                            class: None,
                            attrs: [("type".to_string(), input_kind(&f.ty).to_string())]
                                .into_iter()
                                .collect(),
                            children: vec![],
                        },
                    ],
                })
                .collect(),
            Slot::TableHeader => vec![RenderNode::Element {
                tag: "tr".into(),
                class: None,
                attrs: Default::default(),
                children: fields
                    .iter()
                    .map(|f| RenderNode::Element {
                        tag: "th".into(),
                        class: None,
                        attrs: Default::default(),
                        children: vec![RenderNode::Text {
                            text: f.label.clone(),
                        }],
                    })
                    .collect(),
            }],
            Slot::TableMockRows => (0..2)
                .map(|_| RenderNode::Element {
                    tag: "tr".into(),
                    class: None,
                    attrs: Default::default(),
                    children: fields
                        .iter()
                        .map(|_| RenderNode::Element {
                            tag: "td".into(),
                            class: None,
                            attrs: Default::default(),
                            children: vec![RenderNode::Text { text: "—".into() }],
                        })
                        .collect(),
                })
                .collect(),
            // Live, behavior-driven — the client fills this.
            Slot::CrudList => vec![node.clone()],
        },
    }
}

/// Map a DSL field type to an HTML `<input type>` (mirrors the interpreter).
fn input_kind(ty: &str) -> &'static str {
    match ty {
        "email" => "email",
        "password" => "password",
        "number" => "number",
        "checkbox" => "checkbox",
        _ => "text",
    }
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

        let models = build_render_models(&doc, &BTreeMap::new());

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
        assert!(
            root.starts_with("screen product"),
            "root_class was {root:?}"
        );

        // The form screen carries input fields, with the id field dropped.
        let form = models
            .iter()
            .find(|m| m.kind == "resource_form")
            .expect("a resource_form render model");
        assert!(!form.fields.is_empty());
        assert!(form.fields.iter().all(|f| f.name != "id"));
    }

    #[test]
    fn emits_static_shell_for_a_freeform_screen() {
        let doc: DslDoc = serde_yml::from_str(
            r#"version: "1"
screens:
  - name: MarkdownEditor
    route: /editor
    template:
      kind: freeform
      body: |
        rsx! { div { "hi" } }
"#,
        )
        .unwrap();

        let models = build_render_models(&doc, &BTreeMap::new());
        let m = models
            .iter()
            .find(|m| m.screen == "MarkdownEditor")
            .expect("a freeform render model");
        assert_eq!(m.kind, "freeform");
        assert_eq!(m.layout, "freeform");
        assert_eq!(m.behavior, Some(Behavior::Static));
        assert_eq!(m.route, "/editor");
        assert_eq!(m.root_class.as_deref(), Some("screen markdown_editor"));
        // A titled shell: h1 with the screen name + a hint paragraph. The inline
        // rsx body is deliberately NOT rendered (can't be, in wasm).
        assert!(
            matches!(
                &m.nodes[0],
                RenderNode::Element { tag, children, .. }
                    if tag == "h1"
                    && matches!(&children[0], RenderNode::Text { text } if text == "MarkdownEditor")
            ),
            "first node should be the titled h1, got {:?}",
            m.nodes
        );
        assert!(
            m.nodes.iter().any(|n| matches!(
                n,
                RenderNode::Element { class: Some(c), .. } if c == "preview-hint"
            )),
            "expected a preview-hint node, got {:?}",
            m.nodes
        );
    }

    #[test]
    fn emits_generic_nodes_for_a_runtime_layout() {
        use dioxus_mcp_registry::{Behavior, LayoutDescriptor, PreviewSkeleton, RenderNode, Slot};

        // A registered, non-built-in layout whose skeleton has a FormFields slot.
        let mut layouts = BTreeMap::new();
        layouts.insert(
            "callout".to_string(),
            LayoutDescriptor {
                id: "callout".into(),
                label: "Callout".into(),
                nav_rank: 9,
                template: Some("// runtime".into()),
                complex: false,
                styles: None,
                requires: None,
                context_vars: vec![],
                preview: PreviewSkeleton {
                    nodes: vec![RenderNode::Element {
                        tag: "form".into(),
                        class: Some("callout".into()),
                        attrs: Default::default(),
                        children: vec![RenderNode::Slot {
                            slot: Slot::FormFields,
                        }],
                    }],
                    behavior: Some(Behavior::Static),
                },
            },
        );

        let doc: DslDoc = serde_yml::from_str(
            r#"version: "1"
screens:
  - name: SignupCallout
    route: /signup
    template:
      kind: callout
      fields:
        - {name: email, type: email}
        - {name: bio, type: textarea}
"#,
        )
        .unwrap();

        let models = build_render_models(&doc, &layouts);
        assert_eq!(models.len(), 1);
        let m = &models[0];
        assert_eq!(m.kind, "callout");
        assert_eq!(m.layout, "callout");
        assert!(matches!(m.behavior, Some(Behavior::Static)));
        // The FormFields slot expanded into one <label> per field.
        let RenderNode::Element { tag, children, .. } = &m.nodes[0] else {
            panic!("expected an element node, got {:?}", m.nodes);
        };
        assert_eq!(tag, "form");
        let labels = children
            .iter()
            .filter(|n| matches!(n, RenderNode::Element { tag, .. } if tag == "label"))
            .count();
        assert_eq!(labels, 2, "one label per field");
    }
}
