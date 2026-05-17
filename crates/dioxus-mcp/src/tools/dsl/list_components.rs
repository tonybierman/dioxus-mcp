//! `list_components`: dedicated catalog-discovery tool. Returns the official
//! Dioxus 0.7 component catalog (name + one-line description + post-install
//! import path) as a small, structured payload.
//!
//! Why this exists separately from `get_dsl_spec { sections: [components] }`:
//! the spec block ships the catalog wrapped in YAML-as-string with the
//! authoring preamble and install hints. When an agent has decided "I want
//! to pick a component," that wrapping is dead weight — this tool returns
//! just the catalog as JSON so it's cheap to scan and easy to filter.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::execute::DX_COMPONENT_CATALOG_ENTRIES;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ListComponentsParams {
    /// Optional case-insensitive substring filter applied to component names
    /// AND descriptions. Useful when the agent has a concept ("date", "menu")
    /// but doesn't yet know the exact catalog key.
    #[serde(default)]
    pub query: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ComponentEntry {
    /// Snake-case catalog name — pass this verbatim to `dx components add`.
    pub name: String,
    /// One-line description from the upstream registry.
    pub description: String,
    /// Post-install import path. Drop straight into `use ...;` then use the
    /// PascalCase identifier inside `rsx!`.
    pub import: String,
}

#[derive(Debug, Serialize)]
pub struct ListComponentsResult {
    /// `dx components add <name>` install template, with the placeholder
    /// agents should substitute.
    pub install_template: &'static str,
    /// First-time install also requires this line in `src/main.rs` and a
    /// theme stylesheet entry — surfaced here so the caller doesn't have to
    /// chase a separate spec section to discover it.
    pub first_install_setup: &'static [&'static str],
    /// Total catalog size before filtering — lets a caller spot when their
    /// `query` collapses a 45-entry list down to zero matches by accident.
    pub total: usize,
    pub components: Vec<ComponentEntry>,
}

pub async fn list_components(p: ListComponentsParams) -> Result<ListComponentsResult, String> {
    let needle = p.query.as_deref().map(|s| s.trim().to_ascii_lowercase());
    let total = DX_COMPONENT_CATALOG_ENTRIES.len();
    let components: Vec<ComponentEntry> = DX_COMPONENT_CATALOG_ENTRIES
        .iter()
        .filter(|(name, desc)| match &needle {
            None => true,
            Some(q) if q.is_empty() => true,
            Some(q) => {
                name.to_ascii_lowercase().contains(q)
                    || desc.to_ascii_lowercase().contains(q)
            }
        })
        .map(|(name, desc)| ComponentEntry {
            name: (*name).to_string(),
            description: (*desc).to_string(),
            import: format!("use crate::components::{name}::{};", to_pascal(name)),
        })
        .collect();
    Ok(ListComponentsResult {
        install_template: "dx components add <name>",
        first_install_setup: &[
            "add `mod components;` to src/main.rs (first install only)",
            "include the theme stylesheet via `asset!(\"/assets/dx-components-theme.css\")` (first install only)",
        ],
        total,
        components,
    })
}

/// snake_case → PascalCase, inlined to avoid pulling heck into this small
/// surface. The catalog only ever holds ASCII snake_case names.
fn to_pascal(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    let mut upper = true;
    for ch in snake.chars() {
        if ch == '_' {
            upper = true;
        } else if upper {
            for u in ch.to_uppercase() {
                out.push(u);
            }
            upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn list_components_returns_full_catalog_when_unfiltered() {
        let r = list_components(ListComponentsParams { query: None })
            .await
            .unwrap();
        assert_eq!(r.total, DX_COMPONENT_CATALOG_ENTRIES.len());
        assert_eq!(r.components.len(), DX_COMPONENT_CATALOG_ENTRIES.len());
        let names: Vec<&str> = r.components.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"button"));
        assert!(names.contains(&"dropdown_menu"));
    }

    #[tokio::test]
    async fn list_components_filter_matches_name_or_description() {
        let r = list_components(ListComponentsParams {
            query: Some("date".into()),
        })
        .await
        .unwrap();
        let names: Vec<&str> = r.components.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"calendar"), "expected calendar (desc mentions dates), got: {names:?}");
        assert!(names.contains(&"date_picker"), "expected date_picker (name match), got: {names:?}");
        assert!(!names.contains(&"button"), "button should not match 'date': {names:?}");
    }

    #[tokio::test]
    async fn list_components_import_uses_pascal_case() {
        let r = list_components(ListComponentsParams {
            query: Some("dropdown_menu".into()),
        })
        .await
        .unwrap();
        let entry = r
            .components
            .iter()
            .find(|c| c.name == "dropdown_menu")
            .expect("dropdown_menu must be returned");
        assert_eq!(entry.import, "use crate::components::dropdown_menu::DropdownMenu;");
    }
}
