//! `components_audit`: post-hoc complement to `suggest_components`. Scans
//! every `#[component] fn` in `src/` for rsx `class: "<literal>"` attributes
//! whose tokens look like a catalog widget the user hand-rolled with classnames.
//!
//! Why this is separate from `reinvented_widget`: that lint keys on EVENT
//! handlers (`ondragstart` triplet) or BARE TAG names (`<select>`, `<dialog>`).
//! `components_audit` keys on classnames carried by generic `<div>` shapes
//! — the failure mode the deferred P4 in TODO.md called out (hand-rolled
//! modals, tabs, accordions, popovers, calendar grids). All findings are
//! `confidence: low`: class names are conventions, not contracts, so the
//! lint surfaces hints to the agent rather than errors.
//!
//! The classname → catalog table is intentionally narrow (modal, tabs,
//! accordion, popover, …). Generic tokens (`card`, `button`, `input`,
//! `navbar`) are excluded because they appear constantly on layout
//! elements that genuinely are not the catalog widget of that name.

use std::collections::BTreeMap;
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
pub struct ComponentsAuditParams {
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ComponentsAuditFinding {
    /// Catalog widget name (snake_case) the matched classname maps to.
    /// Pass verbatim to `dx components add <name>`.
    pub reinvented: &'static str,
    pub component: String,
    pub file: PathBuf,
    pub line: usize,
    /// The full class-attribute literal that triggered the hit (so the
    /// agent can see the surrounding context — `class: "modal large"` is
    /// more informative than just `modal`).
    pub matched_class: String,
    /// Always `"low"`: classname-based detection is conventional, not
    /// structural. The agent should verify the shape before swapping.
    pub confidence: &'static str,
    pub hint: String,
}

#[derive(Debug, Serialize)]
pub struct ComponentsAuditReport {
    pub findings: Vec<ComponentsAuditFinding>,
    pub parse_errors: Vec<ParseError>,
}

pub async fn components_audit(
    state: &Arc<State>,
    p: ComponentsAuditParams,
) -> Result<ComponentsAuditReport, String> {
    let crate_root = crate_root(state, p.project_root.as_deref()).await?;
    let src_root = crate_root.join("src");
    let files = walk_rs_files(&src_root);

    let mut findings: Vec<ComponentsAuditFinding> = Vec::new();
    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
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
            .then_with(|| a.reinvented.cmp(b.reinvented))
    });

    Ok(ComponentsAuditReport {
        findings,
        parse_errors: collect_parse_errors(&files),
    })
}

/// Classname token → catalog widget name. Curated subset of
/// `list_components.rs::KEYWORD_HINTS` (the keyword surface
/// `suggest_components` already exposes), keeping only shapes commonly
/// named via `class`. Generic tokens (`card`, `button`, `input`, `navbar`)
/// are intentionally absent because they appear constantly on layout
/// elements that aren't the catalog widget of that name.
const CLASS_TO_CATALOG: &[(&str, &str)] = &[
    ("modal", "dialog"),
    ("dialog", "dialog"),
    ("popover", "popover"),
    ("tooltip", "tooltip"),
    ("accordion", "accordion"),
    ("tabs", "tabs"),
    ("tab-strip", "tabs"),
    ("tablist", "tabs"),
    ("calendar", "calendar"),
    ("datepicker", "date_picker"),
    ("date-picker", "date_picker"),
    ("dropdown", "dropdown_menu"),
    ("dropdown-menu", "dropdown_menu"),
    ("pagination", "pagination"),
    ("toast", "toast"),
    ("snackbar", "toast"),
    ("sidebar", "sidebar"),
    ("drawer", "sheet"),
    ("avatar", "avatar"),
    ("badge", "badge"),
    ("progress", "progress"),
    ("progress-bar", "progress"),
];

fn scan_file(ast: &syn::File, file: &Path, out: &mut Vec<ComponentsAuditFinding>) {
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

        // Dedupe per (component, catalog widget) — three `<div class="modal">`
        // in one component surface one finding. First hit wins on line/class.
        let mut hits: BTreeMap<&'static str, (usize, String)> = BTreeMap::new();
        for body in &collector.rsx_bodies {
            scan_class_attrs(body.clone(), &mut hits);
        }
        for (catalog, (line, matched_class)) in hits {
            out.push(ComponentsAuditFinding {
                reinvented: catalog,
                component: component.clone(),
                file: file.to_path_buf(),
                line,
                hint: format!(
                    "`class: \"{matched_class}\"` looks like a hand-rolled {catalog} shape. \
                     Catalog ships `{catalog}` with theming, keyboard navigation, and a11y \
                     wiring already done. `dx components add {catalog}` to install, then call \
                     `describe_component` for the prop surface. `confidence: low` — class \
                     names are conventions, not contracts; verify the shape before swapping."
                ),
                matched_class,
                confidence: "low",
            });
        }
    }
}

