//! `duplicate_helper_across_client_and_server`: flag pure helper fns whose
//! body is byte-identical across `src/components/` and `src/server/`.
//!
//! Pattern: a generator that hits "this logic needs to run on both sides of
//! a server fn" copy-pastes the helper into both halves of the app instead
//! of lifting it into `src/model/`. iter03 has `fn normalize_positions(list:
//! &mut Vec<Card>)` defined verbatim in `src/components/board_screen.rs` AND
//! `src/server/state.rs`. The two definitions WILL drift; both call sites
//! end up depending on identical impls until one side patches a bug and the
//! other doesn't.
//!
//! Detection: for every top-level `fn` in `src/components/` and
//! `src/server/`, take the normalized source of the *body* (whitespace
//! collapsed). Group by `(fn_name, normalized_body)`. A group with at
//! least one site in each directory is a finding.
//!
//! Severity is `warning` — the drift risk is real, but a generator might
//! ship the duplicate intentionally if the model layer hasn't been built
//! yet. The fix is mechanical: move the fn into `src/model/<name>.rs` and
//! re-export from both call sites.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use quote::ToTokens;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::state::State;
use crate::tools::ast::{ParseError, ScannedFile, collect_parse_errors, walk_rs_files};
use crate::tools::scaffold::crate_root;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct DuplicateHelperParams {
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct HelperSite {
    pub file: PathBuf,
    pub line: usize,
    /// Which side of the app this site lives on: `client` (under
    /// `src/components/`) or `server` (under `src/server/`).
    pub side: &'static str,
}

#[derive(Debug, Serialize)]
pub struct DuplicateHelperFinding {
    pub code: &'static str,
    pub severity: &'static str,
    pub fn_name: String,
    /// Every site sharing the byte-identical body. Always contains at least
    /// one `client` and one `server` site (otherwise we wouldn't emit).
    pub sites: Vec<HelperSite>,
    pub message: String,
    pub fix: String,
}

#[derive(Debug, Serialize)]
pub struct DuplicateHelperReport {
    pub findings: Vec<DuplicateHelperFinding>,
    pub parse_errors: Vec<ParseError>,
}

pub async fn duplicate_helper_client_server(
    state: &Arc<State>,
    p: DuplicateHelperParams,
) -> Result<DuplicateHelperReport, String> {
    let root = crate_root(state, p.project_root.as_deref()).await?;
    let src_root = root.join("src");
    let files = walk_rs_files(&src_root);
    let components_root = src_root.join("components");
    let server_root = src_root.join("server");

    let mut groups: HashMap<(String, String), Vec<HelperSite>> = HashMap::new();
    for sf in &files {
        let Some(side) = classify_side(&sf.path, &components_root, &server_root) else {
            continue;
        };
        collect_fn_bodies(sf, side, &mut groups);
    }

    let mut findings: Vec<DuplicateHelperFinding> = Vec::new();
    for ((fn_name, _body_key), sites) in groups {
        if sites.len() < 2 {
            continue;
        }
        let has_client = sites.iter().any(|s| s.side == "client");
        let has_server = sites.iter().any(|s| s.side == "server");
        if !(has_client && has_server) {
            continue;
        }
        let n = sites.len();
        let summary_locs: Vec<String> = sites
            .iter()
            .map(|s| format!("{}:{} ({})", s.file.display(), s.line, s.side,))
            .collect();
        findings.push(DuplicateHelperFinding {
            code: "duplicate_helper_across_client_and_server",
            severity: "warning",
            fn_name: fn_name.clone(),
            sites: sites.clone(),
            message: format!(
                "`fn {fn_name}` has byte-identical bodies at {n} sites across \
                 `src/components/` and `src/server/`: {locs}. Two impls of the \
                 same logic will drift — a bug fixed on one side won't reach \
                 the other.",
                locs = summary_locs.join(", "),
            ),
            fix: format!(
                "Move `fn {fn_name}` into `src/model/{module}.rs` (or an \
                 existing model module) and re-export it from both call sites.",
                module = fn_name,
            ),
        });
    }

    findings.sort_by(|a, b| a.fn_name.cmp(&b.fn_name));
    Ok(DuplicateHelperReport {
        findings,
        parse_errors: collect_parse_errors(&files),
    })
}

fn classify_side(
    path: &std::path::Path,
    components_root: &std::path::Path,
    server_root: &std::path::Path,
) -> Option<&'static str> {
    if path.starts_with(components_root) {
        Some("client")
    } else if path.starts_with(server_root) {
        Some("server")
    } else {
        None
    }
}

