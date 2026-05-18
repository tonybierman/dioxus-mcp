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

/// Per-catalog-entry caveats — known shape limitations the agent should weigh
/// BEFORE running `dx components add`. Only listed when the widget materially
/// fails to model a common variant of its named pattern (e.g. drag_and_drop_list
/// is *one* sortable list — no cross-list drop callback). Leave entries off
/// when the widget covers the full conventional surface; this list should
/// stay short.
const CATALOG_LIMITATIONS: &[(&str, &str)] = &[(
    "drag_and_drop_list",
    "single sortable list; no drop callback. Cannot model cross-list / \
         cross-column moves — hand-roll the html5 dragstart/dragover/drop \
         pattern for kanban-style boards.",
)];

fn limitations_for(name: &str) -> Option<&'static str> {
    CATALOG_LIMITATIONS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, l)| *l)
}

/// Public alias for `describe_component` so the limitations table has a
/// single source of truth.
pub(super) fn limitations_for_describe(name: &str) -> Option<&'static str> {
    limitations_for(name)
}

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
    /// Prop/event surface hint — captures how the widget is controlled
    /// (main props + events) so the caller can pick without calling
    /// `describe_component`. For full prop typing call `describe_component`.
    pub prop_hint: String,
    /// Post-install import path. Drop straight into `use ...;` then use the
    /// PascalCase identifier inside `rsx!`.
    pub import: String,
    /// Known shape limitations — only present when the widget materially
    /// fails to model a common variant of its named pattern (e.g.
    /// `drag_and_drop_list` is one sortable list, no cross-list drop).
    /// Read this BEFORE running `dx components add` so installs that
    /// won't fit the user's shape are rejected up front.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limitations: Option<String>,
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
        .filter(|(name, desc, hint)| match &needle {
            None => true,
            Some(q) if q.is_empty() => true,
            Some(q) => {
                name.to_ascii_lowercase().contains(q)
                    || desc.to_ascii_lowercase().contains(q)
                    || hint.to_ascii_lowercase().contains(q)
            }
        })
        .map(|(name, desc, hint)| ComponentEntry {
            name: (*name).to_string(),
            description: (*desc).to_string(),
            prop_hint: (*hint).to_string(),
            import: format!("use crate::components::{name}::{};", to_pascal(name)),
            limitations: limitations_for(name).map(str::to_string),
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

// ----------------- suggest_components -----------------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SuggestComponentsParams {
    /// Free-text prompt / user ask. Scanned for UI-primitive keywords
    /// (drag, dialog, combobox, calendar, toast, …); matching catalog
    /// entries are returned. Pass the user's verbatim words — the matcher
    /// is intentionally generous.
    pub prompt: String,
}

#[derive(Debug, Serialize)]
pub struct SuggestComponentsResult {
    /// Keywords from the prompt that mapped to a catalog entry.
    pub matched_keywords: Vec<String>,
    /// Suggested catalog entries, ordered by match strength (descending).
    pub components: Vec<ComponentEntry>,
    /// One-line nudge for what to do next. Empty when no matches were found.
    pub next: String,
}

/// Map a UI-primitive keyword (case-insensitive substring) to one or more
/// canonical catalog names. The matcher is keyword → catalog name, not the
/// other way around, so synonyms ("drag-to-reorder" → drag_and_drop_list) can
/// be handled without rewriting the catalog. Keep this list short and
/// high-signal; the agent should still scan `list_components` for anything
/// that doesn't match.
const KEYWORD_HINTS: &[(&str, &[&str])] = &[
    ("drag", &["drag_and_drop_list"]),
    ("sortable", &["drag_and_drop_list"]),
    ("reorder", &["drag_and_drop_list"]),
    ("dialog", &["dialog", "alert_dialog"]),
    ("modal", &["dialog", "alert_dialog"]),
    ("combobox", &["combo_box"]),
    ("autocomplete", &["combo_box"]),
    ("typeahead", &["combo_box"]),
    ("date picker", &["date_picker", "calendar"]),
    ("datepicker", &["date_picker", "calendar"]),
    ("calendar", &["calendar", "date_picker"]),
    ("toast", &["toast"]),
    ("snackbar", &["toast"]),
    ("notification", &["toast"]),
    ("tooltip", &["tooltip"]),
    ("popover", &["popover"]),
    ("menu", &["dropdown_menu", "context_menu"]),
    ("dropdown", &["dropdown_menu"]),
    ("context menu", &["context_menu"]),
    ("tabs", &["tabs"]),
    ("accordion", &["accordion"]),
    ("slider", &["slider"]),
    ("switch", &["switch"]),
    ("toggle", &["switch"]),
    ("checkbox", &["checkbox"]),
    ("radio", &["radio_group"]),
    ("progress", &["progress"]),
    ("spinner", &["spinner"]),
    ("avatar", &["avatar"]),
    ("badge", &["badge"]),
    ("breadcrumb", &["breadcrumb"]),
    ("pagination", &["pagination"]),
    ("table", &["table"]),
    ("sheet", &["sheet"]),
    ("drawer", &["sheet"]),
    ("command", &["command"]),
    ("palette", &["command"]),
];

pub async fn suggest_components(
    p: SuggestComponentsParams,
) -> Result<SuggestComponentsResult, String> {
    let prompt_lc = p.prompt.to_ascii_lowercase();
    let mut matched_keywords: Vec<String> = Vec::new();
    let mut catalog_names: Vec<&'static str> = Vec::new();
    for (kw, names) in KEYWORD_HINTS {
        if prompt_lc.contains(kw) {
            matched_keywords.push((*kw).to_string());
            for n in *names {
                if !catalog_names.contains(n) {
                    catalog_names.push(*n);
                }
            }
        }
    }
    let mut components: Vec<ComponentEntry> = Vec::new();
    for name in &catalog_names {
        if let Some((n, desc, hint)) = DX_COMPONENT_CATALOG_ENTRIES
            .iter()
            .find(|(n, _, _)| n == name)
        {
            components.push(ComponentEntry {
                name: (*n).to_string(),
                description: (*desc).to_string(),
                prop_hint: (*hint).to_string(),
                import: format!("use crate::components::{n}::{};", to_pascal(n)),
                limitations: limitations_for(n).map(str::to_string),
            });
        }
    }
    let next = if components.is_empty() {
        String::new()
    } else {
        format!(
            "before writing handlers, run `dx components add {}` from the project root and then call `describe_component` for the prop surface",
            components.first().map(|c| c.name.as_str()).unwrap_or("")
        )
    };
    Ok(SuggestComponentsResult {
        matched_keywords,
        components,
        next,
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
        assert!(
            names.contains(&"calendar"),
            "expected calendar (desc mentions dates), got: {names:?}"
        );
        assert!(
            names.contains(&"date_picker"),
            "expected date_picker (name match), got: {names:?}"
        );
        assert!(
            !names.contains(&"button"),
            "button should not match 'date': {names:?}"
        );
    }

    #[tokio::test]
    async fn suggest_components_maps_drag_keyword_to_drag_and_drop_list() {
        let r = suggest_components(SuggestComponentsParams {
            prompt: "I want to build drag-to-reorder cards in this Kanban".into(),
        })
        .await
        .unwrap();
        let names: Vec<&str> = r.components.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"drag_and_drop_list"),
            "expected drag_and_drop_list suggestion, got: {names:?}"
        );
        assert!(
            !r.next.is_empty(),
            "should suggest a next action when matches are found"
        );
    }

    #[tokio::test]
    async fn suggest_components_returns_empty_for_unrelated_prompt() {
        let r = suggest_components(SuggestComponentsParams {
            prompt: "add a new field to my SQL schema".into(),
        })
        .await
        .unwrap();
        assert!(
            r.components.is_empty(),
            "unrelated prompt should not suggest widgets, got: {:?}",
            r.components
        );
        assert!(r.next.is_empty(), "no next action when no matches");
    }

    #[tokio::test]
    async fn drag_and_drop_list_surface_includes_limitations() {
        let r = list_components(ListComponentsParams {
            query: Some("drag_and_drop_list".into()),
        })
        .await
        .unwrap();
        let entry = r
            .components
            .iter()
            .find(|c| c.name == "drag_and_drop_list")
            .expect("drag_and_drop_list must be returned");
        let lim = entry
            .limitations
            .as_deref()
            .expect("drag_and_drop_list should carry a limitations note");
        assert!(
            lim.contains("cross-list") || lim.contains("cross-column"),
            "limitation should mention the cross-column shortcoming; got: {lim}"
        );
    }

    #[tokio::test]
    async fn list_components_omits_limitations_when_none() {
        let r = list_components(ListComponentsParams {
            query: Some("button".into()),
        })
        .await
        .unwrap();
        let entry = r
            .components
            .iter()
            .find(|c| c.name == "button")
            .expect("button must be returned");
        assert!(
            entry.limitations.is_none(),
            "button has no known limitations; got: {:?}",
            entry.limitations
        );
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
        assert_eq!(
            entry.import,
            "use crate::components::dropdown_menu::DropdownMenu;"
        );
    }
}
