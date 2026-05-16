use std::path::PathBuf;
use std::sync::Arc;

use heck::ToPascalCase;

use crate::state::State;

use super::discovery::{find_routable, has_derive, variant_route_path};
use super::types::{CreateRouteParams, ScaffoldResult};

pub async fn create_route(
    state: &Arc<State>,
    p: CreateRouteParams,
) -> Result<ScaffoldResult, String> {
    let crate_root = super::crate_root(state, p.project_root.as_deref()).await?;
    let router_file = match p.router_file.as_deref() {
        Some(rf) => crate_root.join(rf),
        None => find_routable(&crate_root).ok_or_else(|| {
            "could not find a Routable enum in src/; pass router_file".to_string()
        })?,
    };

    let src = std::fs::read_to_string(&router_file)
        .map_err(|e| format!("read {}: {e}", router_file.display()))?;
    let variant_name = p.component.to_pascal_case();
    let mut next_steps = vec![
        format!("ensure `{variant_name}` exists and is in scope at the routable enum"),
        "consider running `cargo fmt` on the router file".into(),
    ];
    match plan_route_insertion(&src, &variant_name, &p.path, &p.params)? {
        RouteInsertion::AlreadyMatches => {
            // Variant already wired — but the import may still be missing
            // (e.g. a route variant was hand-added without an accompanying
            // use statement). Ensure-import is idempotent.
            let mut files_modified: Vec<PathBuf> = Vec::new();
            if let Some(prefix) = p.import_path.as_deref()
                && let Some(new_src) = ensure_use_statement(&src, prefix, &variant_name)
            {
                std::fs::write(&router_file, &new_src).map_err(|e| e.to_string())?;
                files_modified.push(router_file.clone());
                next_steps.insert(
                    0,
                    format!("added `use {prefix}::{variant_name};` to the router file"),
                );
            }
            // Drop the manual scope-check hint when the import is now in
            // place — leaving it would mislead.
            if p.import_path.is_some() {
                next_steps
                    .retain(|s| !s.starts_with("ensure `") || !s.contains("is in scope at the"));
            }
            Ok(ScaffoldResult {
                files_modified,
                next_steps,
                ..Default::default()
            })
        }
        RouteInsertion::Insert { new_src, line } => {
            // Splice the use-statement in before writing so the file lands
            // self-consistent in one shot.
            let final_src = match p.import_path.as_deref() {
                Some(prefix) => {
                    ensure_use_statement(&new_src, prefix, &variant_name).unwrap_or(new_src.clone())
                }
                None => new_src,
            };
            std::fs::write(&router_file, &final_src).map_err(|e| e.to_string())?;
            let rel = router_file
                .strip_prefix(&crate_root)
                .unwrap_or(&router_file)
                .display()
                .to_string();
            next_steps.insert(
                0,
                format!("inserted `{variant_name}` route variant at `{rel}:{line}`"),
            );
            if let Some(prefix) = p.import_path.as_deref() {
                // Detect whether the final source contains the use clause we
                // wanted to add. If it does, mention it; if it doesn't (the
                // file already had a glob or specific import), keep quiet.
                let needle = format!("use {prefix}::{variant_name};");
                let glob = format!("use {prefix}::*;");
                if final_src.contains(&needle) && !src.contains(&needle) {
                    next_steps.insert(
                        1,
                        format!("added `{needle}` to `{rel}` so Routable resolves the variant"),
                    );
                }
                // Drop the now-redundant "ensure in scope" hint whenever the
                // file ends up with a workable import.
                if final_src.contains(&needle) || final_src.contains(&glob) {
                    next_steps.retain(|s| {
                        !(s.starts_with("ensure `") && s.contains("is in scope at the"))
                    });
                }
            }
            Ok(ScaffoldResult {
                files_modified: vec![router_file.clone()],
                next_steps,
                ..Default::default()
            })
        }
    }
}

