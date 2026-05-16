use std::collections::BTreeSet;
use std::path::Path;

use crate::tools::scaffold::{self, ScaffoldResult};

use super::resources::SynthServerFn;
use super::types::*;
use super::util::*;

/// Compute the would-be plan for a dry-run: for every primitive in `doc`,
/// classify its leaf file as `would_create` (path is free) or `collisions`
/// (path already exists), and classify the parent `mod.rs` plus any touched
/// router file as `would_create` / `would_modify`.
pub(super) fn plan_dsl(
    doc: &DslDoc,
    synth_server_fns: &[SynthServerFn],
    crate_root: &Path,
) -> ScaffoldResult {
    let mut out = ScaffoldResult {
        dry_run: true,
        ..Default::default()
    };
    let mut mods_touched: BTreeSet<std::path::PathBuf> = BTreeSet::new();

    let leaf = |out: &mut ScaffoldResult,
                mods_touched: &mut BTreeSet<std::path::PathBuf>,
                subdir: &str,
                name: &str| {
        let leaf_path = leaf_for(crate_root, subdir, name);
        if leaf_path.exists() {
            out.collisions.push(leaf_path);
        } else {
            out.would_create.push(leaf_path);
        }
        let mod_path = crate_root.join(subdir).join("mod.rs");
        if mods_touched.insert(mod_path.clone()) {
            if mod_path.exists() {
                out.would_modify.push(mod_path);
            } else {
                out.would_create.push(mod_path);
            }
        }
    };

    for c in &doc.components {
        leaf(&mut out, &mut mods_touched, "src/components", &c.name);
    }
    for f in &doc.forms {
        leaf(&mut out, &mut mods_touched, "src/components", &f.name);
    }
    for l in &doc.lists {
        leaf(&mut out, &mut mods_touched, "src/components", &l.name);
    }
    for t in &doc.tables {
        leaf(&mut out, &mut mods_touched, "src/components", &t.name);
    }
    for f in &doc.feeds {
        leaf(&mut out, &mut mods_touched, "src/components", &f.name);
    }
    for ls in &doc.login_screens {
        leaf(&mut out, &mut mods_touched, "src/components", &ls.name);
    }
    for pr in &doc.protected_routes {
        leaf(&mut out, &mut mods_touched, "src/components", &pr.name);
    }
    for sc in &doc.screens {
        leaf(&mut out, &mut mods_touched, "src/components", &sc.name);
    }
    for sf in &doc.server_fns {
        leaf(&mut out, &mut mods_touched, "src/server", &sf.name);
    }
    for sig in &doc.signals {
        leaf(&mut out, &mut mods_touched, "src/signals", &sig.name);
    }
    for sk in &doc.sockets {
        leaf(&mut out, &mut mods_touched, "src/sockets", &sk.name);
    }
    for ss in &doc.session_states {
        leaf(&mut out, &mut mods_touched, "src/auth", &ss.name);
    }
    for m in &doc.models {
        leaf(&mut out, &mut mods_touched, "src/model", &m.name);
    }
    for st in &doc.stores {
        leaf(&mut out, &mut mods_touched, "src/state", &st.name);
    }
    for cs in &doc.client_stores {
        leaf(&mut out, &mut mods_touched, "src/state", &cs.name);
    }
    for sf in synth_server_fns {
        leaf(&mut out, &mut mods_touched, "src/server", &sf.name);
    }

    // Router file: modified when there are routed primitives (screens or login_screens).
    if (!doc.screens.is_empty() || !doc.login_screens.is_empty())
        && let Some(router) = scaffold::find_routable(crate_root)
    {
        out.would_modify.push(router);
    }

    // `modify:` entries — classify each target as would_modify (file present)
    // or collisions (missing, would error or be skipped in if_missing mode).
    for m in &doc.modify {
        let target_path = modify_target_path(m, crate_root);
        if target_path.exists() {
            if !out.would_modify.iter().any(|p| p == &target_path) {
                out.would_modify.push(target_path);
            }
        } else {
            out.collisions.push(target_path);
        }
    }

    dedup_paths(&mut out.would_create);
    dedup_paths(&mut out.would_modify);
    dedup_paths(&mut out.collisions);
    out
}

/// Canonicalize a route-path string for collision detection. Strip a trailing
/// slash (except for the root "/") and replace any `:param` segments with a
/// `:` placeholder so `/users/:id` and `/users/:user_id` collide as the user
/// intends (same shape, different param name). Stays purely textual — no
/// `Routable`-style nesting awareness needed for pre-flight.
pub(super) fn normalize_route_path(path: &str) -> String {
    let trimmed = if path.len() > 1 {
        path.trim_end_matches('/')
    } else {
        path
    };
    trimmed
        .split('/')
        .map(|seg| if seg.starts_with(':') { ":" } else { seg })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_route_path_collapses_param_names_and_trailing_slash() {
        assert_eq!(normalize_route_path("/users"), "/users");
        assert_eq!(normalize_route_path("/users/"), "/users");
        assert_eq!(normalize_route_path("/"), "/");
        assert_eq!(normalize_route_path("/users/:id"), "/users/:");
        assert_eq!(
            normalize_route_path("/users/:user_id"),
            normalize_route_path("/users/:id")
        );
    }
}
