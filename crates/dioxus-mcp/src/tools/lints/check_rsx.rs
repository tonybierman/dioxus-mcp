use std::path::PathBuf;
use std::sync::Arc;

use proc_macro2::TokenTree;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::visit::Visit;

use crate::state::State;
use crate::tools::{ambiguous_attrs_for_element, resolve_in_project};

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct CheckRsxParams {
    /// Path to a single Rust file to scan (absolute, or relative to the project
    /// root). One of `file` or `files` must be provided.
    #[serde(default)]
    pub file: Option<String>,
    /// Batch form: multiple file paths. When set, each file is linted and the
    /// per-file breakdown lands in `per_file`; top-level `issues` is the flat
    /// merge across files, with each issue annotated with its `file`.
    #[serde(default)]
    pub files: Option<Vec<String>>,
    /// Absolute path to the Dioxus project root. Required when paths are
    /// relative and the server was not started in the target project directory.
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RsxIssue {
    pub line: usize,
    pub column: usize,
    pub message: String,
    /// Set in batch mode so callers can attribute a merged issue to its file.
    /// Omitted in single-file mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub struct CheckRsxFileReport {
    pub file: PathBuf,
    pub rsx_block_count: usize,
    pub issues: Vec<RsxIssue>,
}

#[derive(Debug, Serialize)]
pub struct CheckRsxReport {
    /// File scanned, in single-file mode only. In batch mode this is omitted
    /// — `per_file[i].file` carries the per-file paths, and surfacing a
    /// single "first file" at the top level was misleading callers
    /// (`lint_project` was pointing at the first `.rs` file alphabetically,
    /// often a router.rs with 0 rsx blocks, while every actual finding lived
    /// inside `per_file`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
    /// Sum of rsx! blocks across all scanned files.
    pub rsx_block_count: usize,
    /// Names of every lint that ran on the file(s). Lets a caller distinguish
    /// "no issues found" from "no checks ran" — an empty `issues` list with a
    /// non-empty `checks_run` is a real clean bill of health.
    pub checks_run: &'static [&'static str],
    /// Flat list of issues. In batch mode each carries its `file`.
    pub issues: Vec<RsxIssue>,
    /// Per-file breakdown. Empty in single-file mode.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub per_file: Vec<CheckRsxFileReport>,
}

/// Static catalogue of the checks `lint_rsx_tokens` performs. Keep aligned
/// with `walk_lint` — adding a new lint there should add an entry here.
pub const CHECKS_RUN: &[&str] = &[
    "missing_key_in_for_loop",
    "ambiguous_attribute_e0034",
    "empty_event_handler_closure",
];

pub async fn check_rsx(state: &Arc<State>, p: CheckRsxParams) -> Result<CheckRsxReport, String> {
    let mut requested: Vec<String> = Vec::new();
    if let Some(f) = p.file.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        requested.push(f.to_owned());
    }
    if let Some(fs) = &p.files {
        for f in fs {
            let f = f.trim();
            if !f.is_empty() {
                requested.push(f.to_owned());
            }
        }
    }
    if requested.is_empty() {
        return Err("check_rsx: pass `file` (single path) or `files` (list of paths)".into());
    }

    let batch_mode = requested.len() > 1 || p.files.is_some();
    let mut per_file: Vec<CheckRsxFileReport> = Vec::with_capacity(requested.len());
    for spec in &requested {
        let path = resolve_in_project(state, spec, p.project_root.as_deref()).await;
        let report = lint_single_file(&path)?;
        per_file.push(report);
    }

    let total_blocks: usize = per_file.iter().map(|r| r.rsx_block_count).sum();
    let issues: Vec<RsxIssue> = if batch_mode {
        per_file
            .iter()
            .flat_map(|r| {
                r.issues.iter().map(|i| RsxIssue {
                    line: i.line,
                    column: i.column,
                    message: i.message.clone(),
                    file: Some(r.file.clone()),
                })
            })
            .collect()
    } else {
        per_file[0]
            .issues
            .iter()
            .map(|i| RsxIssue {
                line: i.line,
                column: i.column,
                message: i.message.clone(),
                file: None,
            })
            .collect()
    };

    Ok(CheckRsxReport {
        // Single-file mode: surface the scanned file. Batch mode: omit, so
        // callers (notably `lint_project`) don't see a misleading "first
        // file" pointer that bears no relationship to where the findings are.
        file: if batch_mode {
            None
        } else {
            Some(per_file[0].file.clone())
        },
        rsx_block_count: total_blocks,
        checks_run: CHECKS_RUN,
        issues,
        per_file: if batch_mode { per_file } else { Vec::new() },
    })
}

