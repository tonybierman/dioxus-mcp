//! `vec_or_owned_prop_passthrough`: hint when a `#[component] fn` takes an
//! owned non-Copy / non-Signal arg type (`Vec<T>`, `Box<T>`, or a user
//! struct) that will be re-cloned on every parent render.
//!
//! Pattern: a generator hands child components owned values instead of
//! `ReadOnlySignal<T>` / `Rc<[T]>`. The first render is fine — but every
//! parent reactive write triggers another render of the parent (and a
//! re-clone of the args), even if the data hasn't changed.
//!
//! iter03's canonical cases:
//!   - `fn Column(cards: Vec<Card>, …)` — re-cloned per BoardBody render
//!   - `fn CardItem(card: Card, …)` — re-cloned per Column render
//!
//! Suggestion: convert to `ReadOnlySignal<Vec<Card>>` for the Vec or
//! `Rc<[Card]>` for an immutable slice; for single owned structs, hand a
//! `ReadOnlySignal<Card>`. The parent then only re-renders the child when
//! the underlying signal actually changes.
//!
//! Confidence:
//!   - `medium` when the parent has reactive writes (the re-clone happens
//!     repeatedly under user interaction) — this is the "real" hit.
//!   - `low` when there's no upstream parent or no parent with reactive
//!     writes — the lint still surfaces the type, but the cost is bounded
//!     to mount.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use quote::ToTokens;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::spanned::Spanned;
use syn::visit::Visit;

