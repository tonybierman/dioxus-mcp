//! Screen navigator: a left rail that groups screens by data object (resource)
//! and shows ONE screen at a time in the stage, instead of dumping every screen
//! in a flat scroll. A real CRUD app produces ~5 screens per resource
//! (list/new/edit, …), so the flat stack doesn't scale and hides the fact that
//! those screens are a navigational flow around one resource.
//!
//! It unifies the two preview sources — handwritten `screens:` (parsed locally,
//! [`PreviewItem::Screen`]) and server-synthesized `render_models` (from the
//! dry-run, [`PreviewItem::Model`]), which are disjoint — into one grouped
//! index. The active screen is *derived* via a memo (never reconciled in an
//! effect: reading a prop and writing the signal you also read is the classic
//! infinite-loop trap), so selection self-heals when an edit removes a screen.
//!
//! A lightweight [`PreviewNav`] handle is provided via context so in-preview
//! affordances (a list's "New" button, a row → edit, a form → back-to-list) can
//! switch the active screen. These only mutate the `selected` signal — nothing
//! navigates the real browser, since this is a wasm SPA.

use std::collections::HashMap;

use dioxus::prelude::*;

use crate::model::{RenderModel, Screen};

use super::{RenderModelView, ScreenPreview};

/// One previewable screen, from either source. Both inner types are
/// `Clone + PartialEq`, so `PreviewGroup` can derive `PartialEq` — required for
/// the parent `use_memo` over groups to memoize and avoid spurious remounts.
#[derive(Debug, Clone, PartialEq)]
pub enum PreviewItem {
    Screen(Screen),
    Model(RenderModel),
}

/// A single rail entry.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewEntry {
    /// Selection key — the screen name, unique within a doc.
    pub id: String,
    /// Short rail label (kind-derived: "List"/"New"/"Edit", else the name).
    pub label: String,
    /// The screen's route (may be empty).
    pub route: String,
    /// Template kind, used for ordering and for fake-router resolution.
    pub kind: String,
    pub item: PreviewItem,
}

/// Entries sharing one data object (or the catch-all "Pages").
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewGroup {
    /// Group heading — the resource's `item_type`, or "Pages".
    pub heading: String,
    pub entries: Vec<PreviewEntry>,
}

/// Sort key within a group: the canonical CRUD flow order, then everything else.
fn kind_rank(kind: &str) -> u8 {
    match kind {
        "resource_list" => 0,
        "resource_form" => 1,
        "resource_edit_form" => 2,
        _ => 3,
    }
}

/// Short rail label. Resource screens get a flow label; anything else (a
/// `client_crud` or handwritten page) shows its name, which is more telling.
fn label_for(kind: &str, name: &str) -> String {
    match kind {
        "resource_list" => "List".into(),
        "resource_form" => "New".into(),
        "resource_edit_form" => "Edit".into(),
        _ => name.to_string(),
    }
}