fn lint_single_file(path: &std::path::Path) -> Result<CheckRsxFileReport, String> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let file = syn::parse_file(&src).map_err(|e| {
        let s = e.span().start();
        format!(
            "rust parse error in {} at line {} col {}: {e}",
            path.display(),
            s.line,
            s.column
        )
    })?;

    struct Visitor<'a> {
        src: &'a str,
        rsx_blocks: usize,
        issues: Vec<RsxIssue>,
    }
    impl<'a, 'ast> Visit<'ast> for Visitor<'a> {
        fn visit_macro(&mut self, m: &'ast syn::Macro) {
            let is_rsx = m
                .path
                .segments
                .last()
                .map(|s| s.ident == "rsx")
                .unwrap_or(false);
            if is_rsx {
                self.rsx_blocks += 1;
                lint_rsx_tokens(m, self.src, &mut self.issues);
            }
            syn::visit::visit_macro(self, m);
        }
    }

    let mut v = Visitor {
        src: &src,
        rsx_blocks: 0,
        issues: Vec::new(),
    };
    v.visit_file(&file);

    Ok(CheckRsxFileReport {
        file: path.to_path_buf(),
        rsx_block_count: v.rsx_blocks,
        issues: v.issues,
    })
}

fn lint_rsx_tokens(m: &syn::Macro, _src: &str, issues: &mut Vec<RsxIssue>) {
    // Heuristic linting on the raw token stream. Real parsing requires the
    // dioxus-rsx crate (which is unstable across versions); this catches the
    // most common 0.7 mistakes without bringing in a full parser.
    let tokens: Vec<TokenTree> = m.tokens.clone().into_iter().collect();
    walk_lint(&tokens, false, true, None, issues);
}