/// Walk an rsx token stream looking for `class : "<literal>"` triples and
/// record the first hit per catalog widget. The detection is purely
/// structural: `Ident("class")` → `Punct(':')` → `Literal("...")`. Any
/// other shape (e.g. `class: if cond { ... } else { ... }`) is skipped at
/// this MVP — class expressions don't carry a stable literal we can match.
fn scan_class_attrs(
    ts: proc_macro2::TokenStream,
    hits: &mut BTreeMap<&'static str, (usize, String)>,
) {
    let trees: Vec<TokenTree> = ts.into_iter().collect();
    let mut i = 0;
    while i < trees.len() {
        if let TokenTree::Ident(id) = &trees[i]
            && id == "class"
            && let Some(TokenTree::Punct(p)) = trees.get(i + 1)
            && p.as_char() == ':'
            && let Some(TokenTree::Literal(lit)) = trees.get(i + 2)
        {
            let raw = lit.to_string();
            if let Some(unquoted) = strip_str_literal(&raw) {
                let line = lit.span().start().line;
                for token in unquoted.split_whitespace() {
                    let tok_lc = token.to_ascii_lowercase();
                    if let Some((_, catalog)) = CLASS_TO_CATALOG
                        .iter()
                        .find(|(needle, _)| *needle == tok_lc.as_str())
                        && !hits.contains_key(*catalog)
                    {
                        hits.insert(catalog, (line, unquoted.to_string()));
                    }
                }
            }
        }
        if let TokenTree::Group(g) = &trees[i] {
            scan_class_attrs(g.stream(), hits);
        }
        i += 1;
    }
}

