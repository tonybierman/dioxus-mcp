//! `shared_enum_validation`: detect a string-literal enum validated
//! independently on both the client and server side, and nudge towards a
//! shared `enum`.
//!
//! Pattern: iter03's `const COLUMNS: [(&str, &str); 3] = [("todo", …),
//! ("doing", …), ("done", …)]` in `board_screen.rs:8` AND the server fns
//! `move_card` / `create_card` pattern-matching `"todo" | "doing" | "done"`
//! independently. Both sides have to agree about the literal set — and
//! they will, until someone adds `"review"` to one half.
//!
//! Detection — narrow on purpose so unrelated string lookups don't false
//! positive:
//!   1. Scan every `const NAME: [(&str, …); N] = […]` (or `&[(&str, …)]`)
//!      under `src/components/`. Pull the first-position string literals
//!      into a sorted set `S_client`.
//!   2. Scan every server-fn body for `match <expr> { "x" | "y" | "z" =>
//!      … }` arms whose pattern is a literal alternation of ≥ 2 string
//!      literals. Pull each alternation into a sorted set.
//!   3. For each server alternation set that exactly equals a client
//!      const set, emit a finding pointing at both sites and suggesting a
//!      shared `enum` under `src/model/`.
//!
//! Severity `info`, confidence `low` — the duplication isn't a bug, it's
//! a future refactor candidate that gets more valuable the more times
//! the set changes.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::visit::Visit;

use crate::state::State;
use crate::tools::ast::{ParseError, collect_parse_errors, walk_rs_files};
use crate::tools::scaffold::crate_root;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SharedEnumValidationParams {
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ClientSite {
    pub file: PathBuf,
    pub line: usize,
    /// `const NAME` the array is bound to.
    pub binding: String,
}

#[derive(Debug, Serialize)]
pub struct ServerSite {
    pub file: PathBuf,
    pub line: usize,
    pub server_fn: String,
}

#[derive(Debug, Serialize)]
pub struct SharedEnumFinding {
    pub code: &'static str,
    pub severity: &'static str,
    pub confidence: &'static str,
    /// Sorted list of literal values that both sides agree on (e.g.
    /// `["doing", "done", "todo"]`).
    pub values: Vec<String>,
    pub client_sites: Vec<ClientSite>,
    pub server_sites: Vec<ServerSite>,
    pub message: String,
    pub fix: String,
}

#[derive(Debug, Serialize)]
pub struct SharedEnumReport {
    pub findings: Vec<SharedEnumFinding>,
    pub parse_errors: Vec<ParseError>,
}

pub async fn shared_enum_validation(
    state: &Arc<State>,
    p: SharedEnumValidationParams,
) -> Result<SharedEnumReport, String> {
    let root = crate_root(state, p.project_root.as_deref()).await?;
    let src_root = root.join("src");
    let components_root = src_root.join("components");
    let files = walk_rs_files(&src_root);

    // Client side: const arrays of tuple-first-string literals.
    let mut client_by_set: HashMap<BTreeSet<String>, Vec<ClientSite>> = HashMap::new();
    for sf in &files {
        if !sf.path.starts_with(&components_root) {
            continue;
        }
        let Ok(ast) = &sf.ast else { continue };
        for item in &ast.items {
            let syn::Item::Const(c) = item else { continue };
            let Some(literals) = extract_const_array_first_strings(&c.expr) else {
                continue;
            };
            if literals.len() < 2 {
                continue;
            }
            let set: BTreeSet<String> = literals.into_iter().collect();
            client_by_set.entry(set).or_default().push(ClientSite {
                file: sf.path.clone(),
                line: c.ident.span().start().line,
                binding: c.ident.to_string(),
            });
        }
    }

    // Server side: match arms with `"a" | "b" | "c"` patterns inside server fns.
    let mut server_by_set: HashMap<BTreeSet<String>, Vec<ServerSite>> = HashMap::new();
    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
        for item in &ast.items {
            let syn::Item::Fn(f) = item else { continue };
            if !is_server_fn(f) {
                continue;
            }
            let server_fn_name = f.sig.ident.to_string();
            let mut v = MatchArmVisitor {
                alternations: Vec::new(),
            };
            v.visit_block(&f.block);
            for (set, line) in v.alternations {
                if set.len() < 2 {
                    continue;
                }
                server_by_set.entry(set).or_default().push(ServerSite {
                    file: sf.path.clone(),
                    line,
                    server_fn: server_fn_name.clone(),
                });
            }
        }
    }

    let mut findings: Vec<SharedEnumFinding> = Vec::new();
    for (set, client_sites) in &client_by_set {
        let Some(server_sites) = server_by_set.get(set) else {
            continue;
        };
        let values: Vec<String> = set.iter().cloned().collect();
        let values_str = values
            .iter()
            .map(|v| format!("{v:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        findings.push(SharedEnumFinding {
            code: "shared_enum_validation",
            severity: "info",
            confidence: "low",
            values: values.clone(),
            client_sites: client_sites.clone(),
            server_sites: server_sites.clone(),
            message: format!(
                "Client const array and server `match` patterns both pin the same \
                 string set {{ {values_str} }} independently. Either side adding a \
                 value silently desyncs from the other.",
            ),
            fix: format!(
                "Define an `enum` in `src/model/` (with `serde::Serialize` + \
                 `serde::Deserialize` + `Copy`) covering {{ {values_str} }} and \
                 reference it from both halves: the client `for` loop drives off \
                 `<Enum as IntoEnumIterator>::iter()` (with `strum`), and the \
                 server fn args take the enum directly so the pattern match is \
                 exhaustive at compile time."
            ),
        });
    }
    findings.sort_by(|a, b| a.values.cmp(&b.values));

    Ok(SharedEnumReport {
        findings,
        parse_errors: collect_parse_errors(&files),
    })
}