/// Add `use {prefix}::{name};` to `src` if neither the specific import nor a
/// matching `use {prefix}::*;` glob is already present. Returns the modified
/// source, or `None` when the file already imports the symbol (idempotent).
///
/// The new statement lands directly after the existing run of `use` lines at
/// the top of the file, preserving header doc comments / inner attributes /
/// blank lines. If no `use` statements exist, the new line is inserted after
/// the leading attribute block (and any leading blank lines).
pub fn ensure_use_statement(src: &str, prefix: &str, name: &str) -> Option<String> {
    let target = format!("use {prefix}::{name};");
    let glob = format!("use {prefix}::*;");
    if src.contains(&target) || src.contains(&glob) {
        return None;
    }
    // Also handle the grouped form: `use crate::components::{A, B};` —
    // check the whole-file source naively for `use {prefix}::{` then for
    // `{name}` inside that brace group, splitting on `};` so we stop at the
    // end of the use statement.
    let grouped_open = format!("use {prefix}::{{");
    if let Some(pos) = src.find(&grouped_open) {
        let rest = &src[pos + grouped_open.len()..];
        if let Some(end) = rest.find("};") {
            let inner = &rest[..end];
            if inner.split(',').map(|s| s.trim()).any(|s| s == name) {
                return None;
            }
        }
    }

    let lines: Vec<&str> = src.split_inclusive('\n').collect();
    // Find the index of the last leading `use ...;` line (consecutive run at
    // the top of the file, allowing for blank lines / leading attributes).
    let mut last_use: Option<usize> = None;
    let mut header_end: usize = 0;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        if t.starts_with("//") || t.starts_with("#![") || t.starts_with("#[") || t.is_empty() {
            header_end = i + 1;
            continue;
        }
        if t.starts_with("use ") {
            last_use = Some(i);
            header_end = i + 1;
            continue;
        }
        break;
    }
    let insert_at = last_use.map(|i| i + 1).unwrap_or(header_end);
    let mut out = String::with_capacity(src.len() + target.len() + 1);
    for (i, line) in lines.iter().enumerate() {
        if i == insert_at {
            out.push_str(&target);
            out.push('\n');
        }
        out.push_str(line);
    }
    if insert_at >= lines.len() {
        // Inserting past the end (file has no trailing newline / very short).
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&target);
        out.push('\n');
    }
    Some(out)
}

#[cfg_attr(test, derive(Debug))]
enum RouteInsertion {
    /// The variant already exists and points at the same path. No-op.
    AlreadyMatches,
    /// Variant doesn't exist; this is the new source with the variant inserted.
    /// `line` is the 1-based line number of the inserted `#[route(...)]`
    /// attribute in `new_src` — surfaced in `next_steps` so callers can jump
    /// straight to the new variant in the routable enum.
    Insert { new_src: String, line: usize },
}