use crate::state::State;
use crate::tools::ast::{ParseError, ScannedFile, collect_parse_errors, walk_rs_files};
use crate::tools::scaffold::crate_root;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct VecOrOwnedPropParams {
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct VecOrOwnedFinding {
    pub code: &'static str,
    pub severity: &'static str,
    pub confidence: &'static str,
    pub file: PathBuf,
    pub line: usize,
    pub component: String,
    pub arg_name: String,
    pub arg_type: String,
    /// Components observed CALLING this component in their rsx and that
    /// themselves have reactive writes in their body. Surfaces the actual
    /// re-render driver for the reviewer.
    pub reactive_callers: Vec<String>,
    pub message: String,
    pub fix: String,
}

#[derive(Debug, Serialize)]
pub struct VecOrOwnedPropReport {
    pub findings: Vec<VecOrOwnedFinding>,
    pub parse_errors: Vec<ParseError>,
}

pub async fn vec_or_owned_prop_passthrough(
    state: &Arc<State>,
    p: VecOrOwnedPropParams,
) -> Result<VecOrOwnedPropReport, String> {
    let root = crate_root(state, p.project_root.as_deref()).await?;
    let src_root = root.join("src");
    let files = walk_rs_files(&src_root);

    // Pre-scan: components with reactive writes in their body, plus a
    // forward map of (parent_component -> set of child components called
    // in its rsx). We need both to compute confidence.
    let mut reactive: HashSet<String> = HashSet::new();
    let mut child_calls: HashMap<String, HashSet<String>> = HashMap::new();
    let mut components: Vec<ComponentSig> = Vec::new();
    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
        for item in &ast.items {
            let syn::Item::Fn(f) = item else { continue };
            if !is_component_fn(f) {
                continue;
            }
            let name = f.sig.ident.to_string();
            if has_reactive_writes(&f.block) {
                reactive.insert(name.clone());
            }
            let mut callee_scan = RsxCalleeScan {
                callees: HashSet::new(),
            };
            callee_scan.visit_block(&f.block);
            child_calls.insert(name.clone(), callee_scan.callees);
            components.push(ComponentSig {
                name,
                file: sf.path.clone(),
                inputs: f.sig.inputs.clone(),
            });
        }
    }

    // Reverse the child_calls map: for each child, who calls them?
    let mut callers_of: HashMap<String, HashSet<String>> = HashMap::new();
    for (parent, callees) in &child_calls {
        for callee in callees {
            callers_of
                .entry(callee.clone())
                .or_default()
                .insert(parent.clone());
        }
    }

    let mut findings: Vec<VecOrOwnedFinding> = Vec::new();
    for comp in &components {
        for input in &comp.inputs {
            let syn::FnArg::Typed(pt) = input else {
                continue;
            };
            let arg_name = match &*pt.pat {
                syn::Pat::Ident(p) => p.ident.to_string(),
                syn::Pat::Type(t) => match &*t.pat {
                    syn::Pat::Ident(p) => p.ident.to_string(),
                    _ => continue,
                },
                _ => continue,
            };
            let Some(category) = classify_owned_prop(&pt.ty) else {
                continue;
            };
            let ty_str = tighten_ws(&pt.ty.to_token_stream().to_string());

            let reactive_callers: Vec<String> = callers_of
                .get(&comp.name)
                .map(|set| {
                    let mut v: Vec<String> = set
                        .iter()
                        .filter(|c| reactive.contains(*c))
                        .cloned()
                        .collect();
                    v.sort();
                    v
                })
                .unwrap_or_default();

            let confidence: &'static str = if !reactive_callers.is_empty() {
                "medium"
            } else {
                "low"
            };

            let (message, fix) = match category {
                OwnedCategory::Vec(inner) => (
                    format!(
                        "`{comp}::{arg}: {ty}` is an owned `Vec<{inner}>` — every \
                         parent re-render reclones the whole vec. {hint}",
                        comp = comp.name,
                        arg = arg_name,
                        ty = ty_str,
                        inner = inner,
                        hint = if reactive_callers.is_empty() {
                            "No reactive parent caller observed; cost bounded to mount.".to_string()
                        } else {
                            format!(
                                "Callers with reactive writes: {}. \
                                 Each `.set()` / `.with_mut()` on a parent signal re-runs \
                                 the parent body and reclones this vec.",
                                reactive_callers.join(", "),
                            )
                        },
                    ),
                    format!(
                        "Switch to `{arg}: ReadOnlySignal<Vec<{inner}>>` and have the parent \
                         pass a signal handle instead of cloning the vec; or use \
                         `{arg}: Rc<[{inner}]>` if the slice is immutable and you want a \
                         cheap clone without a signal.",
                        arg = arg_name,
                        inner = inner,
                    ),
                ),
                OwnedCategory::Owned(name) => (
                    format!(
                        "`{comp}::{arg}: {ty}` is an owned `{name}` — every parent \
                         re-render reclones the struct. {hint}",
                        comp = comp.name,
                        arg = arg_name,
                        ty = ty_str,
                        name = name,
                        hint = if reactive_callers.is_empty() {
                            "No reactive parent caller observed; cost bounded to mount.".to_string()
                        } else {
                            format!(
                                "Callers with reactive writes: {}. \
                                 Each parent signal write reclones {name}.",
                                reactive_callers.join(", "),
                            )
                        },
                    ),
                    format!(
                        "Switch to `{arg}: ReadOnlySignal<{name}>` and have the parent \
                         pass a signal handle; or `{arg}: Rc<{name}>` if the struct is \
                         immutable and you want a cheap clone without a signal.",
                        arg = arg_name,
                        name = name,
                    ),
                ),
            };
            findings.push(VecOrOwnedFinding {
                code: "vec_or_owned_prop_passthrough",
                severity: "info",
                confidence,
                file: comp.file.clone(),
                line: pt.pat.span().start().line,
                component: comp.name.clone(),
                arg_name,
                arg_type: ty_str,
                reactive_callers,
                message,
                fix,
            });
        }
    }
    findings.sort_by(|a, b| {
        a.component
            .cmp(&b.component)
            .then(a.arg_name.cmp(&b.arg_name))
    });

    Ok(VecOrOwnedPropReport {
        findings,
        parse_errors: collect_parse_errors(&files),
    })
}

struct ComponentSig {
    name: String,
    file: PathBuf,
    inputs: syn::punctuated::Punctuated<syn::FnArg, syn::Token![,]>,
}

