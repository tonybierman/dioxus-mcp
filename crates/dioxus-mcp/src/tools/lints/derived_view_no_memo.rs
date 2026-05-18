//! `derived_view_no_memo`: flag a pure derivation fn — one that takes a
//! `&[T]` slice and returns an owned `Vec<T>` — when it's invoked from
//! inside an `rsx!` body without being wrapped in `use_memo(…)`. Each
//! render reruns the filter / sort / clone, even when neither the source
//! signal nor the selector changed.
//!
//! Canonical iter03 shape: `column_cards(&cards.read(), col_id)` called
//! three times per render from `BoardBody`'s `for` loop. The fn filters
//! and sorts; without `use_memo` every keystroke or unrelated signal
//! write reclones the entire visible board.
//!
//! Detection:
//!   1. Walk every `.rs` file, collect free fns whose signature looks
//!      `fn name(&[T], …) -> Vec<T>` (slice-in, owned-Vec-out — the
//!      canonical "derived view" shape).
//!   2. Walk every `#[component]` fn's `rsx!` blocks. For each call
//!      `name(…)` whose name is in the derivation map, flag it unless
//!      the immediate parent context is `use_memo(|| name(…))`.
//!
//! Severity: `warning`. The shape compiles and runs correctly, but the
//! per-render recompute is the wrong default for any list view bigger
//! than ~50 items. Fix is mechanical: wrap each call in `use_memo`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use proc_macro2::TokenTree;
use quote::ToTokens;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::visit::Visit;

use crate::state::State;
use crate::tools::ast::{ParseError, collect_parse_errors, walk_rs_files};
use crate::tools::scaffold::crate_root;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct DerivedViewNoMemoParams {
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DerivedViewFinding {
    pub code: &'static str,
    pub severity: &'static str,
    pub file: PathBuf,
    pub line: usize,
    /// Component whose rsx! body contains the call.
    pub component: String,
    /// The derivation fn name (`column_cards` etc.).
    pub callee: String,
    /// How many call-sites of this callee appear inside the same rsx!
    /// body. iter03's BoardBody hits 3 (one per column). Surfaced so the
    /// reviewer sees the per-render multiplier at a glance.
    pub calls_in_rsx_block: usize,
    pub message: String,
    pub fix: String,
}

#[derive(Debug, Serialize)]
pub struct DerivedViewReport {
    pub findings: Vec<DerivedViewFinding>,
    pub parse_errors: Vec<ParseError>,
}

pub async fn derived_view_no_memo(
    state: &Arc<State>,
    p: DerivedViewNoMemoParams,
) -> Result<DerivedViewReport, String> {
    let root = crate_root(state, p.project_root.as_deref()).await?;
    let src_root = root.join("src");
    let files = walk_rs_files(&src_root);

    // Phase 1: every free fn shaped `fn name(&[T], …) -> Vec<U>` in the
    // crate. Cross-file by ident match — generators put derivations
    // alongside the component that uses them, but not always.
    let derivations: HashSet<String> = collect_derivations(&files);
    if derivations.is_empty() {
        return Ok(DerivedViewReport {
            findings: Vec::new(),
            parse_errors: collect_parse_errors(&files),
        });
    }

    // Phase 2: walk every component fn, scan its rsx! bodies for calls
    // into the derivation set.
    let mut findings: Vec<DerivedViewFinding> = Vec::new();
    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
        for item in &ast.items {
            let syn::Item::Fn(f) = item else { continue };
            if !is_component_fn(&f.attrs) {
                continue;
            }
            let comp = f.sig.ident.to_string();
            let mut rsx = RsxCollector::default();
            rsx.visit_block(&f.block);
            for body in &rsx.bodies {
                let calls = find_calls_outside_use_memo(body, &derivations);
                // Tally per-callee count so we can surface the
                // per-render multiplier without double-reporting.
                let mut by_callee: HashMap<String, Vec<usize>> = HashMap::new();
                for c in &calls {
                    by_callee.entry(c.callee.clone()).or_default().push(c.line);
                }
                for (callee, lines) in by_callee {
                    let n = lines.len();
                    let first_line = *lines.iter().min().unwrap_or(&0);
                    findings.push(DerivedViewFinding {
                        code: "derived_view_no_memo",
                        severity: "warning",
                        file: sf.path.clone(),
                        line: first_line,
                        component: comp.clone(),
                        callee: callee.clone(),
                        calls_in_rsx_block: n,
                        message: format!(
                            "`{callee}(…)` returns an owned `Vec<…>` derived from a slice and \
                             is called {n} time(s) directly in `{comp}`'s rsx! body — each \
                             render reruns the filter/sort/clone. Wrap each call in \
                             `use_memo(|| {callee}(…))` so the derivation only reruns when \
                             its inputs actually change.",
                        ),
                        fix: format!(
                            "Replace `{callee}(args)` with `use_memo(move || {callee}(args))()`. \
                             If the source is a `Signal<T>`, the memo's reactive read tracks the \
                             signal automatically; the closure reruns only when the signal value \
                             changes. For props that don't carry a signal, lift the source into \
                             a `use_signal` or `use_memo` in the parent."
                        ),
                    });
                }
            }
        }
    }

    findings.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.line.cmp(&b.line))
            .then(a.callee.cmp(&b.callee))
    });

    Ok(DerivedViewReport {
        findings,
        parse_errors: collect_parse_errors(&files),
    })
}