/// Inspect `src` for a `#[derive(Routable)]` enum and decide what to do for
/// `(variant_name, path)`:
/// - If a variant with the same name already maps to the same path → no-op.
/// - If a variant with the same name maps to a different path → conflict error.
/// - Otherwise → return the source with the variant inserted before the enum's
///   closing brace.
fn plan_route_insertion(
    src: &str,
    variant_name: &str,
    path: &str,
    params: &[(String, String)],
) -> Result<RouteInsertion, String> {
    let file = syn::parse_file(src).map_err(|e| format!("parse: {e}"))?;
    let routable = file
        .items
        .iter()
        .find_map(|it| match it {
            syn::Item::Enum(e) if e.attrs.iter().any(|a| has_derive(a, "Routable")) => Some(e),
            _ => None,
        })
        .ok_or_else(|| "no `#[derive(Routable)]` enum in source".to_string())?;
    let enum_name = routable.ident.to_string();

    for v in &routable.variants {
        if v.ident == variant_name {
            let existing_path = variant_route_path(v);
            return match existing_path {
                Some(p) if p == path => Ok(RouteInsertion::AlreadyMatches),
                Some(p) => Err(format!(
                    "route conflict: variant {variant_name} already maps to {p:?}, not {path:?}"
                )),
                None => Err(format!(
                    "variant {variant_name} already exists in {enum_name} but has no #[route(\"...\")] attribute"
                )),
            };
        }
        // Same path under a different variant name — Dioxus's Routable would
        // route the first match and silently shadow the second. Surface it
        // here so the user picks one.
        if let Some(p) = variant_route_path(v)
            && p == path
        {
            return Err(format!(
                "route conflict: path {path:?} is already mapped by variant {} in {enum_name}; \
                 rename one or change the path before re-running",
                v.ident
            ));
        }
    }

    let fields = if params.is_empty() {
        String::new()
    } else {
        let inner = params
            .iter()
            .map(|(n, t)| format!("{n}: {t}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(" {inner} ")
    };
    let variant = format!("    #[route(\"{path}\")]\n    {variant_name} {{{fields}}},\n");
    let needle_open = format!("enum {enum_name}");
    let Some(start) = src.find(&needle_open) else {
        return Err(format!("could not locate `enum {enum_name}` in source"));
    };
    let after_open = src[start..]
        .find('{')
        .map(|i| start + i + 1)
        .ok_or_else(|| "malformed enum".to_string())?;
    let mut depth = 1;
    let mut end = after_open;
    for (i, ch) in src[after_open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = after_open + i;
                    break;
                }
            }
            _ => {}
        }
    }
    let mut new_src = String::with_capacity(src.len() + variant.len());
    new_src.push_str(&src[..end]);
    if !src[..end].ends_with('\n') {
        new_src.push('\n');
    }
    // 1-based line where the inserted `#[route(...)]` attribute now lives in
    // `new_src`. Counting newlines in the prefix and adding 1 gives the line
    // number of the first character we're about to append.
    let line = new_src.bytes().filter(|&b| b == b'\n').count() + 1;
    new_src.push_str(&variant);
    new_src.push_str(&src[end..]);
    Ok(RouteInsertion::Insert { new_src, line })
}

#[cfg(test)]
mod plan_route_tests {
    use super::{RouteInsertion, plan_route_insertion};

    const BASE: &str = r#"use dioxus::prelude::*;

#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[route("/")]
    Home {},
    #[route("/users/:id")]
    User { id: i32 },
}
"#;

    #[test]
    fn inserts_new_variant() {
        let r = plan_route_insertion(BASE, "About", "/about", &[]).unwrap();
        match r {
            RouteInsertion::Insert { new_src, line } => {
                assert!(new_src.contains("#[route(\"/about\")]"));
                assert!(new_src.contains("About {}"));
                assert!(new_src.contains("Home {}"));
                assert!(new_src.contains("User { id: i32 }"));
                // The reported line should point at the `#[route("/about")]`
                // attribute of the inserted variant.
                let lines: Vec<&str> = new_src.lines().collect();
                assert_eq!(
                    lines.get(line - 1).copied(),
                    Some("    #[route(\"/about\")]"),
                    "line {line} should be the inserted #[route(...)], got: {:?}",
                    lines.get(line - 1)
                );
            }
            _ => panic!("expected Insert"),
        }
    }

    #[test]
    fn inserts_new_variant_with_params() {
        let r = plan_route_insertion(
            BASE,
            "EditUser",
            "/users/:id/edit",
            &[("id".into(), "i64".into())],
        )
        .unwrap();
        match r {
            RouteInsertion::Insert { new_src, .. } => {
                assert!(new_src.contains("#[route(\"/users/:id/edit\")]"));
                assert!(
                    new_src.contains("EditUser { id: i64 }"),
                    "expected variant with id field, got:\n{new_src}"
                );
            }
            _ => panic!("expected Insert"),
        }
    }

    #[test]
    fn skips_existing_variant_same_path() {
        let r = plan_route_insertion(BASE, "Home", "/", &[]).unwrap();
        assert!(matches!(r, RouteInsertion::AlreadyMatches));
    }

    #[test]
    fn errors_on_existing_variant_different_path() {
        let err = plan_route_insertion(BASE, "Home", "/landing", &[]).unwrap_err();
        assert!(err.contains("route conflict"), "got: {err}");
        assert!(err.contains("Home"));
    }

    #[test]
    fn errors_without_routable_enum() {
        let src = "pub enum NotRoutable { Foo, Bar }";
        let err = plan_route_insertion(src, "Foo", "/foo", &[]).unwrap_err();
        assert!(err.contains("Routable"));
    }

    #[test]
    fn errors_on_path_collision_with_different_variant() {
        // A new variant `Landing` at `/` collides with the existing
        // `Home {}` at `/`. The variant name is fresh so a name-only check
        // wouldn't catch it — the path-collision check should.
        let err = plan_route_insertion(BASE, "Landing", "/", &[]).unwrap_err();
        assert!(err.contains("route conflict"), "got: {err}");
        assert!(
            err.contains("Home"),
            "should name the colliding variant, got: {err}"
        );
        assert!(
            err.contains("\"/\""),
            "should quote the colliding path, got: {err}"
        );
    }
}