enum OwnedCategory {
    /// `Vec<T>` (or fully qualified `std::vec::Vec<T>`). Inner type name.
    Vec(String),
    /// A user-defined struct passed by value (no Signal wrapper). Stored
    /// last-segment name.
    Owned(String),
}

/// Classify whether an arg type is the "owned non-Signal non-Copy" shape
/// this lint targets. Returns None for trivially-Copy primitives, &T,
/// Signal-family wrappers, EventHandler, Memo, and the stdlib types we
/// don't want to flag (`String`, `&str` etc.).
fn classify_owned_prop(ty: &syn::Type) -> Option<OwnedCategory> {
    let syn::Type::Path(tp) = ty else { return None };
    let seg = tp.path.segments.last()?;
    let name = seg.ident.to_string();
    // Reactive containers are pre-cleared.
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
            | "Element"
            | "VNode"
    ) {
        return None;
    }
    // Vec<T> → flag specifically.
    if name == "Vec" {
        if let syn::PathArguments::AngleBracketed(ab) = &seg.arguments
            && let Some(syn::GenericArgument::Type(inner)) = ab.args.first()
        {
            let inner_name = tighten_ws(&inner.to_token_stream().to_string());
            return Some(OwnedCategory::Vec(inner_name));
        }
        return Some(OwnedCategory::Vec("_".into()));
    }
    // Stdlib types we explicitly don't flag — too noisy. `String` shows
    // up as a common cheap-enough clone in Dioxus components; flagging it
    // would drown out the real hits.
    if matches!(
        name.as_str(),
        "String"
            | "str"
            | "bool"
            | "char"
            | "f32"
            | "f64"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "Option"
            | "Result"
            | "Box"
            | "Rc"
            | "Arc"
            | "Cow"
            | "HashMap"
            | "BTreeMap"
            | "HashSet"
            | "BTreeSet"
            | "PathBuf"
            | "Path"
    ) {
        return None;
    }
    // What remains: a user-defined type passed by value. That's the
    // shape we want to flag.
    Some(OwnedCategory::Owned(name))
}

fn is_component_fn(f: &syn::ItemFn) -> bool {
    f.attrs.iter().any(|a| {
        a.path()
            .segments
            .last()
            .map(|s| s.ident == "component")
            .unwrap_or(false)
    })
}

/// Does the block contain any `.set(…)` / `.with_mut(…)` / `+=` write on
/// a signal-shaped receiver? We're lenient: any of those tokens in a
/// method call qualifies. The goal is just to flag components whose body
/// will trigger re-renders during user interaction.
fn has_reactive_writes(block: &syn::Block) -> bool {
    struct Walk {
        found: bool,
    }
    impl<'ast> Visit<'ast> for Walk {
        fn visit_expr_method_call(&mut self, e: &'ast syn::ExprMethodCall) {
            if matches!(
                e.method.to_string().as_str(),
                "set" | "with_mut" | "write" | "replace" | "take" | "modify"
            ) {
                self.found = true;
            }
            syn::visit::visit_expr_method_call(self, e);
        }
        fn visit_expr_assign(&mut self, e: &'ast syn::ExprAssign) {
            // `local_lock += 1` parses as a compound assignment.
            // syn 2 models compound assigns as `ExprBinary` inside the
            // ExprAssign, but the simpler heuristic is: any assignment
            // in a closure-heavy body is likely a reactive write. Be
            // conservative — only count assignments whose lhs is an
            // ident (e.g. `x += 1`) not a field access.
            if matches!(&*e.left, syn::Expr::Path(_)) {
                self.found = true;
            }
            syn::visit::visit_expr_assign(self, e);
        }
        fn visit_expr_binary(&mut self, e: &'ast syn::ExprBinary) {
            // syn parses `local_lock += 1` as ExprBinary with op
            // BinOp::AddAssign when wrapped in parens; in plain stmt
            // position it's ExprAssignOp in syn 1 / ExprBinary in syn 2.
            // Either way, any `+=` / `-=` qualifies.
            if matches!(
                e.op,
                syn::BinOp::AddAssign(_)
                    | syn::BinOp::SubAssign(_)
                    | syn::BinOp::MulAssign(_)
                    | syn::BinOp::DivAssign(_)
                    | syn::BinOp::RemAssign(_)
                    | syn::BinOp::BitOrAssign(_)
                    | syn::BinOp::BitAndAssign(_)
                    | syn::BinOp::BitXorAssign(_)
                    | syn::BinOp::ShlAssign(_)
                    | syn::BinOp::ShrAssign(_)
            ) {
                self.found = true;
            }
            syn::visit::visit_expr_binary(self, e);
        }
    }
    let mut w = Walk { found: false };
    w.visit_block(block);
    w.found
}