/// `parent_is_component` is true when the surrounding brace group is the body
/// of a component invocation (e.g. `MyComp { ... }`). It gates lints that only
/// make sense for HTML element attributes — most notably the `onXxx:` empty-
/// closure check, since a component prop named `onclick:` may legitimately be
/// an `EventHandler<()>` / `Callback<()>` that takes no arguments.
///
/// `in_rsx_node_position` is true when the token stream sits at an rsx node
/// position — the top level of a rsx! macro, or the body of an element /
/// component / `for` / `if` / match-arm. It is false inside attribute values
/// and inside closure bodies (e.g. `onclick: move |evt| { ... }`), where a
/// Rust `for` loop has no rsx semantics and must not be lint-checked.
///
/// `parent_element_name` is the lowercase HTML element ident when the
/// surrounding brace group is an element body (e.g. `Some("input")` for
/// `input { ... }`). Used to catch ambiguous-attribute writes (E0034) where
/// the element-specific extension trait and `GlobalAttributesExtension` both
/// define the same setter name.
fn walk_lint(
    tokens: &[TokenTree],
    parent_is_component: bool,
    in_rsx_node_position: bool,
    parent_element_name: Option<&str>,
    issues: &mut Vec<RsxIssue>,
) {
    // 1) `for ... { ... }` without a `key:` attribute somewhere in the body.
    //    Only fires at rsx-node position; a `for` inside an event-handler
    //    closure body is a plain Rust loop, not an rsx loop.
    if in_rsx_node_position {
        let mut i = 0;
        while i < tokens.len() {
            if let TokenTree::Ident(id) = &tokens[i]
                && id == "for"
                && let Some(body_idx) = find_for_body(tokens, i)
            {
                let body_group = match &tokens[body_idx] {
                    TokenTree::Group(g) => g,
                    _ => unreachable!("find_for_body returned a non-group index"),
                };
                let body_tokens: Vec<TokenTree> = body_group.stream().clone().into_iter().collect();
                if !slice_has_key_attr(&body_tokens) {
                    let s = id.span().start();
                    issues.push(RsxIssue {
                                line: s.line,
                                column: s.column,
                                message: "loop in rsx! is missing a `key: ...` attribute on its child element — Dioxus needs keys for stable diffing".into(),
                                file: None,
                            });
                }
            }
            i += 1;
        }
    }

    // 1b) Attribute writes that hit E0034 because the element-specific
    //     extension trait AND `GlobalAttributesExtension` both define the same
    //     setter. Fires only inside an HTML element body whose name has a
    //     non-empty ambiguity list. The fix is to use the explicit attribute-
    //     literal syntax: `"autofocus": "true"`.
    if let Some(elem) = parent_element_name {
        let bad = ambiguous_attrs_for_element(elem);
        if !bad.is_empty() {
            let mut j = 0;
            while j + 1 < tokens.len() {
                if let (TokenTree::Ident(id), TokenTree::Punct(p)) = (&tokens[j], &tokens[j + 1])
                    && p.as_char() == ':'
                {
                    let name = id.to_string();
                    if bad.contains(&name.as_str()) {
                        let s = id.span().start();
                        issues.push(RsxIssue {
                            line: s.line,
                            column: s.column,
                            message: format!(
                                "`{name}` on `{elem}` is ambiguous: both `GlobalAttributesExtension` and `{elem}`'s extension trait define a setter named `{name}` (E0034). Use the explicit attribute syntax instead: `\"{name}\": \"...\"` (or `\"{name}\": true` for boolean attrs)."
                            ),
                            file: None,
                        });
                    }
                }
                j += 1;
            }
        }
    }

    // 2) `onXxx: |..|` or `onXxx: move |..|` with empty closure params.
    //    Only meaningful inside an HTML element body — on component invocations
    //    the prop type may legitimately be a zero-arg callback.
    if !parent_is_component {
        let mut j = 0;
        while j + 2 < tokens.len() {
            let (a, b) = (&tokens[j], &tokens[j + 1]);
            if let (TokenTree::Ident(id), TokenTree::Punct(p)) = (a, b) {
                let name = id.to_string();
                if name.starts_with("on") && name.len() > 2 && p.as_char() == ':' {
                    let mut k = j + 2;
                    if matches!(tokens.get(k), Some(TokenTree::Ident(i)) if i == "move") {
                        k += 1;
                    }
                    let starts_pipe =
                        matches!(tokens.get(k), Some(TokenTree::Punct(q)) if q.as_char() == '|');
                    let ends_pipe = matches!(tokens.get(k + 1), Some(TokenTree::Punct(q)) if q.as_char() == '|');
                    if starts_pipe && ends_pipe {
                        let s = id.span().start();
                        issues.push(RsxIssue {
                            line: s.line,
                            column: s.column,
                            message: format!(
                                "`{name}` handler closure takes no parameters; in Dioxus it should accept an Event, e.g. `{name}: move |evt: Event<MouseData>| {{ ... }}`"
                            ),
                            file: None,
                        });
                    }
                }
            }
            j += 1;
        }
    }

    // Recurse into groups so element/component bodies are scanned. Track the
    // immediately preceding ident so the recursed body knows whether it sits
    // inside a component (uppercase) or an HTML element (lowercase). Also
    // detect closure-arg pipes (`|...|`) so the brace group that follows is
    // recognised as a closure body (Rust expression context), not an rsx body.
    let mut last_ident: Option<&proc_macro2::Ident> = None;
    let mut seen_open_pipe = false;
    let mut after_closure_args = false;
    for tt in tokens {
        match tt {
            TokenTree::Ident(id) => {
                last_ident = Some(id);
                after_closure_args = false;
            }
            TokenTree::Punct(p) if p.as_char() == '|' => {
                if seen_open_pipe {
                    after_closure_args = true;
                    seen_open_pipe = false;
                } else {
                    seen_open_pipe = true;
                }
                last_ident = None;
            }
            TokenTree::Group(g) => {
                let is_brace = g.delimiter() == proc_macro2::Delimiter::Brace;
                let is_closure_body = after_closure_args && is_brace;
                // An rsx node body is a brace group preceded by an element /
                // component ident. Closure bodies (preceded by `|...|`) and
                // non-brace groups (parens, brackets) are Rust expression
                // contexts and must not run the for-loop key check.
                let group_in_rsx_position = is_brace && last_ident.is_some() && !is_closure_body;
                let is_component = last_ident
                    .map(|id| starts_uppercase(&id.to_string()))
                    .unwrap_or(false);
                // The element name flows down only when entering an HTML
                // element's body (lowercase ident + brace group at rsx
                // position). Component bodies and non-rsx groups reset it.
                let child_element_name: Option<String> = if group_in_rsx_position && !is_component {
                    last_ident.map(|id| id.to_string())
                } else {
                    None
                };
                let inner: Vec<TokenTree> = g.stream().clone().into_iter().collect();
                walk_lint(
                    &inner,
                    is_component,
                    group_in_rsx_position,
                    child_element_name.as_deref(),
                    issues,
                );
                last_ident = None;
                seen_open_pipe = false;
                after_closure_args = false;
            }
            _ => {
                last_ident = None;
                // A `,` ends an attribute value / arg-list element; any other
                // punct (e.g. `:`, `.`, `=`) also closes the current closure-
                // detection scope. Reset state so a later `|` starts fresh.
                seen_open_pipe = false;
                after_closure_args = false;
            }
        }
    }
}

