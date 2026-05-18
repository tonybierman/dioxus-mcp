use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub ast: Result<syn::File, syn::Error>,
}

#[derive(Debug, Serialize, Clone)]
pub struct ParseError {
    pub file: PathBuf,
    pub error: String,
}

pub(crate) fn walk_rs_files(root: &Path) -> Vec<ScannedFile> {
    let mut out = Vec::new();
    let walker = walkdir::WalkDir::new(root).into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        if e.depth() == 0 {
            return true;
        }
        if name.starts_with('.') {
            return false;
        }
        !matches!(name.as_ref(), "target" | "node_modules")
    });

    for entry in walker.flatten() {
        let path = entry.path();
        if !entry.file_type().is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        let ast = syn::parse_file(&source);
        out.push(ScannedFile {
            path: path.to_path_buf(),
            ast,
        });
    }
    out
}

pub(crate) fn collect_parse_errors(files: &[ScannedFile]) -> Vec<ParseError> {
    files
        .iter()
        .filter_map(|f| {
            f.ast.as_ref().err().map(|e| ParseError {
                file: f.path.clone(),
                error: e.to_string(),
            })
        })
        .collect()
}

/// Returns `true` when `path` lives under `src/components/<catalog_name>/...`
/// — i.e. the file is part of a catalog wrapper installed via
/// `dx components add`. Lints that flag "you reinvented a catalog widget"
/// must skip these paths: the wrapper file IS the widget, and emitting the
/// hand-rolled shape there is by definition correct.
pub(crate) fn is_catalog_wrapper(path: &Path, src_root: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(src_root) else {
        return false;
    };
    let mut comps = rel.components();
    let Some(first) = comps.next() else {
        return false;
    };
    if first.as_os_str() != "components" {
        return false;
    }
    let Some(second) = comps.next() else {
        return false;
    };
    let widget_name = second.as_os_str().to_string_lossy();
    crate::tools::dsl::dx_component_names().any(|n| n == widget_name)
}