fn is_server_fn(f: &syn::ItemFn) -> bool {
    f.attrs.iter().any(|a| {
        let last = a.path().segments.last().map(|s| s.ident.to_string());
        matches!(
            last.as_deref(),
            Some("server" | "get" | "post" | "put" | "delete" | "patch")
        )
    })
}

/// Implement Clone for ClientSite/ServerSite so we can re-pack into findings.
impl Clone for ClientSite {
    fn clone(&self) -> Self {
        Self {
            file: self.file.clone(),
            line: self.line,
            binding: self.binding.clone(),
        }
    }
}
impl Clone for ServerSite {
    fn clone(&self) -> Self {
        Self {
            file: self.file.clone(),
            line: self.line,
            server_fn: self.server_fn.clone(),
        }
    }
}

/// Pull the first-position string literals from `[("a", …), ("b", …), …]`
/// (or a `&[…]` reference variant). Returns `None` when the expression
/// isn't an array of tuple-first-strings.
fn extract_const_array_first_strings(expr: &syn::Expr) -> Option<Vec<String>> {
    let array = match expr {
        syn::Expr::Array(a) => a,
        syn::Expr::Reference(r) => match &*r.expr {
            syn::Expr::Array(a) => a,
            _ => return None,
        },
        _ => return None,
    };
    let mut out: Vec<String> = Vec::new();
    for el in &array.elems {
        match el {
            syn::Expr::Tuple(t) => {
                let first = t.elems.first()?;
                let s = lit_str_value(first)?;
                out.push(s);
            }
            syn::Expr::Lit(l) => {
                if let syn::Lit::Str(s) = &l.lit {
                    out.push(s.value());
                }
            }
            _ => return None,
        }
    }
    Some(out)
}

fn lit_str_value(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Lit(l) => {
            if let syn::Lit::Str(s) = &l.lit {
                Some(s.value())
            } else {
                None
            }
        }
        syn::Expr::Reference(r) => lit_str_value(&r.expr),
        _ => None,
    }
}

struct MatchArmVisitor {
    /// (literal-set, line of the match).
    alternations: Vec<(BTreeSet<String>, usize)>,
}

impl<'ast> Visit<'ast> for MatchArmVisitor {
    fn visit_expr_match(&mut self, m: &'ast syn::ExprMatch) {
        for arm in &m.arms {
            let set = collect_literal_alternation(&arm.pat);
            if set.len() >= 2 {
                self.alternations
                    .push((set, m.match_token.span.start().line));
            }
        }
        syn::visit::visit_expr_match(self, m);
    }
}