/// Collect the names of component-shape identifiers invoked inside the
/// parent's rsx (uppercased idents directly followed by a `{ … }` group).
/// Mirrors `prop_drill`'s detection — we just don't care about props here.
struct RsxCalleeScan {
    callees: HashSet<String>,
}

impl<'ast> Visit<'ast> for RsxCalleeScan {
    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        if m.path
            .segments
            .last()
            .map(|s| s.ident == "rsx")
            .unwrap_or(false)
        {
            let tokens: Vec<proc_macro2::TokenTree> = m.tokens.clone().into_iter().collect();
            scan_invocations(&tokens, &mut self.callees);
        }
        syn::visit::visit_macro(self, m);
    }
}

fn scan_invocations(tokens: &[proc_macro2::TokenTree], out: &mut HashSet<String>) {
    use proc_macro2::{Delimiter, TokenTree};
    let mut i = 0;
    while i < tokens.len() {
        if let TokenTree::Ident(id) = &tokens[i] {
            let s = id.to_string();
            // Component-shape: first char uppercase. Plain HTML elements
            // are lowercase ("div", "span") so they won't match.
            if s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && let Some(TokenTree::Group(g)) = tokens.get(i + 1)
                && g.delimiter() == Delimiter::Brace
            {
                out.insert(s);
            }
        }
        if let TokenTree::Group(g) = &tokens[i] {
            let inner: Vec<TokenTree> = g.stream().into_iter().collect();
            scan_invocations(&inner, out);
        }
        i += 1;
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

#[allow(dead_code)]
fn _typed_unused(_: ScannedFile) {}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn run(files: &[(&str, &str)]) -> VecOrOwnedPropReport {
        let dir = TempDir::new().unwrap();
        let src_dir = dir.path().join("src");
        for (rel, content) in files {
            let p = src_dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, content).unwrap();
        }
        let scanned = walk_rs_files(&src_dir);

        let mut reactive: HashSet<String> = HashSet::new();
        let mut child_calls: HashMap<String, HashSet<String>> = HashMap::new();
        let mut components: Vec<ComponentSig> = Vec::new();
        for sf in &scanned {
            let Ok(ast) = &sf.ast else { continue };
            for item in &ast.items {
                let syn::Item::Fn(f) = item else { continue };
                if !is_component_fn(f) {
                    continue;
                }
                let name = f.sig.ident.to_string();
                if has_reactive_writes(&f.block) {
                    reactive.insert(name.clone());
                }
                let mut callee_scan = RsxCalleeScan {
                    callees: HashSet::new(),
                };
                callee_scan.visit_block(&f.block);
                child_calls.insert(name.clone(), callee_scan.callees);
                components.push(ComponentSig {
                    name,
                    file: sf.path.clone(),
                    inputs: f.sig.inputs.clone(),
                });
            }
        }
        let mut callers_of: HashMap<String, HashSet<String>> = HashMap::new();
        for (parent, callees) in &child_calls {
            for callee in callees {
                callers_of
                    .entry(callee.clone())
                    .or_default()
                    .insert(parent.clone());
            }
        }

        let mut findings: Vec<VecOrOwnedFinding> = Vec::new();
        for comp in &components {
            for input in &comp.inputs {
                let syn::FnArg::Typed(pt) = input else {
                    continue;
                };
                let arg_name = match &*pt.pat {
                    syn::Pat::Ident(p) => p.ident.to_string(),
                    _ => continue,
                };
                let Some(category) = classify_owned_prop(&pt.ty) else {
                    continue;
                };
                let ty_str = tighten_ws(&pt.ty.to_token_stream().to_string());
                let reactive_callers: Vec<String> = callers_of
                    .get(&comp.name)
                    .map(|set| {
                        set.iter()
                            .filter(|c| reactive.contains(*c))
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                let confidence: &'static str = if !reactive_callers.is_empty() {
                    "medium"
                } else {
                    "low"
                };
                let _ = category;
                findings.push(VecOrOwnedFinding {
                    code: "vec_or_owned_prop_passthrough",
                    severity: "info",
                    confidence,
                    file: comp.file.clone(),
                    line: pt.pat.span().start().line,
                    component: comp.name.clone(),
                    arg_name,
                    arg_type: ty_str,
                    reactive_callers,
                    message: String::new(),
                    fix: String::new(),
                });
            }
        }
        VecOrOwnedPropReport {
            findings,
            parse_errors: Vec::new(),
        }
    }

    /// iter03 Column shape: `cards: Vec<Card>` on a child whose parent
    /// (BoardBody) has reactive writes via `cards.with_mut(...)`. Must
    /// fire at `medium` confidence.
    #[test]
    fn flags_vec_with_reactive_parent_as_medium() {
        let r = run(&[(
            "board.rs",
            r#"use dioxus::prelude::*;
#[component]
fn BoardBody() -> Element {
    let mut cards = use_signal(Vec::<Card>::new);
    let on_add = move |_| {
        cards.with_mut(|c| c.push(Card::default()));
    };
    let c = cards.read().clone();
    rsx! { Column { cards: c } }
}

#[component]
fn Column(cards: Vec<Card>) -> Element { rsx! { div {} } }
"#,
        )]);
        let f = r
            .findings
            .iter()
            .find(|f| f.component == "Column" && f.arg_name == "cards")
            .expect("Column.cards must surface");
        assert_eq!(f.confidence, "medium");
        assert_eq!(f.reactive_callers, vec!["BoardBody".to_string()]);
    }

    /// Bare-mount component with no upstream caller — re-clone cost is
    /// just at mount. Fire at `low` confidence.
    #[test]
    fn no_reactive_parent_lands_at_low_confidence() {
        let r = run(&[(
            "lone.rs",
            r#"use dioxus::prelude::*;
#[component]
fn Lone(cards: Vec<i32>) -> Element { rsx! { div {} } }
"#,
        )]);
        let f = r
            .findings
            .iter()
            .find(|f| f.component == "Lone")
            .expect("Lone.cards must surface");
        assert_eq!(f.confidence, "low");
        assert!(f.reactive_callers.is_empty());
    }

    /// Args wrapped in `Signal<…>` / `ReadOnlySignal<…>` / `EventHandler<…>`
    /// are reactive handles — the lint must NOT fire on those.
    #[test]
    fn does_not_flag_signal_family_props() {
        let r = run(&[(
            "ok.rs",
            r#"use dioxus::prelude::*;
#[component]
fn Ok(
    items: ReadOnlySignal<Vec<i32>>,
    cards: Signal<Vec<String>>,
    on_pick: EventHandler<String>,
) -> Element { rsx! { div {} } }
"#,
        )]);
        assert!(
            r.findings.is_empty(),
            "must not flag signal wrappers: {r:?}"
        );
    }

    /// Stdlib types we explicitly skip — `String` etc. — must NOT fire.
    #[test]
    fn does_not_flag_string_prop() {
        let r = run(&[(
            "s.rs",
            r#"use dioxus::prelude::*;
#[component]
fn Tag(name: String) -> Element { rsx! { div {} } }
"#,
        )]);
        assert!(r.findings.is_empty(), "String is on the skip list: {r:?}");
    }
}