fn starts_uppercase(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

/// Locate the `{ ... }` body group of a `for` loop. Skips groups with other
/// delimiters (e.g. the parens from `for x in xs.iter() { ... }`) so the
/// key-attribute scan inspects the loop's actual child elements.
fn find_for_body(tokens: &[TokenTree], start_after: usize) -> Option<usize> {
    for (k, tt) in tokens.iter().enumerate().skip(start_after + 1) {
        if let TokenTree::Group(g) = tt
            && g.delimiter() == proc_macro2::Delimiter::Brace
        {
            return Some(k);
        }
    }
    None
}

fn slice_has_key_attr(slice: &[TokenTree]) -> bool {
    for w in slice.windows(2) {
        if let (TokenTree::Ident(id), TokenTree::Punct(p)) = (&w[0], &w[1])
            && id == "key"
            && p.as_char() == ':'
        {
            return true;
        }
    }
    for tt in slice {
        if let TokenTree::Group(g) = tt {
            let inner: Vec<TokenTree> = g.stream().clone().into_iter().collect();
            if slice_has_key_attr(&inner) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::lint_single_file;

    fn lint(src: &str) -> Vec<String> {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("t.rs");
        std::fs::write(&path, src).unwrap();
        lint_single_file(&path)
            .unwrap()
            .issues
            .into_iter()
            .map(|i| i.message)
            .collect()
    }

    #[test]
    fn detects_missing_key_on_loop_child() {
        let issues = lint(
            r#"use dioxus::prelude::*;
fn t() {
    let _ = rsx! {
        ul {
            for x in vec![1, 2, 3] {
                li { "{x}" }
            }
        }
    };
}
"#,
        );
        assert!(
            issues.iter().any(|m| m.contains("missing a `key:")),
            "expected key warning, got {issues:?}"
        );
    }

    #[test]
    fn no_warning_when_key_on_element_with_other_attrs() {
        // Element body has `key:` mixed with other attributes — the lint
        // recurses into the body group and finds it.
        let issues = lint(
            r#"use dioxus::prelude::*;
fn t() {
    let _ = rsx! {
        table {
            for p in vec![1] {
                tr { key: "{p}", class: "row",
                    td { "{p}" }
                }
            }
        }
    };
}
"#,
        );
        assert!(
            issues.iter().all(|m| !m.contains("missing a `key:")),
            "did not expect a key warning, got {issues:?}"
        );
    }

    #[test]
    fn no_warning_when_for_iterates_via_method_call() {
        // Regression: `for x in xs.iter() { ... }` used to make the lint
        // target the `()` of `.iter()` instead of the body, mis-firing
        // even when the body had a proper `key:` attribute.
        let issues = lint(
            r#"use dioxus::prelude::*;
fn t() {
    let xs: Vec<i32> = vec![];
    let _ = rsx! {
        ul {
            for x in xs.iter() {
                li { key: "{x}", "{x}" }
            }
        }
    };
}
"#,
        );
        assert!(
            issues.iter().all(|m| !m.contains("missing a `key:")),
            "did not expect a key warning, got {issues:?}"
        );
    }

    #[test]
    fn no_warning_when_key_on_component_invocation() {
        // Component invocation `Row { key: "{p.id}", ... }` is also a brace
        // group; the recursion into it finds the key.
        let issues = lint(
            r#"use dioxus::prelude::*;
#[derive(Clone, PartialEq, Props)]
struct RowProps { id: i32 }
fn Row(props: RowProps) -> Element { rsx! {} }
fn t() {
    let _ = rsx! {
        for p in vec![1, 2] {
            Row { key: "{p}", id: p }
        }
    };
}
"#,
        );
        assert!(
            issues.iter().all(|m| !m.contains("missing a `key:")),
            "did not expect a key warning, got {issues:?}"
        );
    }

    #[test]
    fn flags_empty_closure_on_element_event_handler() {
        // HTML element `onclick: || { ... }` is wrong — handlers must accept
        // an Event<MouseData>.
        let issues = lint(
            r#"use dioxus::prelude::*;
fn t() {
    let _ = rsx! {
        button { onclick: |_| {}, "noop" }
        button { onclick: || {}, "bad" }
    };
}
"#,
        );
        assert!(
            issues.iter().any(|m| m.contains("takes no parameters")),
            "expected onclick warning, got {issues:?}"
        );
    }

    #[test]
    fn no_warning_for_zero_arg_closure_on_component_prop() {
        // Component props named `onXxx:` can legitimately be `EventHandler<()>`
        // or `Callback<()>` that take no args. Used to false-positive.
        let issues = lint(
            r#"use dioxus::prelude::*;
#[derive(Clone, PartialEq, Props)]
struct DialogProps { onclose: EventHandler<()> }
#[component]
fn Dialog(props: DialogProps) -> Element { rsx! {} }
fn t() {
    let _ = rsx! {
        Dialog { onclose: move || {} }
    };
}
"#,
        );
        assert!(
            issues.iter().all(|m| !m.contains("takes no parameters")),
            "did not expect onclose warning on a component prop, got {issues:?}"
        );
    }

    #[test]
    fn no_warning_when_key_on_component_with_shorthand_prop() {
        // The exact shape from TODO.md item #1: a component invocation that
        // uses `key:` plus a shorthand prop (`x` resolving to the prop named
        // `x`). The shorthand makes the body `{ key: "{x.id}", x }` — the
        // trailing bare ident must not confuse the key search.
        let issues = lint(
            r#"use dioxus::prelude::*;
#[derive(Clone, PartialEq, Props)]
struct RowProps { x: i32 }
fn Row(props: RowProps) -> Element { rsx! {} }
fn t() {
    let xs: Vec<i32> = vec![];
    let _ = rsx! {
        for x in xs {
            Row { key: "{x}", x }
        }
    };
}
"#,
        );
        assert!(
            issues.iter().all(|m| !m.contains("missing a `key:")),
            "did not expect a key warning, got {issues:?}"
        );
    }

    #[test]
    fn no_warning_when_key_uses_field_access_interpolation() {
        // TODO.md item #3: `key: "{item.id}"` was reportedly flagged.
        // String literal carries the interpolation; the lint should see
        // `key:` and stop, regardless of the literal's contents.
        let issues = lint(
            r#"use dioxus::prelude::*;
#[derive(Clone, PartialEq, Props)]
struct Item { id: i32 }
fn t() {
    let items: Vec<Item> = vec![];
    let _ = rsx! {
        ul {
            for item in items.iter() {
                li { key: "{item.id}", "{item.id}" }
            }
        }
    };
}
"#,
        );
        assert!(
            issues.iter().all(|m| !m.contains("missing a `key:")),
            "did not expect a key warning, got {issues:?}"
        );
    }

    #[test]
    fn no_warning_when_key_wrapped_in_match_or_if() {
        // Real-world shape: the loop body wraps elements in `match` / `if` arms;
        // the key still has to be detected by walking into the arm bodies.
        let issues = lint(
            r#"use dioxus::prelude::*;
fn t() {
    let items: Vec<(i32, bool)> = vec![];
    let _ = rsx! {
        ul {
            for (id, show) in items {
                if show {
                    li { key: "{id}", "{id}" }
                }
            }
        }
    };
}
"#,
        );
        assert!(
            issues.iter().all(|m| !m.contains("missing a `key:")),
            "did not expect a key warning, got {issues:?}"
        );
    }

    #[test]
    fn no_warning_when_key_on_component_with_tuple_destructure() {
        // Closer to the shape users actually write: `for (id, label) in items`
        // binding via destructure, then a component invocation with multiple
        // props including `key:`. Guards against regressions where the tuple
        // pattern's parens get mistaken for the loop body.
        let issues = lint(
            r#"use dioxus::prelude::*;
#[derive(Clone, PartialEq, Props)]
struct RowProps { id: i32, label: String }
fn Row(props: RowProps) -> Element { rsx! {} }
fn t() {
    let items: Vec<(i32, String)> = vec![];
    let _ = rsx! {
        div {
            for (id, label) in items {
                Row { key: "{id}", id: id, label: label }
            }
        }
    };
}
"#,
        );
        assert!(
            issues.iter().all(|m| !m.contains("missing a `key:")),
            "did not expect a key warning, got {issues:?}"
        );
    }

    #[test]
    fn no_warning_for_rust_for_loop_inside_event_handler_closure() {
        // Regression: a `for` inside an `onclick: move |evt| { ... }` body
        // is a plain Rust loop, not an rsx loop. The lint used to recurse
        // into the closure body and flag it as missing a `key:` attribute.
        let issues = lint(
            r#"use dioxus::prelude::*;
fn t() {
    let _ = rsx! {
        button {
            onclick: move |_evt| {
                let items = vec![1, 2, 3];
                for x in items {
                    let _ = x;
                }
            },
            "click"
        }
    };
}
"#,
        );
        assert!(
            issues.iter().all(|m| !m.contains("missing a `key:")),
            "did not expect a key warning on a Rust for-loop inside a closure, got {issues:?}"
        );
    }

    #[test]
    fn still_flags_missing_key_in_actual_rsx_loop() {
        // Sanity: the rsx-position check still fires on a real missing key,
        // even when there's also a closure handler in the same element.
        let issues = lint(
            r#"use dioxus::prelude::*;
fn t() {
    let items: Vec<i32> = vec![];
    let _ = rsx! {
        ul {
            onclick: move |_evt| { let _ = 1; },
            for x in items {
                li { "{x}" }
            }
        }
    };
}
"#,
        );
        assert!(
            issues.iter().any(|m| m.contains("missing a `key:")),
            "expected key warning on the rsx for-loop, got {issues:?}"
        );
    }

    #[test]
    fn flags_ambiguous_autofocus_on_input() {
        // E0034: both `GlobalAttributesExtension` and `InputExtension` define
        // `autofocus`. The agent that hit this set `autofocus: true` directly.
        let issues = lint(
            r#"use dioxus::prelude::*;
fn t() {
    let _ = rsx! {
        input { autofocus: true, value: "" }
    };
}
"#,
        );
        assert!(
            issues
                .iter()
                .any(|m| m.contains("autofocus") && m.contains("E0034")),
            "expected E0034 ambiguity warning on autofocus, got {issues:?}"
        );
    }

    #[test]
    fn flags_ambiguous_autofocus_on_button_textarea_select() {
        // Same lint must fire on the other form elements that define autofocus.
        for elem in ["button", "textarea", "select"] {
            let src = format!(
                r#"use dioxus::prelude::*;
fn t() {{
    let _ = rsx! {{
        {elem} {{ autofocus: true }}
    }};
}}
"#
            );
            let issues = lint(&src);
            assert!(
                issues
                    .iter()
                    .any(|m| m.contains("autofocus") && m.contains("E0034")),
                "expected E0034 warning on {elem}.autofocus, got {issues:?}"
            );
        }
    }

    #[test]
    fn does_not_flag_autofocus_on_non_form_element() {
        // `div` doesn't have an InputExtension-style trait with `autofocus`,
        // so it isn't ambiguous (the lint must not fire).
        let issues = lint(
            r#"use dioxus::prelude::*;
fn t() {
    let _ = rsx! {
        div { autofocus: true, "x" }
    };
}
"#,
        );
        assert!(
            issues.iter().all(|m| !m.contains("E0034")),
            "did not expect ambiguity warning on div.autofocus, got {issues:?}"
        );
    }

    #[test]
    fn does_not_flag_autofocus_when_using_explicit_attribute_syntax() {
        // The recommended fix — `"autofocus": "true"` (string-key form) — must
        // not trigger the lint, because the literal key isn't a method call.
        let issues = lint(
            r#"use dioxus::prelude::*;
fn t() {
    let _ = rsx! {
        input { "autofocus": "true", value: "" }
    };
}
"#,
        );
        assert!(
            issues.iter().all(|m| !m.contains("E0034")),
            "did not expect ambiguity warning on string-key autofocus, got {issues:?}"
        );
    }

    #[test]
    fn does_not_flag_autofocus_on_component_invocation() {
        // A component named `Input` with an `autofocus: bool` prop is not
        // ambiguous — the lint must be element-scoped (lowercase ident).
        let issues = lint(
            r#"use dioxus::prelude::*;
#[derive(Clone, PartialEq, Props)]
struct InputProps { autofocus: bool }
fn Input(props: InputProps) -> Element { rsx! {} }
fn t() {
    let _ = rsx! {
        Input { autofocus: true }
    };
}
"#,
        );
        assert!(
            issues.iter().all(|m| !m.contains("E0034")),
            "did not expect ambiguity warning on a component invocation, got {issues:?}"
        );
    }
}
