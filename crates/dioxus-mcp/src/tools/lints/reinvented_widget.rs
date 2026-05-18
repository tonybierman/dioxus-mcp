//! `reinvented_widget`: spot components hand-rolling UI patterns the catalog
//! already covers.
//!
//! Today the lint checks for two shapes:
//!
//! 1. The HTML5 drag-and-drop triplet (`ondragstart` + `ondragover` +
//!    `ondrop`) — full triplet emits `confidence: high`, the drop-target
//!    half on its own emits `confidence: low` (kanban-column shape).
//! 2. Bare DOM elements whose catalog equivalent exists (`<select>`,
//!    `<dialog>`, `<textarea>`, `<input>`) — `confidence: low`. The
//!    hand-rolled DOM forms are correct, just not the catalog-blessed
//!    primitive that ships with theming / a11y / keyboard navigation.
//!
//! All findings are HINTS, not errors. The drag-and-drop catalog widget
//! is a single sortable list (see `list_components` → `limitations`), so
//! kanban-style boards with multiple drop targets genuinely need the
//! hand-rolled pattern. Same for `<input type="file">` and other
//! specialised forms the catalog doesn't model.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use proc_macro2::TokenTree;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::visit::Visit;

use crate::state::State;
use crate::tools::ast::{ParseError, collect_parse_errors, is_catalog_wrapper, walk_rs_files};
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
    /// `"high"` for the full ondragstart+ondragover+ondrop triplet on one
    /// component (clear hand-rolled drag interaction). `"low"` for partial
    /// shapes — currently the `ondragover`+`ondrop` drop-target half by
    /// itself, which often pairs with `ondragstart` on a sibling card
    /// component (kanban boards). Callers can filter low-confidence
    /// findings out for noisier dashboards.
    pub confidence: &'static str,
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

/// HTML elements whose direct catalog equivalent ships with the dx-components
/// catalog. The lint flags each occurrence at `confidence: low` — the bare
/// DOM form is functionally correct, just unstyled and missing the catalog's
/// keyboard / a11y / theming defaults. The mapping is intentionally
/// conservative: only elements where the catalog widget is a *direct*
/// drop-in (a styled `<select>` etc.), not "a Tabs widget could replace
/// these radio buttons."
const DOM_TO_CATALOG: &[(&str, &str)] = &[
    ("select", "select"),
    ("dialog", "dialog"),
    ("textarea", "textarea"),
    // `<input>` is broader (text vs checkbox vs radio vs file vs date), so
    // we suggest the closest single-type catalog widget (`input`) and let
    // the agent pick a more specific one (`checkbox`, `radio_group`,
    // `date_picker`, `slider`) when the `type=` says so. Confidence stays
    // low because <input type=file> has no catalog equivalent.
    ("input", "input"),
];

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
        // Track every bare DOM element we want to flag — keyed by catalog
        // name so the same `<select>` appearing twice in one component only
        // surfaces once (the suggestion is the same either way).
        let mut dom_hits: std::collections::BTreeMap<&'static str, (&'static str, usize)> =
            std::collections::BTreeMap::new();
        for body in &collector.rsx_bodies {
            for tt in body.clone() {
                scan_event_handlers(
                    tt.clone(),
                    &mut has_dragstart,
                    &mut has_dragover,
                    &mut has_drop,
                    &mut first_line,
                );
            }
            scan_dom_elements(body.clone(), &mut dom_hits);
        }
        for (dom_name, (catalog_name, line)) in dom_hits {
            out.push(ReinventedFinding {
                reinvented: catalog_name,
                component: component.clone(),
                file: file.to_path_buf(),
                line,
                confidence: "low",
                hint: format!(
                    "`<{dom_name}>` is a bare DOM element; the catalog ships `{catalog_name}` \
                     with theming, keyboard navigation, and a11y wiring already done. \
                     `dx components add {catalog_name}` to install. `confidence: low` because \
                     specialised forms (e.g. `<input type=\"file\">`) have no catalog \
                     equivalent — verify the use case before swapping."
                ),
            });
        }
        if has_dragstart && has_dragover && has_drop {
            let line = first_line.unwrap_or_else(|| f.sig.ident.span().start().line);
            out.push(ReinventedFinding {
                reinvented: "drag_and_drop_list",
                component,
                file: file.to_path_buf(),
                line,
                confidence: "high",
                hint: "all three of `ondragstart` / `ondragover` / `ondrop` are wired here. \
                       Catalog widget `drag_and_drop_list` covers single-list reordering with \
                       pointer / keyboard / touch out of the box. Caveat: it does NOT support \
                       cross-list drops, so kanban-style boards with multiple drop targets \
                       genuinely need this hand-rolled pattern — verify shape before installing."
                    .into(),
            });
        } else if has_dragover && has_drop {
            // Drop-target-only shape: classic kanban column that receives
            // dropped cards but doesn't initiate drags itself (the cards
            // do, in a sibling component). Standup's `Column` is this
            // shape — exactly the case the TODO called out. Lower
            // confidence because a stray `ondrop`+`ondragover` on a
            // non-drag-target component (e.g., a file-upload drop zone)
            // would also match and isn't necessarily a catalog candidate.
            let line = first_line.unwrap_or_else(|| f.sig.ident.span().start().line);
            out.push(ReinventedFinding {
                reinvented: "drag_and_drop_list",
                component,
                file: file.to_path_buf(),
                line,
                confidence: "low",
                hint: "drop-target half of HTML5 drag-and-drop (`ondragover` + `ondrop`) is \
                       wired here without `ondragstart`. Common kanban shape: the card \
                       component (a sibling) starts the drag, this component receives it. \
                       Catalog `drag_and_drop_list` covers single-list reordering but does \
                       NOT support cross-list drops, so a multi-column board legitimately \
                       needs the hand-rolled pattern. Filter `confidence: \"low\"` out if \
                       you only want the full-triplet hits."
                    .into(),
            });
        }
    }
}