#[cfg(test)]
mod ensure_use_tests {
    use super::ensure_use_statement;

    #[test]
    fn adds_use_when_absent() {
        let src =
            "use dioxus::prelude::*;\n\n#[derive(Routable, Clone, PartialEq)]\npub enum Route {}\n";
        let out = ensure_use_statement(src, "crate::components", "Home").unwrap();
        assert!(out.contains("use crate::components::Home;"));
        // Inserted after the existing `use` line, not at the top.
        let prelude_at = out.find("use dioxus::prelude::*;").unwrap();
        let home_at = out.find("use crate::components::Home;").unwrap();
        assert!(
            home_at > prelude_at,
            "expected Home use after prelude use, got:\n{out}"
        );
    }

    #[test]
    fn idempotent_when_already_imported() {
        let src = "use crate::components::Home;\n\npub enum Route {}\n";
        assert!(ensure_use_statement(src, "crate::components", "Home").is_none());
    }

    #[test]
    fn idempotent_when_glob_already_present() {
        let src = "use crate::components::*;\n\npub enum Route {}\n";
        assert!(ensure_use_statement(src, "crate::components", "Home").is_none());
    }

    #[test]
    fn idempotent_when_grouped_import_includes_name() {
        let src = "use crate::components::{Home, About};\n\npub enum Route {}\n";
        assert!(ensure_use_statement(src, "crate::components", "Home").is_none());
        assert!(ensure_use_statement(src, "crate::components", "About").is_none());
        // A name outside the group must still trigger insertion.
        let out = ensure_use_statement(src, "crate::components", "Contact").unwrap();
        assert!(out.contains("use crate::components::Contact;"));
    }

    #[test]
    fn inserts_after_last_use_in_main_rs_shape() {
        // Mimics the `dx new` starter shape: doc comment + multiple uses +
        // a top-level main fn. The new line must land in the use block.
        let src = "//! starter\n\nuse dioxus::prelude::*;\nuse server_fn::client::reqwest::ReqwestClient;\n\n#[derive(Routable, Clone, PartialEq)]\npub enum Route {}\n";
        let out = ensure_use_statement(src, "crate::components", "Hero").unwrap();
        let lines: Vec<&str> = out.lines().collect();
        // Find the new line and check it sits inside the use block.
        let hero_idx = lines
            .iter()
            .position(|l| l.contains("use crate::components::Hero;"))
            .unwrap();
        let enum_idx = lines
            .iter()
            .position(|l| l.contains("#[derive(Routable"))
            .unwrap();
        assert!(hero_idx < enum_idx, "Hero must precede the enum:\n{out}");
        // No accidental indentation.
        assert_eq!(lines[hero_idx], "use crate::components::Hero;");
    }
}