/// `"a" | "b" | "c"` parses as `Pat::Or` with literal arms; collect every
/// literal under the Or (recursively) and return the set. Skips Or-arms
/// that contain non-literal patterns.
fn collect_literal_alternation(p: &syn::Pat) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    fn walk(p: &syn::Pat, out: &mut BTreeSet<String>, all_lits: &mut bool) {
        match p {
            syn::Pat::Or(or) => {
                for case in &or.cases {
                    walk(case, out, all_lits);
                }
            }
            syn::Pat::Lit(syn::PatLit {
                lit: syn::Lit::Str(s),
                ..
            }) => {
                out.insert(s.value());
            }
            _ => {
                *all_lits = false;
            }
        }
    }
    let mut all_lits = true;
    walk(p, &mut out, &mut all_lits);
    if !all_lits {
        return BTreeSet::new();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn run(files: &[(&str, &str)]) -> SharedEnumReport {
        let dir = TempDir::new().unwrap();
        let src_dir = dir.path().join("src");
        for (rel, content) in files {
            let p = src_dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, content).unwrap();
        }
        let scanned = walk_rs_files(&src_dir);
        let components_root = src_dir.join("components");

        let mut client_by_set: HashMap<BTreeSet<String>, Vec<ClientSite>> = HashMap::new();
        for sf in &scanned {
            if !sf.path.starts_with(&components_root) {
                continue;
            }
            let Ok(ast) = &sf.ast else { continue };
            for item in &ast.items {
                let syn::Item::Const(c) = item else { continue };
                let Some(literals) = extract_const_array_first_strings(&c.expr) else {
                    continue;
                };
                if literals.len() < 2 {
                    continue;
                }
                let set: BTreeSet<String> = literals.into_iter().collect();
                client_by_set.entry(set).or_default().push(ClientSite {
                    file: sf.path.clone(),
                    line: c.ident.span().start().line,
                    binding: c.ident.to_string(),
                });
            }
        }

        let mut server_by_set: HashMap<BTreeSet<String>, Vec<ServerSite>> = HashMap::new();
        for sf in &scanned {
            let Ok(ast) = &sf.ast else { continue };
            for item in &ast.items {
                let syn::Item::Fn(f) = item else { continue };
                if !is_server_fn(f) {
                    continue;
                }
                let mut v = MatchArmVisitor {
                    alternations: Vec::new(),
                };
                v.visit_block(&f.block);
                for (set, line) in v.alternations {
                    if set.len() < 2 {
                        continue;
                    }
                    server_by_set.entry(set).or_default().push(ServerSite {
                        file: sf.path.clone(),
                        line,
                        server_fn: f.sig.ident.to_string(),
                    });
                }
            }
        }

        let mut findings: Vec<SharedEnumFinding> = Vec::new();
        for (set, client_sites) in &client_by_set {
            let Some(server_sites) = server_by_set.get(set) else {
                continue;
            };
            let values: Vec<String> = set.iter().cloned().collect();
            findings.push(SharedEnumFinding {
                code: "shared_enum_validation",
                severity: "info",
                confidence: "low",
                values,
                client_sites: client_sites.clone(),
                server_sites: server_sites.clone(),
                message: String::new(),
                fix: String::new(),
            });
        }
        SharedEnumReport {
            findings,
            parse_errors: Vec::new(),
        }
    }

    /// iter03's COLUMNS / move_card shape: const `("todo", …)` array on
    /// client AND a server-side `match column { "todo" | "doing" | "done"
    /// => ok }`. Must fire.
    #[test]
    fn flags_columns_const_paired_with_server_match() {
        let r = run(&[
            (
                "components/board.rs",
                r#"const COLUMNS: [(&str, &str); 3] =
    [("todo", "Todo"), ("doing", "Doing"), ("done", "Done")];
"#,
            ),
            (
                "server/move_card.rs",
                r#"#[post("/api/move")]
async fn move_card(column: String) -> Result<(), ServerFnError> {
    match column.as_str() {
        "todo" | "doing" | "done" => Ok(()),
        _ => Err(ServerFnError::ServerError("bad col".into())),
    }
}
"#,
            ),
        ]);
        assert_eq!(r.findings.len(), 1, "expected one finding: {r:?}");
        let f = &r.findings[0];
        assert_eq!(f.values, vec!["doing", "done", "todo"]);
        assert!(!f.client_sites.is_empty());
        assert!(!f.server_sites.is_empty());
    }

    /// Server match without a matching client const — no finding.
    #[test]
    fn silent_when_server_match_has_no_client_const() {
        let r = run(&[(
            "server/x.rs",
            r#"#[post("/api/x")]
async fn x(s: String) -> Result<(), ServerFnError> {
    match s.as_str() {
        "a" | "b" => Ok(()),
        _ => Err(ServerFnError::ServerError("".into())),
    }
}
"#,
        )]);
        assert!(r.findings.is_empty(), "server-only set: {r:?}");
    }

    /// Sets must match exactly — a client `["todo", "doing", "done"]`
    /// paired with a server `"todo" | "doing"` (subset) doesn't count.
    #[test]
    fn silent_when_sets_do_not_match_exactly() {
        let r = run(&[
            (
                "components/board.rs",
                r#"const COLUMNS: [(&str, &str); 3] =
    [("todo", "Todo"), ("doing", "Doing"), ("done", "Done")];
"#,
            ),
            (
                "server/mismatch.rs",
                r#"#[post("/api/mismatch")]
async fn m(s: String) -> Result<(), ServerFnError> {
    match s.as_str() {
        "todo" | "doing" => Ok(()),
        _ => Err(ServerFnError::ServerError("".into())),
    }
}
"#,
            ),
        ]);
        assert!(r.findings.is_empty(), "subset shouldn't match: {r:?}");
    }
}
