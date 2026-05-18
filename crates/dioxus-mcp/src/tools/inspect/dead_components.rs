use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use proc_macro2::TokenTree;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::visit::Visit;

use crate::state::State;
use crate::tools::ast::{ParseError, collect_parse_errors, walk_rs_files};
use crate::tools::scaffold::crate_root;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct DeadComponentsParams {
    /// Additional component names to treat as alive. `App` and all components reachable
    /// from a Routable enum are always treated as roots.
    #[serde(default)]
    pub roots: Option<Vec<String>>,
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DeadComponent {
    pub name: String,
    pub file: PathBuf,
    pub line: usize,
    /// "catalog_unused" when the component is a freshly-installed catalog
    /// widget waiting to be wired into the app (`src/components/<name>/component.rs`
    /// with a catalog-known name). "abandoned" for everything else — the
    /// classic dead-component case where the file should probably be deleted
    /// or imported somewhere.
    pub kind: &'static str,
    /// Paste-ready import line. Only set for `kind: "catalog_unused"` so the
    /// caller can drop the widget into their rsx! without round-tripping
    /// through `describe_component`. None for `abandoned`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DeadComponentsReport {
    pub dead: Vec<DeadComponent>,
    pub roots: Vec<String>,
    pub total_components: usize,
    pub parse_errors: Vec<ParseError>,
}

pub async fn dead_components(
    state: &Arc<State>,
    p: DeadComponentsParams,
) -> Result<DeadComponentsReport, String> {
    let crate_root = crate_root(state, p.project_root.as_deref()).await?;
    let src_root = crate_root.join("src");

    let index = crate::tools::inspect::project_index::project_index(
        state,
        crate::tools::inspect::project_index::ProjectIndexParams {
            path: None,
            kind: Some("component".into()),
            project_root: p.project_root.clone(),
        },
    )
    .await?;

    let route_roots: HashSet<String> = match crate::tools::inspect::route_map::route_map(
        state,
        crate::tools::inspect::route_map::RouteMapParams {
            router_file: None,
            project_root: p.project_root.clone(),
        },
    )
    .await
    {
        Ok(rm) => {
            let mut s: HashSet<String> = rm.routes.iter().map(|r| r.component.clone()).collect();
            for r in &rm.routes {
                for l in &r.layouts {
                    s.insert(l.clone());
                }
            }
            s
        }
        Err(_) => HashSet::new(),
    };

    let mut roots: HashSet<String> = HashSet::new();
    roots.insert("App".to_string());
    roots.extend(route_roots);
    if let Some(extra) = p.roots {
        roots.extend(extra);
    }

    let component_names: HashSet<String> =
        index.components.iter().map(|c| c.name.clone()).collect();

    // Walk src, count invocations of each known component inside rsx! blocks.
    let mut used: HashSet<String> = HashSet::new();
    let files = walk_rs_files(&src_root);
    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
        let mut v = RsxComponentVisitor {
            known: &component_names,
            used: &mut used,
        };
        v.visit_file(ast);
    }

    let mut roots_vec: Vec<String> = roots.iter().cloned().collect();
    roots_vec.sort();

    let catalog_names: HashSet<&'static str> = crate::tools::dsl::dx_component_names().collect();
    let total_components = index.components.len();
    let mut dead: Vec<DeadComponent> = index
        .components
        .into_iter()
        .filter(|c| !used.contains(&c.name) && !roots.contains(&c.name))
        .map(|c| {
            let (kind, suggestion) =
                classify_dead_component(&c.name, &c.file, &crate_root, &catalog_names);
            DeadComponent {
                name: c.name,
                file: c.file,
                line: c.line,
                kind,
                suggestion,
            }
        })
        .collect();
    dead.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(DeadComponentsReport {
        dead,
        roots: roots_vec,
        total_components,
        parse_errors: collect_parse_errors(&files),
    })
}

