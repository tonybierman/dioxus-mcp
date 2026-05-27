//! Component catalog descriptors. The registry holds the *catalog metadata*
//! (name/description/prop-hint) used by `list_components`/`suggest_components`/
//! validation. The rich per-install descriptor (parsed from component source
//! with `syn`) stays server-side in `describe_component` — the registry does not
//! replace it.

use serde::{Deserialize, Serialize};

/// One catalog entry. Seeds mirror the historical
/// `DX_COMPONENT_CATALOG_ENTRIES` tuples 1:1; `keywords`/`audit_classes` are
/// optional extension points (the `suggest_components` / `components_audit`
/// tables migrate onto them later, default empty for now).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ComponentDescriptor {
    /// snake_case catalog name (e.g. `button`, `drag_and_drop_list`).
    pub name: String,
    /// One-line description.
    #[serde(default)]
    pub description: String,
    /// Prop/event surface hint.
    #[serde(default)]
    pub prop_hint: String,
    /// Keywords for `suggest_components` matching (follow-up; default empty).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// Class names this component should be suggested for by `components_audit`
    /// (follow-up; default empty).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audit_classes: Vec<String>,
}
