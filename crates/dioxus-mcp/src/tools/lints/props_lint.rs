use std::path::PathBuf;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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

    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
        for item in &ast.items {
            let syn::Item::Struct(s) = item else { continue };
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
