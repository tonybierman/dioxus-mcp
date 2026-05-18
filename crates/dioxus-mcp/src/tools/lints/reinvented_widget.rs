//! `reinvented_widget`: spot components hand-rolling UI patterns the catalog
//! already covers.
//!
//! Today the lint only checks for the HTML5 drag-and-drop triplet
//! (`ondragstart` + `ondragover` + `ondrop`). When all three fire from the
//! same component AND the file isn't a catalog wrapper, we emit a hint
//! pointing at `drag_and_drop_list`. This is a HINT, not an error — the
//! catalog `drag_and_drop_list` is a single sortable list (see
//! `list_components` → `limitations`), so kanban-style boards with multiple
//! drop targets genuinely need the hand-rolled pattern. The hint is meant to
//! flag "did you check the catalog?" not "you got it wrong."

use std::path::{Path, PathBuf};
use std::sync::Arc;

use proc_macro2::TokenTree;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::visit::Visit;

use crate::state::State;
use crate::tools::ast::{ParseError, collect_parse_errors, walk_rs_files};
use crate::tools::scaffold::crate_root;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ReinventedWidgetParams {
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ReinventedFinding {
    /// `drag_and_drop_list` for now — keyed by catalog name so callers can
    /// branch per-widget once more reinventions are detected.
    pub reinvented: &'static str,
    pub component: String,
    pub file: PathBuf,
    pub line: usize,
    /// Free-form hint surfaced to the agent. Includes the catalog
    /// limitations note so callers don't blindly install over the top of a
    /// shape the catalog can't model.
    pub hint: String,
}

#[derive(Debug, Serialize)]
pub struct ReinventedWidgetReport {
    pub findings: Vec<ReinventedFinding>,
    pub parse_errors: Vec<ParseError>,
}

pub async fn reinvented_widget(
    state: &Arc<State>,
    p: ReinventedWidgetParams,
) -> Result<ReinventedWidgetReport, String> {
    let crate_root = crate_root(state, p.project_root.as_deref()).await?;
    let src_root = crate_root.join("src");
    let files = walk_rs_files(&src_root);

    let mut findings: Vec<ReinventedFinding> = Vec::new();
    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
        // Catalog wrappers live under `src/components/<catalog_name>/`. Skip
        // them — the wrapper file emits these handlers BY DEFINITION (it's
        // the catalog widget itself).
        if is_catalog_wrapper(&sf.path, &src_root) {
            continue;
        }
        scan_file(ast, &sf.path, &mut findings);
    }
    findings.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.component.cmp(&b.component))
    });

    Ok(ReinventedWidgetReport {
        findings,
        parse_errors: collect_parse_errors(&files),
    })
}

fn is_catalog_wrapper(path: &Path, src_root: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(src_root) else {
        return false;
    };
    let mut comps = rel.components();
    let Some(first) = comps.next() else {
        return false;
    };
    if first.as_os_str() != "components" {
        return false;
    }
    let Some(second) = comps.next() else {
        return false;
    };
    let widget_name = second.as_os_str().to_string_lossy();
    crate::tools::dsl::dx_component_names().any(|n| n == widget_name)
}

fn scan_file(ast: &syn::File, file: &Path, out: &mut Vec<ReinventedFinding>) {
    for item in &ast.items {
        let syn::Item::Fn(f) = item else { continue };
        let is_component = f.attrs.iter().any(|a| {
            a.path()
                .segments
                .last()
                .map(|s| s.ident == "component")
                .unwrap_or(false)
        });
        if !is_component {
            continue;
        }
        let component = f.sig.ident.to_string();

        let mut collector = RsxCollector {
            rsx_bodies: Vec::new(),
        };
        collector.visit_block(&f.block);

        let mut has_dragstart = false;
        let mut has_dragover = false;
        let mut has_drop = false;
        let mut first_line: Option<usize> = None;
        for body in &collector.rsx_bodies {
            for tt in body.clone() {
                scan_event_handlers(
                    tt,
                    &mut has_dragstart,
                    &mut has_dragover,
                    &mut has_drop,
                    &mut first_line,
                );
            }
        }
        if has_dragstart && has_dragover && has_drop {
            let line = first_line.unwrap_or_else(|| f.sig.ident.span().start().line);
            out.push(ReinventedFinding {
                reinvented: "drag_and_drop_list",
                component,
                file: file.to_path_buf(),
                line,
                hint: "all three of `ondragstart` / `ondragover` / `ondrop` are wired here. \
                       Catalog widget `drag_and_drop_list` covers single-list reordering with \
                       pointer / keyboard / touch out of the box. Caveat: it does NOT support \
                       cross-list drops, so kanban-style boards with multiple drop targets \
                       genuinely need this hand-rolled pattern — verify shape before installing."
                    .into(),
            });
        }
    }
}

