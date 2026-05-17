use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use proc_macro2::TokenTree;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syn::spanned::Spanned;
use syn::visit::Visit;

use crate::state::State;
use crate::tools::scaffold::crate_root;
use crate::tools::scan::{ParseError, collect_parse_errors, walk_rs_files};

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AssetAuditParams {
    /// Directories to scan for asset files, relative to crate root. Defaults to `["assets"]`.
    #[serde(default)]
    pub assets_dirs: Option<Vec<String>>,
    /// Absolute path to the Dioxus project root. Defaults to the path the MCP server was
    /// started in.
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MissingAsset {
    pub path: String,
    pub file: PathBuf,
    pub line: usize,
}

#[derive(Debug, Serialize)]
pub struct AssetAuditReport {
    pub assets_dirs: Vec<PathBuf>,
    pub unreferenced_files: Vec<String>,
    pub missing_assets: Vec<MissingAsset>,
    pub dynamic_assets_skipped: usize,
    pub referenced_count: usize,
    pub total_files: usize,
    pub parse_errors: Vec<ParseError>,
}

pub async fn asset_audit(
    state: &Arc<State>,
    p: AssetAuditParams,
) -> Result<AssetAuditReport, String> {
    let crate_root = crate_root(state, p.project_root.as_deref()).await?;
    let dirs = p.assets_dirs.unwrap_or_else(|| vec!["assets".to_string()]);

    let mut file_index: HashSet<String> = HashSet::new();
    let mut files_listed: Vec<String> = Vec::new();
    let mut asset_dir_paths: Vec<PathBuf> = Vec::new();

    for d in &dirs {
        let abs = crate_root.join(d);
        if !abs.exists() {
            continue;
        }
        asset_dir_paths.push(abs.clone());
        for entry in walkdir::WalkDir::new(&abs).into_iter().flatten() {
            if !entry.file_type().is_file() {
                continue;
            }
            let rel_from_root = match entry.path().strip_prefix(&crate_root) {
                Ok(p) => to_forward_slash(p),
                Err(_) => continue,
            };
            let rel_from_assets = match entry.path().strip_prefix(&abs) {
                Ok(p) => to_forward_slash(p),
                Err(_) => continue,
            };
            files_listed.push(rel_from_root.clone());
            // Multiple candidate keys so references resolve regardless of how they're written.
            file_index.insert(rel_from_root.clone());
            file_index.insert(format!("/{rel_from_root}"));
            file_index.insert(rel_from_assets.clone());
            file_index.insert(format!("/{rel_from_assets}"));
        }
    }
    let total_files = files_listed.len();

    let src_root = crate_root.join("src");
    let files = walk_rs_files(&src_root);

    let mut referenced: HashSet<String> = HashSet::new();
    let mut missing_assets: Vec<MissingAsset> = Vec::new();
    let mut dynamic_assets_skipped: usize = 0;

    for sf in &files {
        let Ok(ast) = &sf.ast else { continue };
        let mut v = AssetVisitor {
            file: sf.path.clone(),
            referenced: &mut referenced,
            missing: &mut missing_assets,
            dynamic: &mut dynamic_assets_skipped,
            file_index: &file_index,
        };
        v.visit_file(ast);
    }

    let referenced_count = referenced.len();
    let mut unreferenced_files: Vec<String> = files_listed
        .iter()
        .filter(|f| {
            !referenced.contains(*f)
                && !referenced.contains(&format!("/{f}"))
                && !referenced_matches_any_form(f, &referenced)
        })
        .cloned()
        .collect();
    unreferenced_files.sort();

    Ok(AssetAuditReport {
        assets_dirs: asset_dir_paths,
        unreferenced_files,
        missing_assets,
        dynamic_assets_skipped,
        referenced_count,
        total_files,
        parse_errors: collect_parse_errors(&files),
    })
}

fn referenced_matches_any_form(file_rel_from_root: &str, referenced: &HashSet<String>) -> bool {
    // file_rel_from_root looks like "assets/logo.png"; references may be written as
    // "logo.png" (no dir), "/logo.png", "assets/logo.png", "/assets/logo.png", etc.
    let trimmed = file_rel_from_root.trim_start_matches('/');
    if referenced.contains(trimmed) || referenced.contains(&format!("/{trimmed}")) {
        return true;
    }
    if let Some(rest) = trimmed.split_once('/').map(|(_, r)| r)
        && (referenced.contains(rest) || referenced.contains(&format!("/{rest}")))
    {
        return true;
    }
    false
}

struct AssetVisitor<'a> {
    file: PathBuf,
    referenced: &'a mut HashSet<String>,
    missing: &'a mut Vec<MissingAsset>,
    dynamic: &'a mut usize,
    file_index: &'a HashSet<String>,
}

impl<'a, 'ast> Visit<'ast> for AssetVisitor<'a> {
    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        let last = m
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string().to_lowercase());
        match last.as_deref() {
            Some("asset") => {
                let line = m.path.span().start().line;
                self.record_asset_lit(first_string_literal(&m.tokens), line);
            }
            // syn::Visit doesn't recurse into macro token bodies. The 0.7 rsx!
            // macro can nest `asset!()` calls many levels deep — re-tokenize
            // and walk for them here. (Cover the small handful of macros that
            // legitimately wrap rsx so things like `rsx_node!` keep working.)
            Some("rsx") | Some("rsx_node") | Some("render") => {
                self.scan_tokens_for_asset(&m.tokens);
            }
            _ => {}
        }
        syn::visit::visit_macro(self, m);
    }
}

impl<'a> AssetVisitor<'a> {
    fn record_asset_lit(&mut self, lit: Option<String>, line: usize) {
        match lit {
            Some(lit) => {
                self.referenced.insert(lit.clone());
                let matched = self.file_index.contains(&lit)
                    || self.file_index.contains(lit.trim_start_matches('/'))
                    || self
                        .file_index
                        .contains(&format!("/{}", lit.trim_start_matches('/')));
                if !matched {
                    self.missing.push(MissingAsset {
                        path: lit,
                        file: self.file.clone(),
                        line,
                    });
                }
            }
            None => *self.dynamic += 1,
        }
    }

    fn scan_tokens_for_asset(&mut self, tokens: &proc_macro2::TokenStream) {
        let mut it = tokens.clone().into_iter().peekable();
        while let Some(tt) = it.next() {
            match tt {
                TokenTree::Ident(id) if id == "asset" => {
                    // Match `asset ! ( ... )` — bang then a parenthesized group.
                    let Some(TokenTree::Punct(p)) = it.peek() else {
                        continue;
                    };
                    if p.as_char() != '!' {
                        continue;
                    }
                    it.next();
                    let Some(TokenTree::Group(g)) = it.next() else {
                        continue;
                    };
                    let line = id.span().start().line;
                    self.record_asset_lit(first_string_literal(&g.stream()), line);
                }
                TokenTree::Group(g) => {
                    // Recurse into nested groups (rsx! bodies are mostly braced).
                    self.scan_tokens_for_asset(&g.stream());
                }
                _ => {}
            }
        }
    }
}

fn first_string_literal(tokens: &proc_macro2::TokenStream) -> Option<String> {
    for tt in tokens.clone() {
        if let TokenTree::Literal(lit) = tt {
            let s = lit.to_string();
            if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
                return Some(s[1..s.len() - 1].to_string());
            }
        }
    }
    None
}

fn to_forward_slash(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}
