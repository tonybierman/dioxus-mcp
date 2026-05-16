use std::collections::BTreeSet;
use std::path::Path;

use heck::ToSnakeCase;

use crate::tools::scaffold::ScaffoldResult;

use super::resources::SynthServerFn;
use super::types::*;

/// Order-preserving dedup. `files_modified` in particular accumulates one
/// entry per route/component insertion (e.g. src/main.rs and src/components/mod.rs
/// show up dozens of times in a resource scaffold); deduping keeps the response
/// scannable.
pub(super) fn dedup_paths(v: &mut Vec<std::path::PathBuf>) {
    let mut seen: BTreeSet<std::path::PathBuf> = BTreeSet::new();
    v.retain(|p| seen.insert(p.clone()));
}

/// Return the unique set of top-level src/{module}/ subdirs that received at
/// least one emitted file. Used to drive crate-root `pub mod` injection.
pub(super) fn top_level_modules_touched(result: &ScaffoldResult, crate_root: &Path) -> Vec<String> {
    let src = crate_root.join("src");
    let mut out: BTreeSet<String> = BTreeSet::new();
    let scan = |paths: &Vec<std::path::PathBuf>, out: &mut BTreeSet<String>| {
        for p in paths {
            let Ok(rel) = p.strip_prefix(&src) else {
                continue;
            };
            let mut comps = rel.components();
            let Some(first) = comps.next() else { continue };
            // Only count entries that are *inside* a subdir (i.e. there's
            // another component after the first) — a bare `src/main.rs` edit
            // isn't a module subdir.
            if comps.next().is_none() {
                continue;
            }
            if let std::path::Component::Normal(name) = first
                && let Some(n) = name.to_str()
            {
                out.insert(n.to_string());
            }
        }
    };
    scan(&result.files_created, &mut out);
    scan(&result.files_modified, &mut out);
    out.into_iter().collect()
}

pub(super) fn has_extra_documents(yaml: &str) -> bool {
    // A leading "---" is a valid single-document marker; multiple "---" lines
    // (or any "---" after non-whitespace content) means multi-document.
    let mut seen_content = false;
    for line in yaml.lines() {
        let trimmed = line.trim_end();
        if trimmed == "---" {
            if seen_content {
                return true;
            }
        } else if !line.trim().is_empty() && !line.trim_start().starts_with('#') {
            seen_content = true;
        }
    }
    false
}

pub(super) fn merge(into: &mut ScaffoldResult, from: ScaffoldResult) {
    into.files_created.extend(from.files_created);
    into.files_modified.extend(from.files_modified);
    into.next_steps.extend(from.next_steps);
    into.collisions.extend(from.collisions);
    into.would_create.extend(from.would_create);
    into.would_modify.extend(from.would_modify);
}

pub(super) fn leaf_for(crate_root: &Path, subdir: &str, name: &str) -> std::path::PathBuf {
    let snake = name.to_snake_case();
    crate_root.join(subdir).join(format!("{snake}.rs"))
}

/// If `target` is in the skip set, record it as a collision and return true.
pub(super) fn skip_or_record(
    skip: &BTreeSet<std::path::PathBuf>,
    result: &mut ScaffoldResult,
    target: std::path::PathBuf,
) -> bool {
    if skip.contains(&target) {
        result.collisions.push(target);
        true
    } else {
        false
    }
}

/// Walk the doc and return the set of leaf files that already exist on disk —
/// the primitives whose target file should be skipped in `if_missing` mode.
pub(super) fn skip_set(
    doc: &DslDoc,
    synth_server_fns: &[SynthServerFn],
    crate_root: &Path,
) -> BTreeSet<std::path::PathBuf> {
    let mut s = BTreeSet::new();
    let mut maybe_add = |subdir: &str, name: &str| {
        let p = leaf_for(crate_root, subdir, name);
        if p.exists() {
            s.insert(p);
        }
    };
    for c in &doc.components {
        maybe_add("src/components", &c.name);
    }
    for f in &doc.forms {
        maybe_add("src/components", &f.name);
    }
    for l in &doc.lists {
        maybe_add("src/components", &l.name);
    }
    for t in &doc.tables {
        maybe_add("src/components", &t.name);
    }
    for f in &doc.feeds {
        maybe_add("src/components", &f.name);
    }
    for ls in &doc.login_screens {
        maybe_add("src/components", &ls.name);
    }
    for pr in &doc.protected_routes {
        maybe_add("src/components", &pr.name);
    }
    for sc in &doc.screens {
        maybe_add("src/components", &sc.name);
    }
    for sf in &doc.server_fns {
        maybe_add("src/server", &sf.name);
    }
    for sig in &doc.signals {
        maybe_add("src/signals", &sig.name);
    }
    for sk in &doc.sockets {
        maybe_add("src/sockets", &sk.name);
    }
    for ss in &doc.session_states {
        maybe_add("src/auth", &ss.name);
    }
    for m in &doc.models {
        maybe_add("src/model", &m.name);
    }
    for st in &doc.stores {
        maybe_add("src/state", &st.name);
    }
    for cs in &doc.client_stores {
        maybe_add("src/state", &cs.name);
    }
    for sf in synth_server_fns {
        maybe_add("src/server", &sf.name);
    }
    s
}

pub(super) fn modify_target_path(m: &DslModify, crate_root: &Path) -> std::path::PathBuf {
    match m {
        DslModify::AddModelField { model, .. } | DslModify::RemoveModelField { model, .. } => {
            leaf_for(crate_root, "src/model", model)
        }
        DslModify::AddComponentProp { component, .. }
        | DslModify::RemoveComponentProp { component, .. } => {
            leaf_for(crate_root, "src/components", component)
        }
        DslModify::AddServerFnArg { server_fn, .. } => {
            leaf_for(crate_root, "src/server", server_fn)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_multidoc_yaml() {
        assert!(has_extra_documents("a: 1\n---\nb: 2"));
        assert!(!has_extra_documents("---\na: 1\nb: 2"));
        assert!(!has_extra_documents("# comment\na: 1"));
    }
}