fn scan_event_handlers(
    tt: TokenTree,
    has_dragstart: &mut bool,
    has_dragover: &mut bool,
    has_drop: &mut bool,
    first_line: &mut Option<usize>,
) {
    match tt {
        TokenTree::Ident(id) => {
            let s = id.to_string();
            let matched = match s.as_str() {
                "ondragstart" => {
                    *has_dragstart = true;
                    true
                }
                "ondragover" => {
                    *has_dragover = true;
                    true
                }
                "ondrop" => {
                    *has_drop = true;
                    true
                }
                _ => false,
            };
            if matched && first_line.is_none() {
                *first_line = Some(id.span().start().line);
            }
        }
        TokenTree::Group(g) => {
            for inner in g.stream() {
                scan_event_handlers(inner, has_dragstart, has_dragover, has_drop, first_line);
            }
        }
        _ => {}
    }
}

struct RsxCollector {
    rsx_bodies: Vec<proc_macro2::TokenStream>,
}

impl<'ast> Visit<'ast> for RsxCollector {
    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        let is_rsx = m
            .path
            .segments
            .last()
            .map(|s| s.ident == "rsx")
            .unwrap_or(false);
        if is_rsx {
            self.rsx_bodies.push(m.tokens.clone());
        }
        syn::visit::visit_macro(self, m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_file(p: &Path, body: &str) {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, body).unwrap();
    }

    fn scan(crate_root: &Path) -> Vec<ReinventedFinding> {
        let src_root = crate_root.join("src");
        let files = walk_rs_files(&src_root);
        let mut findings = Vec::new();
        for sf in &files {
            let Ok(ast) = &sf.ast else { continue };
            if is_catalog_wrapper(&sf.path, &src_root) {
                continue;
            }
            scan_file(ast, &sf.path, &mut findings);
        }
        findings
    }

    #[test]
    fn flags_dnd_triplet_in_user_component() {
        let dir = tempdir().unwrap();
        write_file(
            &dir.path().join("src/components/board.rs"),
            r#"use dioxus::prelude::*;
#[component]
fn Board() -> Element {
    rsx! {
        div {
            ondragstart: move |_| {},
            ondragover: move |e| { e.prevent_default(); },
            ondrop: move |_| {},
        }
    }
}
"#,
        );
        let findings = scan(dir.path());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].reinvented, "drag_and_drop_list");
        assert_eq!(findings[0].component, "Board");
        assert!(findings[0].hint.contains("cross-list"));
    }

    #[test]
    fn does_not_flag_partial_triplet() {
        let dir = tempdir().unwrap();
        // Two of three handlers — common shape for a draggable card that
        // ISN'T also a drop target. Don't flag this.
        write_file(
            &dir.path().join("src/components/card.rs"),
            r#"use dioxus::prelude::*;
#[component]
fn Card() -> Element {
    rsx! {
        div {
            draggable: "true",
            ondragstart: move |_| {},
            ondragend: move |_| {},
        }
    }
}
"#,
        );
        assert!(scan(dir.path()).is_empty());
    }

    #[test]
    fn skips_catalog_wrappers() {
        // A catalog wrapper file (under src/components/<catalog_name>/) IS
        // the widget — it's allowed to emit all three handlers.
        let dir = tempdir().unwrap();
        write_file(
            &dir.path()
                .join("src/components/drag_and_drop_list/component.rs"),
            r#"use dioxus::prelude::*;
#[component]
pub fn DragAndDropList() -> Element {
    rsx! {
        div {
            ondragstart: move |_| {},
            ondragover: move |e| { e.prevent_default(); },
            ondrop: move |_| {},
        }
    }
}
"#,
        );
        assert!(
            scan(dir.path()).is_empty(),
            "catalog wrapper should be skipped"
        );
    }
}
