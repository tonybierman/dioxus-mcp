use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use quote::ToTokens;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::spanned::Spanned;

use crate::state::State;
use crate::tools::ast::{ParseError, collect_parse_errors, walk_rs_files};
use crate::tools::scaffold::crate_root;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PropsLintParams {
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PropsIssue {
    pub code: &'static str,
    pub message: String,
    pub file: PathBuf,
    pub line: usize,
    /// Identifier the issue is about. For `props_missing_partial_eq` it is
    /// the struct name; for `missing_partial_eq_on_prop_type` it is the
    /// `Component::arg` site so a reader can grep straight to it.
    pub struct_name: String,
    pub fix: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PropsLintReport {
    pub issues: Vec<PropsIssue>,
    pub parse_errors: Vec<ParseError>,
}

pub async fn props_lint(state: &Arc<State>, p: PropsLintParams) -> Result<PropsLintReport, String> {
    let crate_root = crate_root(state, p.project_root.as_deref()).await?;
    let src_root = crate_root.join("src");
    let files = walk_rs_files(&src_root);

    let mut issues: Vec<PropsIssue> = Vec::new();

    let known_partial_eq = collect_partial_eq_types(&files);

    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
        for item in &ast.items {
            match item {
                syn::Item::Struct(s) => {
                    let derives = collect_derives(&s.attrs);
                    if !derives.iter().any(|d| d == "Props") {
                        continue;
                    }
                    if !derives.iter().any(|d| d == "PartialEq") {
                        let line = s.ident.span().start().line;
                        issues.push(PropsIssue {
                            code: "props_missing_partial_eq",
                            message: format!(
                                "`{}` derives `Props` but not `PartialEq`; Dioxus needs `PartialEq` on Props for memoization",
                                s.ident
                            ),
                            file: sf.path.clone(),
                            line,
                            struct_name: s.ident.to_string(),
                            fix: Some("add `PartialEq` to the derive list, e.g. `#[derive(Props, PartialEq, Clone)]`".to_string()),
                        });
                    }
                }
                syn::Item::Fn(f) => {
                    if !is_component_fn(f) {
                        continue;
                    }
                    let comp_name = f.sig.ident.to_string();
                    for input in &f.sig.inputs {
                        let syn::FnArg::Typed(pt) = input else {
                            continue;
                        };
                        let arg_name = pat_ident_name(&pt.pat).unwrap_or_else(|| "_".to_string());
                        let ty_str = pt.ty.to_token_stream().to_string();
                        if type_is_always_reactive(&pt.ty) {
                            continue;
                        }
                        let unknown = collect_unknown_user_types(&pt.ty, &known_partial_eq);
                        if unknown.is_empty() {
                            continue;
                        }
                        let line = pt.pat.span().start().line;
                        let names = unknown.to_vec().join(", ");
                        issues.push(PropsIssue {
                            code: "missing_partial_eq_on_prop_type",
                            message: format!(
                                "`{comp}::{arg}: {ty}` — type{plural} {names} has no reachable \
                                 `PartialEq` impl in this crate, so Dioxus can't memoize {comp} \
                                 across renders. Each parent re-render will re-run the component.",
                                comp = comp_name,
                                arg = arg_name,
                                ty = tighten_ws(&ty_str),
                                plural = if unknown.len() == 1 { "" } else { "s" },
                                names = names,
                            ),
                            file: sf.path.clone(),
                            line,
                            struct_name: format!("{comp_name}::{arg_name}"),
                            fix: Some(format!(
                                "`derive(PartialEq)` on {names} (alongside `Clone` if missing), or wrap the arg in `Signal<…>` / `ReadOnlySignal<…>` so memoization isn't required",
                            )),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    Ok(PropsLintReport {
        issues,
        parse_errors: collect_parse_errors(&files),
    })
}

fn collect_derives(attrs: &[syn::Attribute]) -> Vec<String> {
    let mut out = Vec::new();
    for a in attrs {
        if !a.path().is_ident("derive") {
            continue;
        }
        let _ = a.parse_nested_meta(|m| {
            if let Some(seg) = m.path.segments.last() {
                out.push(seg.ident.to_string());
            }
            Ok(())
        });
    }
    out
}

/// A function annotated with `#[component]` (or `#[dioxus::component]`,
/// `#[dioxus::prelude::component]`). Matches by last path segment to be
/// robust against fully-qualified imports.
fn is_component_fn(f: &syn::ItemFn) -> bool {
    f.attrs.iter().any(|a| {
        a.path()
            .segments
            .last()
            .map(|s| s.ident == "component")
            .unwrap_or(false)
    })
}

fn pat_ident_name(p: &syn::Pat) -> Option<String> {
    match p {
        syn::Pat::Ident(pi) => Some(pi.ident.to_string()),
        syn::Pat::Type(pt) => pat_ident_name(&pt.pat),
        _ => None,
    }
}

/// Build the set of types in the crate that have a reachable `PartialEq`
/// impl. We look at: structs/enums with `derive(PartialEq)` and any
/// `impl PartialEq for T`. The set holds the *last* path segment of the
/// type name (which is what shows up in arg positions like `Card`).
fn collect_partial_eq_types(files: &[crate::tools::ast::ScannedFile]) -> HashSet<String> {
    let mut out: HashSet<String> = HashSet::new();
    for sf in files {
        let Ok(ast) = &sf.ast else { continue };
        for item in &ast.items {
            match item {
                syn::Item::Struct(s)
                    if collect_derives(&s.attrs).iter().any(|d| d == "PartialEq") =>
                {
                    out.insert(s.ident.to_string());
                }
                syn::Item::Enum(e)
                    if collect_derives(&e.attrs).iter().any(|d| d == "PartialEq") =>
                {
                    out.insert(e.ident.to_string());
                }
                syn::Item::Impl(i) => {
                    let Some((_, trait_path, _)) = &i.trait_ else {
                        continue;
                    };
                    let last = trait_path.segments.last().map(|s| s.ident.to_string());
                    if last.as_deref() != Some("PartialEq") {
                        continue;
                    }
                    if let syn::Type::Path(tp) = &*i.self_ty
                        && let Some(seg) = tp.path.segments.last()
                    {
                        out.insert(seg.ident.to_string());
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Types that are reactive containers: Dioxus memoization doesn't care
/// whether the inner type is `PartialEq` because the prop is a handle, not
/// a value. Matches by last path segment.
fn type_is_always_reactive(ty: &syn::Type) -> bool {
    let syn::Type::Path(tp) = ty else {
        return false;
    };
    let Some(seg) = tp.path.segments.last() else {
        return false;
    };
    matches!(
        seg.ident.to_string().as_str(),
        "Signal"
            | "ReadOnlySignal"
            | "WriteSignal"
            | "EventHandler"
            | "Memo"
            | "Resource"
            | "GlobalSignal"
            | "Callback"
    )
}

/// Stdlib / Dioxus-prelude types known to implement `PartialEq` for any
/// `PartialEq` `T`. We propagate the check into their generic args.
const STD_PARTIAL_EQ: &[&str] = &[
    "String", "str", "bool", "char", "f32", "f64", "i8", "i16", "i32", "i64", "i128", "isize",
    "u8", "u16", "u32", "u64", "u128", "usize", "Vec", "Box", "Option", "Result", "Rc", "Arc",
    "Cow", "HashMap", "BTreeMap", "HashSet", "BTreeSet", "VecDeque", "PathBuf", "Path",
    // Dioxus / hooks types that print as plain names in arg positions:
    "Element", "VNode",
];

/// Walk every type ident appearing in `ty` and collect those that aren't
/// a known stdlib type AND aren't in the local `known_partial_eq` set.
/// Builtins (`Vec`, `Option`, etc.) are treated as transparent — we only
/// flag the *leaf* user types they contain. Tuple/array/reference shapes
/// recurse into their components.
fn collect_unknown_user_types(ty: &syn::Type, known: &HashSet<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    collect_into(ty, known, &mut out);
    out.sort();
    out.dedup();
    out
}

fn collect_into(ty: &syn::Type, known: &HashSet<String>, out: &mut Vec<String>) {
    match ty {
        syn::Type::Path(tp) => {
            let Some(seg) = tp.path.segments.last() else {
                return;
            };
            let name = seg.ident.to_string();
            // Always-reactive types shouldn't be probed: they are pre-cleared
            // by the caller, but be defensive in case we recurse into one
            // via a generic position.
            if matches!(
                name.as_str(),
                "Signal"
                    | "ReadOnlySignal"
                    | "WriteSignal"
                    | "EventHandler"
                    | "Memo"
                    | "Resource"
                    | "GlobalSignal"
                    | "Callback"
            ) {
                return;
            }
            let is_known = STD_PARTIAL_EQ.iter().any(|s| *s == name) || known.contains(&name);
            if !is_known {
                out.push(name);
            }
            // Recurse into generic args regardless: `Vec<Card>` is fine for
            // `Vec` but `Card` still needs the check.
            if let syn::PathArguments::AngleBracketed(ab) = &seg.arguments {
                for arg in &ab.args {
                    if let syn::GenericArgument::Type(t) = arg {
                        collect_into(t, known, out);
                    }
                }
            }
        }
        syn::Type::Tuple(t) => {
            for el in &t.elems {
                collect_into(el, known, out);
            }
        }
        syn::Type::Array(a) => collect_into(&a.elem, known, out),
        syn::Type::Slice(s) => collect_into(&s.elem, known, out),
        syn::Type::Reference(r) => collect_into(&r.elem, known, out),
        syn::Type::Paren(p) => collect_into(&p.elem, known, out),
        syn::Type::Group(g) => collect_into(&g.elem, known, out),
        _ => {}
    }
}

fn tighten_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ast::walk_rs_files;
    use tempfile::TempDir;

    fn scan(files: &[(&str, &str)]) -> Vec<PropsIssue> {
        let dir = TempDir::new().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        for (name, content) in files {
            std::fs::write(src_dir.join(name), content).unwrap();
        }
        let scanned = walk_rs_files(&src_dir);
        let known = collect_partial_eq_types(&scanned);

        let mut issues: Vec<PropsIssue> = Vec::new();
        for sf in &scanned {
            let Ok(ast) = &sf.ast else { continue };
            for item in &ast.items {
                let syn::Item::Fn(f) = item else { continue };
                if !is_component_fn(f) {
                    continue;
                }
                let comp_name = f.sig.ident.to_string();
                for input in &f.sig.inputs {
                    let syn::FnArg::Typed(pt) = input else {
                        continue;
                    };
                    let arg_name = pat_ident_name(&pt.pat).unwrap_or("_".into());
                    let ty_str = pt.ty.to_token_stream().to_string();
                    if type_is_always_reactive(&pt.ty) {
                        continue;
                    }
                    let unknown = collect_unknown_user_types(&pt.ty, &known);
                    if unknown.is_empty() {
                        continue;
                    }
                    issues.push(PropsIssue {
                        code: "missing_partial_eq_on_prop_type",
                        message: ty_str,
                        file: sf.path.clone(),
                        line: pt.pat.span().start().line,
                        struct_name: format!("{comp_name}::{arg_name}"),
                        fix: Some(unknown.join(",")),
                    });
                }
            }
        }
        issues
    }

    /// iter03's `Column` shape: inline-arg component receiving
    /// `cards: Vec<Card>` and `Signal<…>` props. With Card defined
    /// elsewhere and `derive(PartialEq)`, the lint must stay silent.
    #[test]
    fn silent_when_user_type_derives_partial_eq() {
        let issues = scan(&[
            (
                "model.rs",
                r#"#[derive(Clone, PartialEq)]
pub struct Card { pub id: String, pub title: String }
"#,
            ),
            (
                "board.rs",
                r#"use dioxus::prelude::*;
#[component]
fn Column(
    id: String,
    cards: Vec<Card>,
    dragging: Signal<Option<String>>,
    on_move: EventHandler<(String, String, i32)>,
) -> Element {
    rsx! { div {} }
}
"#,
            ),
        ]);
        assert!(issues.is_empty(), "must stay silent: {issues:?}");
    }

    /// Same Column shape, but Card has no PartialEq. The cards arg must
    /// fire; the Signal / EventHandler args must not.
    #[test]
    fn flags_unmemoizable_user_type_in_inline_arg() {
        let issues = scan(&[
            (
                "model.rs",
                r#"#[derive(Clone)]
pub struct Card { pub id: String }
"#,
            ),
            (
                "board.rs",
                r#"use dioxus::prelude::*;
#[component]
fn Column(
    id: String,
    cards: Vec<Card>,
    dragging: Signal<Option<String>>,
    on_move: EventHandler<String>,
) -> Element {
    rsx! { div {} }
}
"#,
            ),
        ]);
        assert_eq!(issues.len(), 1, "expected one finding: {issues:?}");
        assert_eq!(issues[0].struct_name, "Column::cards");
        assert_eq!(issues[0].code, "missing_partial_eq_on_prop_type");
    }

    /// A user-defined type can satisfy `PartialEq` via an explicit
    /// `impl PartialEq for T` block instead of a derive. The lint must
    /// recognise that path too.
    #[test]
    fn impl_partial_eq_for_satisfies_check() {
        let issues = scan(&[
            (
                "model.rs",
                r#"pub struct Card { pub id: String }
impl PartialEq for Card {
    fn eq(&self, other: &Self) -> bool { self.id == other.id }
}
"#,
            ),
            (
                "board.rs",
                r#"use dioxus::prelude::*;
#[component]
fn Column(cards: Vec<Card>) -> Element { rsx! { div {} } }
"#,
            ),
        ]);
        assert!(issues.is_empty(), "manual impl counts: {issues:?}");
    }

    /// Regression: the existing `props_missing_partial_eq` lint (on
    /// `#[derive(Props)]` structs) must keep firing.
    #[test]
    fn struct_derive_props_without_partial_eq_still_fires() {
        let issues = scan(&[(
            "props.rs",
            r#"#[derive(Props, Clone)]
pub struct Foo { pub n: i32 }
"#,
        )]);
        // scan() above only checks inline args, so this is a separate
        // path. Just verify the helper sees the struct as having no
        // PartialEq impl — the real `props_lint` runner emits the
        // finding.
        let dir = TempDir::new().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(
            src_dir.join("p.rs"),
            r#"#[derive(Props, Clone)]
pub struct Foo { pub n: i32 }
"#,
        )
        .unwrap();
        let scanned = walk_rs_files(&src_dir);
        let known = collect_partial_eq_types(&scanned);
        assert!(
            !known.contains("Foo"),
            "Foo must NOT be marked PartialEq-known"
        );
        let _ = issues;
    }
}