/// Pick the right label and (for catalog widgets) a paste-ready import
/// line for a dead component. The catalog shape is
/// `src/components/<snake_name>/component.rs` — that's what `dx components add`
/// emits and what `verify_install` looks for. Hand-rolled dead components
/// (top-level `src/components/foo.rs` or arbitrary other locations) are
/// "abandoned" and get no suggestion — those are the ones the user actually
/// wants to delete.
fn classify_dead_component(
    name: &str,
    file: &std::path::Path,
    crate_root: &std::path::Path,
    catalog_names: &HashSet<&'static str>,
) -> (&'static str, Option<String>) {
    use heck::ToSnakeCase;
    let snake = name.to_snake_case();
    let expected = crate_root
        .join("src")
        .join("components")
        .join(&snake)
        .join("component.rs");
    let is_catalog_shape = file == expected.as_path();
    if !is_catalog_shape {
        return ("abandoned", None);
    }
    // Catalog-shaped dir but the component name isn't in the canonical 0.7
    // catalog — probably a hand-rolled component using the same layout, so
    // we don't claim it's awaiting an import. Treat as abandoned so the
    // caller knows it's safe to remove.
    if !catalog_names.contains(snake.as_str()) {
        return ("abandoned", None);
    }
    let suggestion = format!(
        "freshly installed via `dx components add {snake}` but never imported. Drop into an rsx! body with `use crate::components::{snake}::{name};` then `{name} {{ /* props */ }}`."
    );
    ("catalog_unused", Some(suggestion))
}

struct RsxComponentVisitor<'a> {
    known: &'a HashSet<String>,
    used: &'a mut HashSet<String>,
}

impl<'a, 'ast> Visit<'ast> for RsxComponentVisitor<'a> {
    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        let is_rsx = m
            .path
            .segments
            .last()
            .map(|s| s.ident == "rsx")
            .unwrap_or(false);
        if is_rsx {
            let tokens: Vec<TokenTree> = m.tokens.clone().into_iter().collect();
            scan_for_components(&tokens, self.known, self.used);
        }
        syn::visit::visit_macro(self, m);
    }
}

fn scan_for_components(tokens: &[TokenTree], known: &HashSet<String>, used: &mut HashSet<String>) {
    let mut i = 0;
    while i < tokens.len() {
        if let TokenTree::Ident(id) = &tokens[i] {
            let name = id.to_string();
            // Component invocation: `Ident {` (or `path::Ident {`).
            if known.contains(&name)
                && let Some(TokenTree::Group(g)) = tokens.get(i + 1)
                && g.delimiter() == proc_macro2::Delimiter::Brace
            {
                used.insert(name);
            }
        }
        i += 1;
    }
    // Recurse into groups for nested rsx! children.
    for tt in tokens {
        if let TokenTree::Group(g) = tt {
            let inner: Vec<TokenTree> = g.stream().into_iter().collect();
            scan_for_components(&inner, known, used);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn catalog() -> HashSet<&'static str> {
        crate::tools::dsl::dx_component_names().collect()
    }

    #[test]
    fn classifies_catalog_install_layout_as_catalog_unused() {
        let crate_root = Path::new("/tmp/proj");
        let file = crate_root.join("src/components/button/component.rs");
        let (kind, suggestion) = classify_dead_component("Button", &file, crate_root, &catalog());
        assert_eq!(kind, "catalog_unused");
        let msg = suggestion.expect("suggestion should be set for catalog widgets");
        assert!(msg.contains("dx components add button"), "{msg}");
        assert!(
            msg.contains("use crate::components::button::Button"),
            "{msg}"
        );
    }

    #[test]
    fn classifies_handrolled_top_level_component_as_abandoned() {
        let crate_root = Path::new("/tmp/proj");
        // Top-level src/components/<name>.rs is the hand-rolled / scaffolded
        // shape — not the catalog dir. Should be flagged as abandoned and
        // get no import hint.
        let file = crate_root.join("src/components/widget.rs");
        let (kind, suggestion) = classify_dead_component("Widget", &file, crate_root, &catalog());
        assert_eq!(kind, "abandoned");
        assert!(suggestion.is_none());
    }

    #[test]
    fn classifies_catalog_shape_with_unknown_name_as_abandoned() {
        // A hand-authored component that just happens to use the catalog
        // directory layout but isn't in the canonical catalog list. Don't
        // claim it's "awaiting install" — let the user clean it up.
        let crate_root = Path::new("/tmp/proj");
        let file = crate_root.join("src/components/custom_widget/component.rs");
        let (kind, suggestion) =
            classify_dead_component("CustomWidget", &file, crate_root, &catalog());
        assert_eq!(kind, "abandoned");
        assert!(suggestion.is_none());
    }
}