/// Group key for a handwritten screen: its template `item_type`, else "Pages".
fn screen_group(s: &Screen) -> String {
    s.template
        .as_ref()
        .and_then(|t| t.item_type.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Pages".into())
}

/// Merge the two disjoint sources into resource-grouped, flow-ordered entries.
/// Groups keep first-appearance order (handwritten screens first, then models).
pub fn build_groups(screens: &[Screen], models: &[RenderModel]) -> Vec<PreviewGroup> {
    let mut order: Vec<String> = Vec::new();
    let mut by_key: HashMap<String, Vec<PreviewEntry>> = HashMap::new();

    let mut push = |key: String, entry: PreviewEntry| {
        if !by_key.contains_key(&key) {
            order.push(key.clone());
        }
        by_key.entry(key).or_default().push(entry);
    };

    for s in screens {
        // Mirror ScreenPreview's dispatch: a screen with no template is "empty".
        let kind = s
            .template
            .as_ref()
            .map(|t| t.kind.clone())
            .unwrap_or_else(|| "empty".into());
        push(
            screen_group(s),
            PreviewEntry {
                id: s.name.clone(),
                label: label_for(&kind, &s.name),
                route: s.route.clone().unwrap_or_default(),
                kind,
                item: PreviewItem::Screen(s.clone()),
            },
        );
    }

    for m in models {
        let key = if m.item_type.is_empty() {
            "Pages".into()
        } else {
            m.item_type.clone()
        };
        push(
            key,
            PreviewEntry {
                id: m.screen.clone(),
                label: label_for(&m.kind, &m.screen),
                route: m.route.clone(),
                kind: m.kind.clone(),
                item: PreviewItem::Model(m.clone()),
            },
        );
    }

    order
        .into_iter()
        .map(|heading| {
            let mut entries = by_key.remove(&heading).unwrap_or_default();
            entries.sort_by(|a, b| {
                kind_rank(&a.kind)
                    .cmp(&kind_rank(&b.kind))
                    .then_with(|| a.id.cmp(&b.id))
            });
            PreviewGroup { heading, entries }
        })
        .collect()
}

/// Context handle the preview components use to switch the active screen. Copy
/// (a `Callback` plus two `Memo`s are all `Copy`), so children just
/// `try_consume_context::<PreviewNav>()` and call it.
#[derive(Clone, Copy)]
pub struct PreviewNav {
    /// Set the active screen by entry id.
    select: Callback<String>,
    /// (item_type, kind) → entry id — for targets whose concrete route is mock
    /// in the preview (a list row's `/x/:id/edit`, a form's back-to-list).
    by_resource: Memo<HashMap<(String, String), String>>,
    /// Concrete route → entry id (e.g. a list's `new_route`).
    by_route: Memo<HashMap<String, String>>,
}

impl PreviewNav {
    /// Navigate to the screen at `route`, if one exists.
    pub fn go_route(&self, route: &str) {
        let target = self.by_route.read().get(route).cloned();
        if let Some(id) = target {
            self.select.call(id);
        }
    }

    /// Is there a screen of `kind` for this resource?
    pub fn has_resource(&self, item_type: &str, kind: &str) -> bool {
        self.by_resource
            .read()
            .contains_key(&(item_type.to_string(), kind.to_string()))
    }

    /// Navigate to the `kind` screen of `item_type`, if one exists.
    pub fn go_resource(&self, item_type: &str, kind: &str) {
        let target = self
            .by_resource
            .read()
            .get(&(item_type.to_string(), kind.to_string()))
            .cloned();
        if let Some(id) = target {
            self.select.call(id);
        }
    }
}

#[component]
pub fn ScreenNavigator(groups: ReadSignal<Vec<PreviewGroup>>) -> Element {
    // The user's explicit click. May be None (initial) or stale (after an edit
    // removed the screen) — `effective` heals both without writing.
    let mut selected = use_signal(|| None::<String>);

    let effective = use_memo(move || {
        let gs = groups();
        let mut ids = gs.iter().flat_map(|g| g.entries.iter().map(|e| e.id.clone()));
        match selected() {
            Some(want) if gs.iter().any(|g| g.entries.iter().any(|e| e.id == want)) => Some(want),
            _ => ids.next(),
        }
    });

    let active = use_memo(move || {
        let want = effective()?;
        groups()
            .iter()
            .flat_map(|g| g.entries.iter())
            .find(|e| e.id == want)
            .cloned()
    });

    // Fake-router lookup tables, derived from groups (stay current).
    let by_route = use_memo(move || {
        let mut m: HashMap<String, String> = HashMap::new();
        for g in groups().iter() {
            for e in g.entries.iter() {
                if !e.route.is_empty() {
                    m.entry(e.route.clone()).or_insert_with(|| e.id.clone());
                }
            }
        }
        m
    });
    let by_resource = use_memo(move || {
        let mut m: HashMap<(String, String), String> = HashMap::new();
        for g in groups().iter() {
            for e in g.entries.iter() {
                m.entry((g.heading.clone(), e.kind.clone()))
                    .or_insert_with(|| e.id.clone());
            }
        }
        m
    });

    let select = use_callback(move |id: String| selected.set(Some(id)));
    use_context_provider(|| PreviewNav {
        select,
        by_resource,
        by_route,
    });

    rsx! {
        div { class: "preview-with-nav",
            nav { class: "preview-nav",
                for group in groups().iter() {
                    div { class: "preview-nav-group", "{group.heading}" }
                    for entry in group.entries.iter() {
                        button {
                            key: "{entry.id}",
                            class: if effective() == Some(entry.id.clone()) { "preview-nav-item active" } else { "preview-nav-item" },
                            onclick: {
                                let id = entry.id.clone();
                                move |_| select.call(id.clone())
                            },
                            span { class: "nav-item-label", "{entry.label}" }
                            if !entry.route.is_empty() {
                                span { class: "nav-item-route", "{entry.route}" }
                            }
                        }
                    }
                }
            }
            div { class: "preview-stage",
                div { class: "preview-root",
                    match active() {
                        Some(entry) => {
                            let id = entry.id.clone();
                            match entry.item {
                                PreviewItem::Screen(s) => rsx! { ScreenPreview { key: "{id}", screen: s } },
                                PreviewItem::Model(m) => rsx! { RenderModelView { key: "{id}", model: m } },
                            }
                        }
                        None => rsx! { p { class: "preview-hint", "No screens to preview." } },
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{RenderModel, Screen, ScreenTemplate};

    fn screen(name: &str, route: &str, kind: &str, item_type: Option<&str>) -> Screen {
        Screen {
            name: name.into(),
            route: Some(route.into()),
            template: Some(ScreenTemplate {
                kind: kind.into(),
                item_type: item_type.map(Into::into),
                ..Default::default()
            }),
        }
    }

    fn model(screen: &str, kind: &str, item_type: &str, route: &str) -> RenderModel {
        RenderModel {
            screen: screen.into(),
            kind: kind.into(),
            route: route.into(),
            item_type: item_type.into(),
            ..Default::default()
        }
    }

    #[test]
    fn groups_models_by_item_type_in_flow_order() {
        // Intentionally out of flow order to prove the sort.
        let models = vec![
            model("ProductEdit", "resource_edit_form", "Product", "/products/:id/edit"),
            model("ProductList", "resource_list", "Product", "/products"),
            model("ProductNew", "resource_form", "Product", "/products/new"),
        ];
        let groups = build_groups(&[], &models);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].heading, "Product");
        let labels: Vec<_> = groups[0].entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["List", "New", "Edit"]);
    }

    #[test]
    fn screens_without_item_type_fall_into_pages() {
        let screens = vec![
            screen("Dashboard", "/", "empty", None),
            screen("TodoScreen", "/todos", "client_crud", Some("Todo")),
        ];
        let groups = build_groups(&screens, &[]);

        let headings: Vec<_> = groups.iter().map(|g| g.heading.as_str()).collect();
        // First-appearance order: Dashboard ("Pages") seen before the Todo group.
        assert_eq!(headings, ["Pages", "Todo"]);
        let pages = groups.iter().find(|g| g.heading == "Pages").unwrap();
        assert_eq!(pages.entries.len(), 1);
        assert_eq!(pages.entries[0].label, "Dashboard");
    }

    #[test]
    fn handwritten_and_synthesized_merge_disjointly() {
        let screens = vec![screen("TodoScreen", "/todos", "client_crud", Some("Todo"))];
        let models = vec![model("ProductList", "resource_list", "Product", "/products")];
        let groups = build_groups(&screens, &models);

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].heading, "Todo");
        assert_eq!(groups[1].heading, "Product");
    }
}