/// Collect free-fn names whose signature is `fn(&[T], …) -> Vec<U>` —
/// the derived-view shape. We don't require `T == U`; iter03's
/// `column_cards(&[Card], &str) -> Vec<Card>` and `card_owners(&[Card])
/// -> Vec<String>` both qualify.
fn collect_derivations(files: &[crate::tools::ast::ScannedFile]) -> HashSet<String> {
    let mut out = HashSet::new();
    for sf in files {
        let Ok(ast) = &sf.ast else { continue };
        for item in &ast.items {
            let syn::Item::Fn(f) = item else { continue };
            if f.sig.asyncness.is_some() {
                continue;
            }
            let returns_vec = match &f.sig.output {
                syn::ReturnType::Default => false,
                syn::ReturnType::Type(_, ty) => is_vec_type(ty),
            };
            if !returns_vec {
                continue;
            }
            let Some(first) = f.sig.inputs.first() else {
                continue;
            };
            let syn::FnArg::Typed(pt) = first else {
                continue;
            };
            if !is_slice_ref_type(&pt.ty) {
                continue;
            }
            out.insert(f.sig.ident.to_string());
        }
    }
    out
}

fn is_vec_type(ty: &syn::Type) -> bool {
    let s = ty.to_token_stream().to_string().replace(' ', "");
    s.starts_with("Vec<") || s.starts_with("std::vec::Vec<") || s.starts_with("alloc::vec::Vec<")
}

fn is_slice_ref_type(ty: &syn::Type) -> bool {
    // We want `&[T]` or `&mut [T]`; a `Vec<T>` taken by value wouldn't
    // create a per-render reclone worth flagging (the caller owns it).
    let syn::Type::Reference(r) = ty else {
        return false;
    };
    matches!(*r.elem, syn::Type::Slice(_))
}

#[derive(Default)]
struct RsxCollector {
    bodies: Vec<proc_macro2::TokenStream>,
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
            self.bodies.push(m.tokens.clone());
        }
        syn::visit::visit_macro(self, m);
    }
}

struct Call {
    callee: String,
    line: usize,
}

/// Walk an rsx! token tree and yield every call `<ident>(…)` whose
/// callee is in `derivations`, EXCEPT those whose direct enclosing
/// context is `use_memo(|| …)` (or `use_memo(move || …)`). We don't try
/// to be clever about nested layers — if the call appears inside an
/// already-memoised closure, the closure body's tokens are processed
/// with `inside_memo == true`.
fn find_calls_outside_use_memo(
    ts: &proc_macro2::TokenStream,
    derivations: &HashSet<String>,
) -> Vec<Call> {
    let mut out = Vec::new();
    let tokens: Vec<TokenTree> = ts.clone().into_iter().collect();
    walk(&tokens, derivations, /*inside_memo=*/ false, &mut out);
    out
}

fn walk(
    tokens: &[TokenTree],
    derivations: &HashSet<String>,
    inside_memo: bool,
    out: &mut Vec<Call>,
) {
    let mut i = 0;
    while i < tokens.len() {
        if let TokenTree::Ident(id) = &tokens[i] {
            let name = id.to_string();
            // `use_memo(...)` — anything inside is exempt. We don't try
            // to look at the closure boundary; the entire arg group is
            // treated as "inside a memo."
            if name == "use_memo"
                && let Some(TokenTree::Group(g)) = tokens.get(i + 1)
            {
                let inner: Vec<TokenTree> = g.stream().into_iter().collect();
                walk(&inner, derivations, /*inside_memo=*/ true, out);
                i += 2;
                continue;
            }
            // Plain call `<ident>(…)` — flag if the ident is in our
            // derivation set and we're not already nested in a memo.
            if !inside_memo
                && derivations.contains(&name)
                && let Some(TokenTree::Group(g)) = tokens.get(i + 1)
                && g.delimiter() == proc_macro2::Delimiter::Parenthesis
            {
                out.push(Call {
                    callee: name,
                    line: id.span().start().line,
                });
                // Recurse into the arg group anyway — a derivation
                // call could nest another derivation in its args.
                let inner: Vec<TokenTree> = g.stream().into_iter().collect();
                walk(&inner, derivations, inside_memo, out);
                i += 2;
                continue;
            }
        }
        if let TokenTree::Group(g) = &tokens[i] {
            let inner: Vec<TokenTree> = g.stream().into_iter().collect();
            walk(&inner, derivations, inside_memo, out);
        }
        i += 1;
    }
}