/// Walk an rsx token stream looking for `<dom_element> { ... }` shapes whose
/// catalog equivalent we want to suggest. The detection is purely structural:
/// an `Ident` immediately followed by a `Group { ... }` with brace delimiter
/// is an rsx element. We DON'T descend into the group as a child-element
/// position (we want all matches), but we DO skip the `if let X = ident:`
/// case by ignoring idents followed by `:` (those are props/handlers).
///
/// `hits` is keyed by the DOM element name so multiple `<select>` blocks in
/// one component dedupe to a single finding (the suggestion is the same).
fn scan_dom_elements(
    ts: proc_macro2::TokenStream,
    hits: &mut std::collections::BTreeMap<&'static str, (&'static str, usize)>,
) {
    let trees: Vec<TokenTree> = ts.into_iter().collect();
    let mut i = 0;
    while i < trees.len() {
        if let TokenTree::Ident(id) = &trees[i] {
            let s = id.to_string();
            let next = trees.get(i + 1);
            let is_element = matches!(
                next,
                Some(TokenTree::Group(g)) if g.delimiter() == proc_macro2::Delimiter::Brace
            );
            if is_element
                && let Some((_, catalog)) =
                    DOM_TO_CATALOG.iter().find(|(dom, _)| *dom == s.as_str())
                && !hits.contains_key(*catalog)
            {
                hits.insert(catalog, (catalog, id.span().start().line));
            }
        }
        if let TokenTree::Group(g) = &trees[i] {
            scan_dom_elements(g.stream(), hits);
        }
        i += 1;
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
        assert_eq!(findings[0].confidence, "high");
        assert!(findings[0].hint.contains("cross-list"));
    }

    #[test]
    fn does_not_flag_draggable_only_partial() {
        let dir = tempdir().unwrap();
        // Two of three handlers — common shape for a draggable card that
        // ISN'T also a drop target (ondragstart + ondragend, no drop).
        // Don't flag — the drop-target subset is what carries signal.
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

    /// Standup `Column` shape: `ondragover` + `ondrop` on a drop-target
    /// column whose `ondragstart` lives on a sibling card component. The
    /// TODO called this out as a P3 — should emit a low-confidence finding
    /// so callers know "this is part of a hand-rolled drag interaction the
    /// catalog could partially cover."
    #[test]
    fn flags_drop_target_only_pair_with_low_confidence() {
        let dir = tempdir().unwrap();
        write_file(
            &dir.path().join("src/components/column.rs"),
            r#"use dioxus::prelude::*;
#[component]
fn Column() -> Element {
    rsx! {
        div {
            ondragover: move |e| { e.prevent_default(); },
            ondrop: move |_| {},
        }
    }
}
"#,
        );
        let findings = scan(dir.path());
        assert_eq!(findings.len(), 1, "expected one finding: {findings:?}");
        assert_eq!(findings[0].confidence, "low");
        assert!(
            findings[0].hint.contains("drop-target"),
            "hint should call out the drop-target shape: {}",
            findings[0].hint
        );
    }

    /// `ondragover` alone (without `ondrop`) is a layout reset / debugging
    /// pattern, NOT a drag interaction — don't emit even the low-confidence
    /// finding.
    #[test]
    fn does_not_flag_dragover_alone() {
        let dir = tempdir().unwrap();
        write_file(
            &dir.path().join("src/components/zone.rs"),
            r#"use dioxus::prelude::*;
#[component]
fn Zone() -> Element {
    rsx! {
        div {
            ondragover: move |e| { e.prevent_default(); },
        }
    }
}
"#,
        );
        assert!(scan(dir.path()).is_empty());
    }

    /// Standup `BoardScreen`'s compose form uses a bare `<select>` for the
    /// column picker; the catalog ships `select`. Before the fix the lint
    /// only caught drag-and-drop, so this hand-rolled DOM form slipped
    /// through. Now it emits a `confidence: low` finding pointing at the
    /// catalog widget.
    #[test]
    fn flags_bare_select_with_catalog_equivalent() {
        let dir = tempdir().unwrap();
        write_file(
            &dir.path().join("src/board_screen.rs"),
            r#"use dioxus::prelude::*;
#[component]
fn BoardScreen() -> Element {
    rsx! {
        form {
            select {
                option { "todo" }
                option { "doing" }
            }
        }
    }
}
"#,
        );
        let findings = scan(dir.path());
        let select_finding = findings
            .iter()
            .find(|f| f.reinvented == "select")
            .expect("bare <select> should be flagged");
        assert_eq!(select_finding.component, "BoardScreen");
        assert_eq!(select_finding.confidence, "low");
        assert!(
            select_finding.hint.contains("dx components add select"),
            "hint should suggest the catalog install command: {}",
            select_finding.hint,
        );
    }

    /// PascalCase idents (i.e. real Dioxus components like the catalog
    /// `Select`) must NOT match the lowercase-DOM rule. Without this guard
    /// a catalog user (who installed `select` and is rendering `Select {}`)
    /// would get a noisy false positive.
    #[test]
    fn does_not_flag_pascal_case_catalog_components() {
        let dir = tempdir().unwrap();
        write_file(
            &dir.path().join("src/board_screen.rs"),
            r#"use dioxus::prelude::*;
#[component]
fn BoardScreen() -> Element {
    rsx! {
        Select {
            value: "todo",
        }
    }
}
"#,
        );
        assert!(
            scan(dir.path()).is_empty(),
            "PascalCase Select must not trigger the lowercase-DOM lint"
        );
    }

    /// Multiple `<select>` elements in one component dedupe to a single
    /// finding — they all surface the same install suggestion, so listing
    /// each occurrence is noise.
    #[test]
    fn dedupes_multiple_bare_selects_in_one_component() {
        let dir = tempdir().unwrap();
        write_file(
            &dir.path().join("src/board_screen.rs"),
            r#"use dioxus::prelude::*;
#[component]
fn BoardScreen() -> Element {
    rsx! {
        select {}
        select {}
        select {}
    }
}
"#,
        );
        let findings = scan(dir.path());
        let select_findings: Vec<&ReinventedFinding> = findings
            .iter()
            .filter(|f| f.reinvented == "select")
            .collect();
        assert_eq!(
            select_findings.len(),
            1,
            "three occurrences must dedupe to one finding: {findings:?}"
        );
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
