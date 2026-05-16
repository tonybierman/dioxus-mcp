use std::path::Path;

pub(super) enum SerdePatch {
    AlreadyOk,
    Patched(std::path::PathBuf),
    PresentWithoutDeriveFeature,
    NoCargoToml,
}

/// Check whether the crate's Cargo.toml already pulls in `serde` with the
/// `derive` feature. If not present at all, append a serde dep line under
/// `[dependencies]`. If present without the derive feature, return a marker so
/// the caller can emit a manual-fix hint (re-writing an existing dep table
/// entry risks clobbering other settings the user authored).
pub(super) fn ensure_serde_in_cargo_toml(crate_root: &Path) -> Result<SerdePatch, String> {
    let path = crate_root.join("Cargo.toml");
    if !path.exists() {
        return Ok(SerdePatch::NoCargoToml);
    }
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let parsed: toml::Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;

    let serde_value = parsed
        .get("dependencies")
        .and_then(|d| d.as_table())
        .and_then(|t| t.get("serde"));
    match serde_value {
        Some(v) => {
            // Either a bare version string (no features) or a table — both need
            // a `derive` feature for `#[derive(Serialize, Deserialize)]`.
            let has_derive = v
                .as_table()
                .and_then(|t| t.get("features"))
                .and_then(|f| f.as_array())
                .map(|arr| arr.iter().any(|x| x.as_str() == Some("derive")))
                .unwrap_or(false);
            if has_derive {
                Ok(SerdePatch::AlreadyOk)
            } else {
                Ok(SerdePatch::PresentWithoutDeriveFeature)
            }
        }
        None => {
            let new_text = append_dep_to_cargo_toml(
                &text,
                "serde",
                r#"serde = { version = "1", features = ["derive"] }"#,
            )?;
            std::fs::write(&path, new_text).map_err(|e| e.to_string())?;
            Ok(SerdePatch::Patched(path))
        }
    }
}

pub(super) enum DioxusRouterPatch {
    /// `dioxus` already has either `router` or `fullstack` in its features
    /// array — fullstack pulls router in transitively in Dioxus 0.7.
    AlreadyOk,
    Patched(std::path::PathBuf),
    /// `dioxus` is declared as a bare version string (e.g. `dioxus = "0.7"`),
    /// so we have nowhere to insert a features array without rewriting the
    /// user's line. Hint instead.
    DioxusNotATable,
    DioxusMissing,
    NoCargoToml,
}

/// Add `"router"` to the `dioxus` dep's `features` array when any Screen /
/// LoginScreen has been declared in the doc. Parity with
/// [`ensure_serde_in_cargo_toml`] — same shape, same idempotency contract.
///
/// We only edit the line in place when `dioxus` is already a table with a
/// `features = [...]` array; a bare-version `dioxus = "0.7"` is left alone
/// (the caller surfaces a hint).
pub(super) fn ensure_dioxus_router_in_cargo_toml(
    crate_root: &Path,
) -> Result<DioxusRouterPatch, String> {
    let path = crate_root.join("Cargo.toml");
    if !path.exists() {
        return Ok(DioxusRouterPatch::NoCargoToml);
    }
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let parsed: toml::Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;

    let dx = parsed
        .get("dependencies")
        .and_then(|d| d.as_table())
        .and_then(|t| t.get("dioxus"));
    let Some(dx) = dx else {
        return Ok(DioxusRouterPatch::DioxusMissing);
    };
    let Some(dx_table) = dx.as_table() else {
        return Ok(DioxusRouterPatch::DioxusNotATable);
    };
    let features = dx_table
        .get("features")
        .and_then(|f| f.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if features.iter().any(|f| f == "router" || f == "fullstack") {
        return Ok(DioxusRouterPatch::AlreadyOk);
    }

    let new_text = inject_router_feature(&text)?;
    if new_text == text {
        // Nothing matched — line we expected to patch wasn't in the format
        // we know how to rewrite. Treat as "not a table" so caller hints.
        return Ok(DioxusRouterPatch::DioxusNotATable);
    }
    std::fs::write(&path, new_text).map_err(|e| e.to_string())?;
    Ok(DioxusRouterPatch::Patched(path))
}

/// Find the `dioxus = { ... features = [...] ... }` line in raw Cargo.toml
/// text and append `"router"` to that inline features array. Operating on
/// the textual representation (rather than re-serializing the parsed `toml`)
/// preserves the user's comments, key order, and quoting style.
pub(super) fn inject_router_feature(text: &str) -> Result<String, String> {
    let mut out = String::with_capacity(text.len() + 16);
    let mut patched = false;
    let mut in_deps = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_deps = trimmed == "[dependencies]";
        }
        if !patched
            && in_deps
            && let Some(rest) = trimmed.strip_prefix("dioxus")
            && rest.trim_start().starts_with('=')
        {
            // Look for an inline `features = [ ... ]` on this line and append
            // `"router"` to it. If features isn't on this line, leave it
            // alone — the table is presumably split across multiple lines or
            // uses a sub-table, and a textual patch isn't safe.
            if let Some(feat_start) = line.find("features") {
                let after = &line[feat_start..];
                if let Some(open) = after.find('[')
                    && let Some(close_rel) = after[open..].find(']')
                {
                    let close = feat_start + open + close_rel;
                    let inner_start = feat_start + open + 1;
                    let inner = &line[inner_start..close];
                    let inner_trim = inner.trim();
                    let new_inner = if inner_trim.is_empty() {
                        "\"router\"".to_string()
                    } else {
                        format!("{}, \"router\"", inner.trim_end())
                    };
                    let mut new_line = String::new();
                    new_line.push_str(&line[..inner_start]);
                    new_line.push_str(&new_inner);
                    new_line.push_str(&line[close..]);
                    out.push_str(&new_line);
                    out.push('\n');
                    patched = true;
                    continue;
                }
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    // Preserve original trailing-newline state.
    if !text.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }
    if !patched {
        return Ok(text.to_string());
    }
    Ok(out)
}

/// Append a new dep line into an existing `[dependencies]` table (or create
/// the table at the end of the file if it doesn't exist). Preserves the
/// user's existing formatting elsewhere — we only inject a single new line.
pub(super) fn append_dep_to_cargo_toml(
    text: &str,
    dep_name: &str,
    line: &str,
) -> Result<String, String> {
    let lines: Vec<&str> = text.lines().collect();
    // Find the `[dependencies]` header; only the literal `[dependencies]` table
    // (not `[dependencies.foo]` sub-tables, which write a single dep each).
    let header_idx = lines.iter().position(|l| l.trim() == "[dependencies]");
    if let Some(idx) = header_idx {
        // Insert right after the header (top of the table block).
        let mut new_lines: Vec<String> = lines.iter().map(|s| (*s).to_string()).collect();
        // Skip past contiguous blank lines just after the header to land below
        // any header-attached blank line.
        let mut insert_at = idx + 1;
        while insert_at < new_lines.len() && new_lines[insert_at].trim().is_empty() {
            insert_at += 1;
        }
        new_lines.insert(insert_at, line.to_string());
        let mut out = new_lines.join("\n");
        if text.ends_with('\n') && !out.ends_with('\n') {
            out.push('\n');
        }
        Ok(out)
    } else {
        // No [dependencies] section at all — append one.
        let mut out = text.to_string();
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("\n[dependencies]\n");
        out.push_str(line);
        out.push('\n');
        let _ = dep_name;
        Ok(out)
    }
}