fn is_component_fn(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path()
            .segments
            .last()
            .map(|s| s.ident == "component")
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn run(content: &str) -> DerivedViewReport {
        let dir = TempDir::new().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("lib.rs"), content).unwrap();
        let files = walk_rs_files(&src_dir);
        let derivations = collect_derivations(&files);
        let mut findings: Vec<DerivedViewFinding> = Vec::new();
        for sf in &files {
            let Ok(ast) = &sf.ast else { continue };
            for item in &ast.items {
                let syn::Item::Fn(f) = item else { continue };
                if !is_component_fn(&f.attrs) {
                    continue;
                }
                let comp = f.sig.ident.to_string();
                let mut rsx = RsxCollector::default();
                rsx.visit_block(&f.block);
                for body in &rsx.bodies {
                    let calls = find_calls_outside_use_memo(body, &derivations);
                    let mut by: HashMap<String, Vec<usize>> = HashMap::new();
                    for c in &calls {
                        by.entry(c.callee.clone()).or_default().push(c.line);
                    }
                    for (callee, lines) in by {
                        findings.push(DerivedViewFinding {
                            code: "derived_view_no_memo",
                            severity: "warning",
                            file: sf.path.clone(),
                            line: *lines.iter().min().unwrap_or(&0),
                            component: comp.clone(),
                            callee,
                            calls_in_rsx_block: lines.len(),
                            message: String::new(),
                            fix: String::new(),
                        });
                    }
                }
            }
        }
        DerivedViewReport {
            findings,
            parse_errors: collect_parse_errors(&files),
        }
    }

    /// iter03's exact shape — three `column_cards(&cards.read(), col_id)`
    /// calls inside `BoardBody`'s rsx loop. Must fire once with
    /// `calls_in_rsx_block: 3`.
    #[test]
    fn flags_iter03_column_cards_shape() {
        let r = run(r#"
fn column_cards(all: &[Card], col: &str) -> Vec<Card> {
    all.iter().filter(|c| c.column == col).cloned().collect()
}
#[component]
fn BoardBody() -> Element {
    let cards = use_signal(Vec::<Card>::new);
    rsx! {
        Column { cards: column_cards(&cards.read(), "todo") }
        Column { cards: column_cards(&cards.read(), "doing") }
        Column { cards: column_cards(&cards.read(), "done") }
    }
}
"#);
        assert_eq!(r.findings.len(), 1, "{r:?}");
        assert_eq!(r.findings[0].callee, "column_cards");
        assert_eq!(r.findings[0].calls_in_rsx_block, 3);
    }

    /// Wrap-in-use_memo silences the finding — the lint must not flag
    /// already-correct call sites.
    #[test]
    fn silent_when_wrapped_in_use_memo() {
        let r = run(r#"
fn column_cards(all: &[Card], col: &str) -> Vec<Card> {
    all.iter().filter(|c| c.column == col).cloned().collect()
}
#[component]
fn BoardBody() -> Element {
    let cards = use_signal(Vec::<Card>::new);
    rsx! {
        Column { cards: use_memo(move || column_cards(&cards.read(), "todo"))() }
    }
}
"#);
        assert!(r.findings.is_empty(), "{r:?}");
    }

    /// A fn that returns `Vec<T>` but doesn't take `&[T]` (e.g. takes
    /// `Vec<T>` by value) isn't the derived-view shape — the caller
    /// already paid the clone, no per-render multiplier.
    #[test]
    fn silent_when_arg_is_not_slice_ref() {
        let r = run(r#"
fn column_cards(all: Vec<Card>, col: &str) -> Vec<Card> {
    all.into_iter().filter(|c| c.column == col).collect()
}
#[component]
fn BoardBody() -> Element {
    rsx! { Column { cards: column_cards(vec![], "todo") } }
}
"#);
        assert!(r.findings.is_empty(), "{r:?}");
    }

    /// Calls outside an rsx! body (e.g. an event handler computing a
    /// one-shot value) don't recompute on render — must not fire.
    #[test]
    fn silent_when_call_is_outside_rsx() {
        let r = run(r#"
fn column_cards(all: &[Card], col: &str) -> Vec<Card> { Vec::new() }
#[component]
fn BoardBody() -> Element {
    let cards = use_signal(Vec::<Card>::new);
    let _ = column_cards(&cards.read(), "todo");
    rsx! { div {} }
}
"#);
        assert!(r.findings.is_empty(), "{r:?}");
    }
}
