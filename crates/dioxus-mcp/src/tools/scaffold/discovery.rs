use std::path::{Path, PathBuf};

/// Find the crate root .rs (src/main.rs preferred, then src/lib.rs).
pub fn find_crate_root_file(crate_root: &Path) -> Option<PathBuf> {
    for cand in &["src/main.rs", "src/lib.rs"] {
        let p = crate_root.join(cand);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Idempotently declare `pub mod {module};` in the crate root (src/main.rs or
/// src/lib.rs). Returns `Ok(Some(path))` if the file was modified, `Ok(None)`
/// if the declaration was already present, and `Ok(None)` if no crate root
/// could be located (silent no-op — callers fall back to a `next_steps` hint).
///
/// Insertion point: after the last existing `mod`/`pub mod` line if any, then
/// after the last `use` line, else at the top after inner attributes.
pub fn upsert_crate_mod(crate_root: &Path, module: &str) -> Result<Option<PathBuf>, String> {
    let Some(path) = find_crate_root_file(crate_root) else {
        return Ok(None);
    };
    let current = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;

    // Already declared (either `pub mod foo;` or `mod foo;`, with any leading
    // whitespace / attributes)?
    let needle_pub = format!("pub mod {module};");
    let needle_priv = format!("mod {module};");
    for raw in current.lines() {
        let t = raw.trim();
        if t == needle_pub || t == needle_priv {
            return Ok(None);
        }
    }

    let lines: Vec<&str> = current.lines().collect();
    // Find insertion line: after last `mod`/`pub mod` line, else after last
    // `use` line, else after the leading attribute/comment block.
    let mut insert_at = None;
    for (i, raw) in lines.iter().enumerate() {
        let t = raw.trim_start();
        if t.starts_with("mod ") || t.starts_with("pub mod ") {
            insert_at = Some(i + 1);
        }
    }
    if insert_at.is_none() {
        for (i, raw) in lines.iter().enumerate() {
            let t = raw.trim_start();
            if t.starts_with("use ") || t.starts_with("pub use ") {
                insert_at = Some(i + 1);
            }
        }
    }
    let insert_at = insert_at.unwrap_or_else(|| {
        // Skip a leading shebang, then any contiguous block of inner
        // attributes / doc comments / blank lines.
        let mut i = 0;
        if i < lines.len() && lines[i].starts_with("#!") && !lines[i].starts_with("#![") {
            i += 1;
        }
        while i < lines.len() {
            let t = lines[i].trim();
            if t.is_empty() || t.starts_with("#![") || t.starts_with("//!") || t.starts_with("//") {
                i += 1;
            } else {
                break;
            }
        }
        i
    });

    let mut new_lines: Vec<String> = lines.iter().map(|s| (*s).to_string()).collect();
    new_lines.insert(insert_at, format!("pub mod {module};"));
    let mut rebuilt = new_lines.join("\n");
    // Preserve trailing newline if the original had one.
    if current.ends_with('\n') && !rebuilt.ends_with('\n') {
        rebuilt.push('\n');
    }

    if rebuilt == current {
        return Ok(None);
    }
    std::fs::write(&path, rebuilt).map_err(|e| e.to_string())?;
    Ok(Some(path))
}

pub fn find_routable(crate_root: &Path) -> Option<PathBuf> {
    for cand in &["src/router.rs", "src/route.rs", "src/main.rs", "src/lib.rs"] {
        let p = crate_root.join(cand);
        if let Ok(s) = std::fs::read_to_string(&p)
            && s.contains("Routable")
        {
            return Some(p);
        }
    }
    // fall back: walk src/
    for entry in walkdir::WalkDir::new(crate_root.join("src"))
        .into_iter()
        .flatten()
    {
        if entry.path().extension().and_then(|x| x.to_str()) != Some("rs") {
            continue;
        }
        if let Ok(s) = std::fs::read_to_string(entry.path())
            && (s.contains("#[derive(Routable") || s.contains("derive(Routable"))
        {
            return Some(entry.path().to_path_buf());
        }
    }
    None
}

/// Extract `(variant_name, path)` pairs from every variant in the
/// `#[derive(Routable)]` enum in `router_src`. Returns an empty list when the
/// file has no Routable enum (or fails to parse) — callers should treat the
/// missing-enum case the same as "no existing routes."
pub fn existing_route_paths(router_src: &str) -> Vec<(String, String)> {
    let Ok(file) = syn::parse_file(router_src) else {
        return Vec::new();
    };
    let Some(routable) = file.items.iter().find_map(|it| match it {
        syn::Item::Enum(e) if e.attrs.iter().any(|a| has_derive(a, "Routable")) => Some(e),
        _ => None,
    }) else {
        return Vec::new();
    };
    routable
        .variants
        .iter()
        .filter_map(|v| variant_route_path(v).map(|p| (v.ident.to_string(), p)))
        .collect()
}

pub fn variant_route_path(v: &syn::Variant) -> Option<String> {
    for a in &v.attrs {
        if !a.path().is_ident("route") {
            continue;
        }
        if let Ok(lit) = a.parse_args::<syn::LitStr>() {
            return Some(lit.value());
        }
    }
    None
}

pub fn has_derive(attr: &syn::Attribute, target: &str) -> bool {
    if !attr.path().is_ident("derive") {
        return false;
    }
    let mut found = false;
    let _ = attr.parse_nested_meta(|m| {
        if m.path.is_ident(target) {
            found = true;
        }
        Ok(())
    });
    found
}