/// Returns the inner contents of a `"..."` string literal token, or `None`
/// for non-string literals (numbers, raw strings, char literals — none of
/// which can be a rsx `class:` attribute value in valid Dioxus 0.7 code).
fn strip_str_literal(raw: &str) -> Option<&str> {
    raw.strip_prefix('"').and_then(|s| s.strip_suffix('"'))
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
    use std::collections::BTreeSet;
    use tempfile::tempdir;

    fn write_file(p: &Path, body: &str) {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, body).unwrap();
    }

    fn scan(crate_root: &Path) -> Vec<ComponentsAuditFinding> {
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
    fn flags_class_modal_as_dialog() {
        let dir = tempdir().unwrap();
        write_file(
            &dir.path().join("src/screens/login.rs"),
            r#"use dioxus::prelude::*;
#[component]
fn Login() -> Element {
    rsx! {
        div { class: "modal",
            "Are you sure?"
        }
    }
}
"#,
        );
        let findings = scan(dir.path());
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].reinvented, "dialog");
        assert_eq!(findings[0].component, "Login");
        assert_eq!(findings[0].confidence, "low");
        assert_eq!(findings[0].matched_class, "modal");
        assert!(findings[0].hint.contains("dx components add dialog"));
    }

    #[test]
    fn flags_class_tabs_as_tabs() {
        let dir = tempdir().unwrap();
        write_file(
            &dir.path().join("src/screens/dashboard.rs"),
            r#"use dioxus::prelude::*;
#[component]
fn Dashboard() -> Element {
    rsx! {
        div { class: "tabs",
            button { "One" }
            button { "Two" }
        }
    }
}
"#,
        );
        let findings = scan(dir.path());
        let tabs = findings
            .iter()
            .find(|f| f.reinvented == "tabs")
            .expect("expected tabs finding");
        assert_eq!(tabs.component, "Dashboard");
        assert_eq!(tabs.matched_class, "tabs");
    }

    #[test]
    fn flags_class_accordion_as_accordion() {
        let dir = tempdir().unwrap();
        write_file(
            &dir.path().join("src/faq.rs"),
            r#"use dioxus::prelude::*;
#[component]
fn Faq() -> Element {
    rsx! {
        section { class: "accordion-panel",
            "Question?"
        }
    }
}
"#,
        );
        // "accordion-panel" doesn't match "accordion" as a token (whitespace
        // split). Confirm we DON'T fire on bigram-only classes — that's the
        // intended conservative posture. The user can rename if they want
        // the hit.
        assert!(
            scan(dir.path()).is_empty(),
            "single hyphenated multi-word class should not match a bare keyword token"
        );

        // Now confirm a clean classname DOES hit.
        let dir2 = tempdir().unwrap();
        write_file(
            &dir2.path().join("src/faq.rs"),
            r#"use dioxus::prelude::*;
#[component]
fn Faq() -> Element {
    rsx! {
        section { class: "accordion",
            "Question?"
        }
    }
}
"#,
        );
        let findings = scan(dir2.path());
        let a = findings
            .iter()
            .find(|f| f.reinvented == "accordion")
            .expect("expected accordion finding");
        assert_eq!(a.component, "Faq");
    }

    #[test]
    fn flags_multiclass_string() {
        let dir = tempdir().unwrap();
        write_file(
            &dir.path().join("src/widgets.rs"),
            r#"use dioxus::prelude::*;
#[component]
fn Widget() -> Element {
    rsx! {
        div { class: "container modal large",
            "Hello"
        }
    }
}
"#,
        );
        let findings = scan(dir.path());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].reinvented, "dialog");
        // The full class string is preserved in `matched_class` so the
        // agent sees the surrounding context.
        assert_eq!(findings[0].matched_class, "container modal large");
    }

    #[test]
    fn dedupes_repeated_class_in_one_component() {
        let dir = tempdir().unwrap();
        write_file(
            &dir.path().join("src/widgets.rs"),
            r#"use dioxus::prelude::*;
#[component]
fn Widget() -> Element {
    rsx! {
        div { class: "modal", "one" }
        div { class: "modal", "two" }
        div { class: "modal", "three" }
    }
}
"#,
        );
        let findings = scan(dir.path());
        let modal_hits: Vec<&ComponentsAuditFinding> = findings
            .iter()
            .filter(|f| f.reinvented == "dialog")
            .collect();
        assert_eq!(
            modal_hits.len(),
            1,
            "three class=\"modal\" divs must dedupe to one finding: {findings:?}"
        );
    }

    #[test]
    fn skips_catalog_wrapper_files() {
        let dir = tempdir().unwrap();
        // `dialog` is a real catalog widget, so this path counts as a
        // wrapper — the lint must NOT fire even though the file uses
        // `class: "modal"`.
        write_file(
            &dir.path().join("src/components/dialog/component.rs"),
            r#"use dioxus::prelude::*;
#[component]
pub fn Dialog() -> Element {
    rsx! {
        div { class: "modal",
            "wrapper internals"
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

    #[test]
    fn ignores_unrelated_classnames() {
        let dir = tempdir().unwrap();
        write_file(
            &dir.path().join("src/widgets.rs"),
            r#"use dioxus::prelude::*;
#[component]
fn Widget() -> Element {
    rsx! {
        nav { class: "navbar", "n" }
        div { class: "card",
            div { class: "container",
                button { class: "primary", "Click" }
            }
        }
    }
}
"#,
        );
        assert!(
            scan(dir.path()).is_empty(),
            "classes like navbar/card/container/primary must not fire"
        );
    }

    #[test]
    fn ignores_pascal_case_catalog_components() {
        let dir = tempdir().unwrap();
        // Rendering the catalog widget itself (PascalCase) — the `class`
        // attribute on the OUTER `div` doesn't contain a catalog keyword,
        // so no fire. The `Dialog {}` element has no `class:` literal so
        // it's ignored regardless.
        write_file(
            &dir.path().join("src/screens/foo.rs"),
            r#"use dioxus::prelude::*;
#[component]
fn Foo() -> Element {
    rsx! {
        div { class: "wrapper",
            Dialog { open: true }
        }
    }
}
"#,
        );
        assert!(scan(dir.path()).is_empty());
    }

    /// The class→catalog table must only point at names that actually
    /// exist in the catalog. Catches typos (e.g. `combo_box` vs `combobox`,
    /// already a bug in `KEYWORD_HINTS`) and catalog drift at test time.
    #[test]
    fn catalog_table_entries_exist_in_catalog() {
        let catalog: BTreeSet<&str> = crate::tools::dsl::dx_component_names().collect();
        for (cls, name) in CLASS_TO_CATALOG {
            assert!(
                catalog.contains(name),
                "class→catalog mapping points at {name:?} (from class token {cls:?}) but no \
                 such widget exists in DX_COMPONENT_CATALOG_ENTRIES"
            );
        }
    }
}
