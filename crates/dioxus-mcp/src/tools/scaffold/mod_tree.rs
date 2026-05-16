use std::collections::BTreeMap;
use std::path::Path;

use super::types::ModUpsert;

/// Insert `pub mod {name}; pub use {name}::*;` into `mod_rs`, keeping all
/// `pub mod` / `pub use` entries sorted by name. Any non-entry lines (comments,
/// hand-written re-exports, etc.) are preserved verbatim at the top of the file.
///
/// When `allow_unused` is true, the file (whether newly created or being
/// rewritten) carries an `#![allow(unused_imports)]` shield: the blanket
/// `pub use foo::*;` re-export pattern routinely flags as `unused_imports`
/// when one of the synthesized items (e.g. a delete_* server fn) isn't called
/// by anything yet. Set to false for `src/components/mod.rs` where every
/// re-export is a real component the user will reference by name.
///
/// When `cfg_attr` is `Some(attr)`, each emitted `pub mod` / `pub use` line is
/// prefixed with that attribute on its own line — used for `src/state/mod.rs`
/// because store files are themselves `#![cfg(feature = "server")]` and the
/// module declarations need the same gate to not break the wasm build.
pub fn upsert_mod_entry(
    mod_rs: &Path,
    name: &str,
    cfg_attr: Option<&str>,
    allow_unused: bool,
) -> Result<ModUpsert, String> {
    if !mod_rs.exists() {
        let mut body = String::new();
        if allow_unused {
            body.push_str("#![allow(unused_imports)]\n");
        }
        for line in [format!("pub mod {name};"), format!("pub use {name}::*;")] {
            if let Some(cfg) = cfg_attr {
                body.push_str(cfg);
                body.push('\n');
            }
            body.push_str(&line);
            body.push('\n');
        }
        std::fs::write(mod_rs, body).map_err(|e| e.to_string())?;
        return Ok(ModUpsert::Created);
    }

    let current = std::fs::read_to_string(mod_rs).map_err(|e| e.to_string())?;
    let mut header: Vec<String> = Vec::new();
    let mut entries: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut header_done = false;
    let mut had_allow_unused = false;
    for raw in current.lines() {
        let line = raw.trim();
        if line == "#![allow(unused_imports)]" {
            had_allow_unused = true;
            continue;
        }
        // Drop outer cfg attributes — we re-emit them uniformly from cfg_attr.
        if line.starts_with("#[cfg(") {
            header_done = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix("pub mod ")
            && let Some(n) = rest.strip_suffix(';')
        {
            header_done = true;
            entries.entry(n.to_string()).or_default().push(raw.into());
            continue;
        }
        if let Some(rest) = line.strip_prefix("pub use ")
            && let Some(n) = rest.strip_suffix("::*;")
        {
            header_done = true;
            entries.entry(n.to_string()).or_default().push(raw.into());
            continue;
        }
        if !header_done {
            header.push(raw.into());
        }
        // Any non-entry line *after* entries started is dropped — we don't want
        // to scatter free-form comments through a sorted block. If a user has
        // such comments they should sit above the first entry.
    }

    entries
        .entry(name.to_string())
        .or_insert_with(|| vec![format!("pub mod {name};"), format!("pub use {name}::*;")]);

    let mut rebuilt = String::new();
    // The caller's `allow_unused` is authoritative: pass true to add the
    // attribute (or keep it if already there), pass false to drop it. This
    // lets callers (e.g. components/mod.rs) clean up the attribute from
    // previously-generated files on the next scaffold write.
    let _ = had_allow_unused;
    if allow_unused {
        rebuilt.push_str("#![allow(unused_imports)]\n");
    }
    for h in &header {
        rebuilt.push_str(h);
        rebuilt.push('\n');
    }
    for lines in entries.values() {
        for l in lines {
            if let Some(cfg) = cfg_attr {
                rebuilt.push_str(cfg);
                rebuilt.push('\n');
            }
            rebuilt.push_str(l);
            rebuilt.push('\n');
        }
    }

    if rebuilt == current {
        return Ok(ModUpsert::Unchanged);
    }
    std::fs::write(mod_rs, rebuilt).map_err(|e| e.to_string())?;
    Ok(ModUpsert::Modified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn creates_when_missing() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("mod.rs");
        let r = upsert_mod_entry(&p, "foo", None, true).unwrap();
        assert_eq!(r, ModUpsert::Created);
        let body = std::fs::read_to_string(&p).unwrap();
        // With `allow_unused: true`, freshly-created mod.rs files carry an
        // `#![allow(unused_imports)]` shield so that wildcard re-exports of
        // as-yet-uncalled items (e.g. delete_* server fns generated alongside
        // their list/get siblings) don't trip `cargo check` warnings while
        // iterating.
        assert_eq!(
            body,
            "#![allow(unused_imports)]\npub mod foo;\npub use foo::*;\n"
        );
    }

    #[test]
    fn creates_without_allow_unused() {
        // `src/components/mod.rs` passes `allow_unused: false` because every
        // re-exported item is a real component the user will reference by
        // name — no wildcard footgun to shield against.
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("mod.rs");
        let r = upsert_mod_entry(&p, "foo", None, false).unwrap();
        assert_eq!(r, ModUpsert::Created);
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(body, "pub mod foo;\npub use foo::*;\n");
    }

    #[test]
    fn strips_existing_allow_unused_when_disabled() {
        // If a previously-generated mod.rs carries the attribute but the
        // current caller passes `allow_unused: false`, we clean it up — the
        // caller's directive is authoritative.
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("mod.rs");
        std::fs::write(
            &p,
            "#![allow(unused_imports)]\npub mod alpha;\npub use alpha::*;\n",
        )
        .unwrap();
        let r = upsert_mod_entry(&p, "beta", None, false).unwrap();
        assert_eq!(r, ModUpsert::Modified);
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(
            body,
            "pub mod alpha;\npub use alpha::*;\npub mod beta;\npub use beta::*;\n"
        );
    }

    #[test]
    fn inserts_sorted_into_existing() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("mod.rs");
        std::fs::write(
            &p,
            "pub mod alpha;\npub use alpha::*;\npub mod zeta;\npub use zeta::*;\n",
        )
        .unwrap();
        let r = upsert_mod_entry(&p, "mid", None, false).unwrap();
        assert_eq!(r, ModUpsert::Modified);
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(
            body,
            "pub mod alpha;\npub use alpha::*;\npub mod mid;\npub use mid::*;\npub mod zeta;\npub use zeta::*;\n"
        );
    }

    #[test]
    fn resorts_an_out_of_order_file() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("mod.rs");
        std::fs::write(
            &p,
            "pub mod zeta;\npub use zeta::*;\npub mod alpha;\npub use alpha::*;\n",
        )
        .unwrap();
        let r = upsert_mod_entry(&p, "alpha", None, false).unwrap();
        assert_eq!(r, ModUpsert::Modified);
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(
            body,
            "pub mod alpha;\npub use alpha::*;\npub mod zeta;\npub use zeta::*;\n"
        );
    }

    #[test]
    fn idempotent_when_already_present_and_sorted() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("mod.rs");
        let initial = "pub mod alpha;\npub use alpha::*;\npub mod beta;\npub use beta::*;\n";
        std::fs::write(&p, initial).unwrap();
        let r = upsert_mod_entry(&p, "alpha", None, false).unwrap();
        assert_eq!(r, ModUpsert::Unchanged);
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(body, initial);
    }

    #[test]
    fn preserves_header_comments() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("mod.rs");
        std::fs::write(
            &p,
            "// hand-written header\n//! crate doc\npub mod zeta;\npub use zeta::*;\n",
        )
        .unwrap();
        let r = upsert_mod_entry(&p, "alpha", None, false).unwrap();
        assert_eq!(r, ModUpsert::Modified);
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(
            body,
            "// hand-written header\n//! crate doc\npub mod alpha;\npub use alpha::*;\npub mod zeta;\npub use zeta::*;\n"
        );
    }

    #[test]
    fn cfg_attr_emitted_for_fresh_file() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("mod.rs");
        let r = upsert_mod_entry(
            &p,
            "product_store",
            Some("#[cfg(feature = \"server\")]"),
            true,
        )
        .unwrap();
        assert_eq!(r, ModUpsert::Created);
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(
            body,
            "#![allow(unused_imports)]\n\
             #[cfg(feature = \"server\")]\npub mod product_store;\n\
             #[cfg(feature = \"server\")]\npub use product_store::*;\n"
        );
    }

    #[test]
    fn cfg_attr_added_to_existing_entries() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("mod.rs");
        std::fs::write(
            &p,
            "#![allow(unused_imports)]\n\
             #[cfg(feature = \"server\")]\npub mod alpha;\n\
             #[cfg(feature = \"server\")]\npub use alpha::*;\n",
        )
        .unwrap();
        let r = upsert_mod_entry(&p, "beta", Some("#[cfg(feature = \"server\")]"), true).unwrap();
        assert_eq!(r, ModUpsert::Modified);
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(
            body,
            "#![allow(unused_imports)]\n\
             #[cfg(feature = \"server\")]\npub mod alpha;\n\
             #[cfg(feature = \"server\")]\npub use alpha::*;\n\
             #[cfg(feature = \"server\")]\npub mod beta;\n\
             #[cfg(feature = \"server\")]\npub use beta::*;\n"
        );
    }
}