fn collect_fn_bodies(
    sf: &ScannedFile,
    side: &'static str,
    out: &mut HashMap<(String, String), Vec<HelperSite>>,
) {
    let Ok(ast) = &sf.ast else { return };
    for item in &ast.items {
        let syn::Item::Fn(f) = item else { continue };
        // Skip async fns — server fns themselves shouldn't be compared
        // against client-side handlers; only pure helpers (no
        // `#[server]` / `#[get]` / etc. attributes, no `async`) are the
        // duplication shape we want to flag.
        if f.sig.asyncness.is_some() {
            continue;
        }
        if is_server_fn_attribute(&f.attrs) || is_component_fn(&f.attrs) {
            continue;
        }
        // Skip extremely short bodies — those are usually wrappers that
        // legitimately differ in their server/client bindings and aren't
        // worth flagging. We measure by token-stream length (after
        // whitespace normalization) rather than statement count, because
        // a single for-loop fn body (one stmt, many tokens) is exactly
        // the duplication shape generators produce.
        let body_key = normalize(&f.block.to_token_stream().to_string());
        if body_key.len() < 40 {
            continue;
        }
        let name = f.sig.ident.to_string();
        out.entry((name, body_key)).or_default().push(HelperSite {
            file: sf.path.clone(),
            line: f.sig.ident.span().start().line,
            side,
        });
    }
}

fn is_server_fn_attribute(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        let last = a.path().segments.last().map(|s| s.ident.to_string());
        matches!(
            last.as_deref(),
            Some("server" | "get" | "post" | "put" | "delete" | "patch")
        )
    })
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

/// Collapse all whitespace runs to a single space and trim ends. We rely on
/// the tokenization stripping comments — `to_token_stream()` preserves
/// significant tokens but drops comments and line breaks.
fn normalize(s: &str) -> String {
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
    use tempfile::TempDir;

    fn run(files: &[(&str, &str)]) -> DuplicateHelperReport {
        let dir = TempDir::new().unwrap();
        let src_dir = dir.path().join("src");
        for (rel, content) in files {
            let path = src_dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, content).unwrap();
        }
        let scanned = walk_rs_files(&src_dir);
        let components_root = src_dir.join("components");
        let server_root = src_dir.join("server");
        let mut groups: HashMap<(String, String), Vec<HelperSite>> = HashMap::new();
        for sf in &scanned {
            if let Some(side) = classify_side(&sf.path, &components_root, &server_root) {
                collect_fn_bodies(sf, side, &mut groups);
            }
        }
        let mut findings = Vec::new();
        for ((fn_name, _body_key), sites) in groups {
            if sites.len() < 2 {
                continue;
            }
            let has_client = sites.iter().any(|s| s.side == "client");
            let has_server = sites.iter().any(|s| s.side == "server");
            if !(has_client && has_server) {
                continue;
            }
            findings.push(DuplicateHelperFinding {
                code: "duplicate_helper_across_client_and_server",
                severity: "warning",
                fn_name,
                sites,
                message: String::new(),
                fix: String::new(),
            });
        }
        DuplicateHelperReport {
            findings,
            parse_errors: Vec::new(),
        }
    }

    /// iter03's `normalize_positions` shape: byte-identical body in
    /// `components/` and `server/` — must fire.
    #[test]
    fn flags_identical_body_across_components_and_server() {
        let body = r#"
fn normalize_positions(board: &mut Vec<Card>) {
    let n = board.len();
    for i in 0..n {
        board[i].position = i as i32;
    }
}
"#;
        let r = run(&[("components/board.rs", body), ("server/state.rs", body)]);
        assert_eq!(r.findings.len(), 1, "expected one finding: {r:?}");
        assert_eq!(r.findings[0].fn_name, "normalize_positions");
        assert_eq!(r.findings[0].sites.len(), 2);
        let sides: Vec<&str> = r.findings[0].sites.iter().map(|s| s.side).collect();
        assert!(sides.contains(&"client"));
        assert!(sides.contains(&"server"));
    }

    /// Same name but different bodies — not a duplicate. Must stay silent.
    #[test]
    fn silent_when_bodies_diverge() {
        let r = run(&[
            (
                "components/board.rs",
                r#"fn shuffle(list: &mut Vec<i32>) {
    list.sort();
    list.reverse();
}"#,
            ),
            (
                "server/state.rs",
                r#"fn shuffle(list: &mut Vec<i32>) {
    list.sort();
}"#,
            ),
        ]);
        assert!(r.findings.is_empty(), "different bodies: {r:?}");
    }

    /// Duplicate within one side (two helpers in `components/`) is the
    /// "extract-a-fn-please" shape, not the cross-side drift this lint
    /// targets. Stay silent.
    #[test]
    fn silent_when_both_sites_on_one_side() {
        let body = r#"
fn helper(x: i32) -> i32 {
    let y = x * 2;
    y + 1
}
"#;
        let r = run(&[("components/a.rs", body), ("components/b.rs", body)]);
        assert!(r.findings.is_empty(), "both client-side: {r:?}");
    }

    /// `async fn` is excluded — server fns themselves would otherwise
    /// match against any handwritten async client helper of the same
    /// name. Only pure sync helpers are the duplication shape.
    #[test]
    fn ignores_async_fns() {
        let body = r#"
async fn shared(x: i32) -> Result<i32, ()> {
    let y = x * 2;
    Ok(y)
}
"#;
        let r = run(&[("components/a.rs", body), ("server/b.rs", body)]);
        assert!(r.findings.is_empty(), "async excluded: {r:?}");
    }
}
