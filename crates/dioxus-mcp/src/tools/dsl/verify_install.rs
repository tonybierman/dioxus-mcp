//! `verify_install`: inspect a project's wiring for `dx components add` output.
//!
//! After `dx components add <name>` runs, two one-time edits must land in the
//! user's crate before the new module compiles cleanly:
//!
//!   1. `mod components;` in `src/main.rs` (or `src/lib.rs`).
//!   2. The catalog theme stylesheet mounted via
//!      `asset!("/assets/dx-components-theme.css")` — typically in the `App`
//!      component body.
//!
//! `dx` prints these reminders to stdout but agents that don't capture CLI
//! output (or that only inspect the file system) miss them. This tool reads
//! the project and reports which steps are still missing, with the exact
//! lines to add — so the agent can finish wiring without re-running `dx`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::state::State;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct VerifyInstallParams {
    /// Optional project root override. Defaults to the detected manifest dir.
    #[serde(default)]
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct VerifyInstallStep {
    /// Stable short id (`mod_components` / `theme_asset` / `components_dir`).
    /// Lets callers branch on a specific step without string-matching titles.
    pub id: &'static str,
    /// Human-readable title.
    pub title: &'static str,
    /// `true` when the step is wired correctly, `false` when action is needed.
    pub ok: bool,
    /// Where the check looked (the file or directory it inspected). When the
    /// step is `ok: true`, this is the path where the wiring was found.
    pub looked_in: Vec<PathBuf>,
    /// The line(s) to add when `ok: false`, ready to paste. Empty when `ok`.
    pub fix: Option<String>,
    /// Free-form hint about *where* to paste the fix (e.g. "top of src/main.rs",
    /// "inside the rsx! body of `App`"). Empty when `ok`.
    pub fix_location: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct VerifyInstallReport {
    /// Absolute crate root the checks ran against.
    pub project_root: PathBuf,
    /// `true` when every step is `ok` — the catalog is fully wired.
    pub fully_wired: bool,
    /// Detected archetype based on what the source tree currently uses
    /// (`basic`, `fullstack`, `fullstack-realtime`, `hand_rolled`).
    /// Determines which archetype-specific dep checks ran below.
    /// `hand_rolled` is set when the source tree shows no signal that the
    /// user is using the `dx components add` catalog (no `components/` dir,
    /// no theme css, no `mod components` declared); in that mode the three
    /// dx-wiring steps are suppressed because they aren't applicable.
    pub archetype: &'static str,
    /// Per-step results in stable order.
    pub steps: Vec<VerifyInstallStep>,
    /// Convenience: short titles of every step where `ok: false`. Lets a
    /// caller scan the report without iterating `steps`.
    pub missing: Vec<&'static str>,
}

pub async fn verify_install(
    state: &Arc<State>,
    p: VerifyInstallParams,
) -> Result<VerifyInstallReport, String> {
    let crate_root = resolve_crate_root(state, p.project_root.as_deref()).await?;
    let src = crate_root.join("src");
    let main_rs = src.join("main.rs");
    let lib_rs = src.join("lib.rs");

    let mod_step = check_mod_components(&main_rs, &lib_rs);
    let theme_step = check_theme_asset(&src);
    let dir_step = check_components_dir(&src.join("components"));

    let signals = ArchetypeSignals::scan(&src);
    let manifest_text = std::fs::read_to_string(crate_root.join("Cargo.toml")).unwrap_or_default();
    let manifest = parse_manifest(&manifest_text);

    // Hand-rolled UI archetype: none of the three dx-catalog markers are
    // present. We treat that as "the user isn't planning to use the catalog"
    // and suppress the three steps so the report doesn't flag noise. Only
    // refines the `basic` label — a fullstack app without catalog wiring
    // still wants its cookie / web-sys / tokio dep checks, so we keep those
    // archetype names intact and just drop the catalog steps.
    let no_dx_catalog = !mod_step.ok && !theme_step.ok && !dir_step.ok;
    let base_archetype = signals.archetype_label();
    let archetype = if no_dx_catalog && base_archetype == "basic" {
        "hand_rolled"
    } else {
        base_archetype
    };

    let mut steps = if no_dx_catalog {
        Vec::new()
    } else {
        let mut s = vec![mod_step];
        // theme_asset is the catalog-specific step: only require the theme
        // stylesheet when the project actually consumes a catalog widget
        // (a `use crate::components::<catalog_name>` import or a matching
        // src/components/<name>/ directory). Projects with a hand-rolled
        // `src/components/` shouldn't be nagged about a stylesheet they
        // don't need.
        if signals.uses_dx_catalog || theme_step.ok {
            s.push(theme_step);
        }
        s.push(dir_step);
        s
    };
    extend_with_archetype_steps(&mut steps, &signals, &manifest, &crate_root);

    let missing: Vec<&'static str> = steps.iter().filter(|s| !s.ok).map(|s| s.id).collect();
    let fully_wired = missing.is_empty();
    Ok(VerifyInstallReport {
        project_root: crate_root,
        fully_wired,
        archetype,
        steps,
        missing,
    })
}

/// Source-tree signals that pick out which archetype-specific dep checks run.
///
/// Each flag is detected by a cheap scan of `src/` — we look at directory
/// presence (`src/server/`, `src/sockets/`) and a substring grep over `.rs`
/// files for the canonical idiom (`TypedHeader<Cookie>`, the `Uuid` type, an
/// `asset!(` macro). Doing it this way keeps the check honest against
/// hand-authored code as well as scaffolder output.
#[derive(Debug, Default, Clone)]
struct ArchetypeSignals {
    has_server: bool,
    has_sockets: bool,
    uses_cookies: bool,
    /// True only when source code imports the bare `headers::Cookie` path
    /// (so the top-level `headers` crate is required). When the user goes
    /// through `axum_extra::headers::Cookie` (the re-export bundled with the
    /// `typed-header` feature), this stays `false`.
    uses_bare_headers_crate: bool,
    uses_uuid: bool,
    uses_assets: bool,
    /// True when the source imports a catalog widget (`use crate::components::<catalog_name>`)
    /// or `src/components/<catalog_name>/` matches a catalog entry. Drives
    /// the theme_asset / mod_components / components_dir checks — we only
    /// flag those when the project actually uses the catalog.
    uses_dx_catalog: bool,
}

impl ArchetypeSignals {
    fn scan(src_dir: &Path) -> Self {
        let mut s = ArchetypeSignals {
            has_server: src_dir.join("server").is_dir(),
            has_sockets: src_dir.join("sockets").is_dir(),
            ..Default::default()
        };

        // Catalog widget directories (`src/components/<catalog_name>/`) are
        // positive evidence the user is on the catalog path — pre-scan so we
        // can also accept this as a `uses_dx_catalog` signal without an
        // explicit `use` import.
        let components_dir = src_dir.join("components");
        let catalog_names: std::collections::BTreeSet<&'static str> =
            crate::tools::dsl::dx_components::dx_component_names().collect();
        if let Ok(entries) = std::fs::read_dir(&components_dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir()
                    && let Some(name) = entry.file_name().to_str()
                    && catalog_names.contains(name)
                {
                    s.uses_dx_catalog = true;
                    break;
                }
            }
        }

        let mut walked: Vec<PathBuf> = Vec::new();
        walk_rs_files(src_dir, &mut walked, 6);
        for path in &walked {
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            // Cookie typed-header is the only stable way to spot the cookie-
            // auth idiom across hand-rolled and scaffolded code. `axum_extra::TypedHeader`
            // is the import line; either signal is enough.
            if text.contains("TypedHeader<Cookie>") || text.contains("axum_extra::TypedHeader") {
                s.uses_cookies = true;
            }
            // `Uuid` is conservative — any `Uuid` reference (Uuid::new_v4(),
            // field type Uuid) triggers the dep check. False positives would
            // be rare (the uuid crate is the only Uuid in common use).
            if text.contains("Uuid") {
                s.uses_uuid = true;
            }
            if text.contains("asset!(") {
                s.uses_assets = true;
            }
            // Only set when the user reaches for the top-level `headers`
            // crate directly. Going through `axum_extra::headers::Cookie`
            // (the re-export the `typed-header` feature provides) does NOT
            // need a separate `headers` dep — detect that by checking the
            // character before each `headers::` occurrence isn't part of a
            // qualified path (i.e. not `axum_extra::headers::`).
            if !s.uses_bare_headers_crate && mentions_bare_headers_path(&text) {
                s.uses_bare_headers_crate = true;
            }
            // `use crate::components::<catalog_name>` — positive evidence the
            // user is consuming the catalog (rather than just having their
            // own `src/components/` for hand-rolled UI).
            if !s.uses_dx_catalog
                && let Some(after) = text.find("use crate::components::")
            {
                let tail = &text[after + "use crate::components::".len()..];
                let ident: String = tail
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if !ident.is_empty() && catalog_names.contains(ident.as_str()) {
                    s.uses_dx_catalog = true;
                }
            }
        }
        s
    }

    fn archetype_label(&self) -> &'static str {
        if self.has_sockets {
            "fullstack-realtime"
        } else if self.has_server {
            "fullstack"
        } else {
            "basic"
        }
    }
}

/// Minimal Cargo.toml dep view used by the archetype checks. We parse once at
/// the top of verify_install and pass this around so the per-step checks
/// don't each re-read the file. `features` is left lower-cased so callers can
/// compare without normalizing on each lookup.
#[derive(Debug, Default, Clone)]
struct ManifestDeps {
    deps: std::collections::BTreeMap<String, ManifestDep>,
}

#[derive(Debug, Default, Clone)]
struct ManifestDep {
    /// `true` when the dep is declared at all (in [dependencies] or any
    /// target-cfg variant). Per-target wiring is fine for us — we just want
    /// to know whether a `cargo add` is still needed.
    present: bool,
    features: Vec<String>,
}

fn parse_manifest(text: &str) -> ManifestDeps {
    let mut out = ManifestDeps::default();
    let Ok(parsed) = text.parse::<toml::Table>() else {
        return out;
    };
    // Top-level [dependencies] plus every [target.<cfg>.dependencies] table.
    // We don't distinguish between them — a wasm-only dep still counts as
    // "the project declares this dep".
    let mut dep_tables: Vec<&toml::Table> = Vec::new();
    if let Some(t) = parsed.get("dependencies").and_then(|v| v.as_table()) {
        dep_tables.push(t);
    }
    if let Some(targets) = parsed.get("target").and_then(|v| v.as_table()) {
        for (_, cfg) in targets {
            if let Some(cfg_table) = cfg.as_table()
                && let Some(t) = cfg_table.get("dependencies").and_then(|v| v.as_table())
            {
                dep_tables.push(t);
            }
        }
    }
    for t in dep_tables {
        for (name, value) in t {
            let entry = out.deps.entry(name.clone()).or_default();
            entry.present = true;
            if let Some(table) = value.as_table()
                && let Some(arr) = table.get("features").and_then(|v| v.as_array())
            {
                for f in arr {
                    if let Some(s) = f.as_str() {
                        entry.features.push(s.to_string());
                    }
                }
            }
        }
    }

    // `[features]` table can pull crate features via `crate/feat` or
    // `crate?/feat` (the latter is the optional-dep form). Fold those into
    // the dep's known feature list so we don't false-positive an
    // "uuid is missing v4" finding when v4 is enabled through a feature
    // alias (e.g. `server = ["uuid/v4"]`). We don't model which crate
    // features are *active* — most dx apps build with the union of features
    // they care about — so treat any feature-pulled entry as enabled.
    if let Some(features) = parsed.get("features").and_then(|v| v.as_table()) {
        for (_, alias_value) in features {
            let Some(alias_list) = alias_value.as_array() else {
                continue;
            };
            for entry in alias_list {
                let Some(s) = entry.as_str() else { continue };
                // Forms: `dep/feat`, `dep?/feat`, `dep:dep_alias`. We skip
                // the alias rename form and `dep:` which only signals the
                // dep is enabled (already known via [dependencies]).
                let Some((dep, feat)) = s.split_once('/') else {
                    continue;
                };
                let dep = dep.trim_end_matches('?');
                if dep.is_empty() || feat.is_empty() {
                    continue;
                }
                let dep_entry = out.deps.entry(dep.to_string()).or_default();
                dep_entry.present = true;
                if !dep_entry.features.iter().any(|x| x == feat) {
                    dep_entry.features.push(feat.to_string());
                }
            }
        }
    }
    out
}

impl ManifestDeps {
    fn has(&self, name: &str) -> bool {
        self.deps.get(name).map(|d| d.present).unwrap_or(false)
    }

    /// Returns the subset of `required` that is NOT enabled on `name`. When
    /// the dep itself is missing, every required feature is missing too.
    fn missing_features(&self, name: &str, required: &[&str]) -> Vec<String> {
        let Some(d) = self.deps.get(name) else {
            return required.iter().map(|s| s.to_string()).collect();
        };
        required
            .iter()
            .filter(|f| !d.features.iter().any(|x| x == *f))
            .map(|s| s.to_string())
            .collect()
    }
}

/// Append per-archetype dep checks based on what the source tree uses.
/// Each block is conditional on the matching signal so we don't badger
/// callers about deps they have no use for. Keep this list short and
/// archetype-driven — generic dep audits belong in `audit_feature_flags`.
fn extend_with_archetype_steps(
    steps: &mut Vec<VerifyInstallStep>,
    signals: &ArchetypeSignals,
    manifest: &ManifestDeps,
    crate_root: &Path,
) {
    let cargo_toml = crate_root.join("Cargo.toml");

    // Cookie typed-headers require `axum-extra` with the `typed-header`
    // feature. The `typed-header` feature re-exports `axum_extra::headers::Cookie`
    // (via the bundled `headers` crate), so we don't need a separate `headers`
    // dep when the user already imports `axum_extra::headers::Cookie`. Only
    // require a top-level `headers` dep when the source uses the bare
    // `headers::Cookie` path.
    if signals.uses_cookies {
        let missing = manifest.missing_features("axum-extra", &["typed-header"]);
        let headers_required = signals.uses_bare_headers_crate;
        let headers_missing = headers_required && !manifest.has("headers");
        let title = if headers_required {
            "`axum-extra` (with `typed-header`) + `headers` for TypedHeader<Cookie>"
        } else {
            "`axum-extra` (with `typed-header`) for TypedHeader<Cookie>"
        };
        if missing.is_empty() && !headers_missing {
            steps.push(VerifyInstallStep {
                id: "axum_extra_cookies",
                title,
                ok: true,
                looked_in: vec![cargo_toml.clone()],
                fix: None,
                fix_location: None,
            });
        } else {
            let mut fix = String::new();
            if !manifest.has("axum-extra") || !missing.is_empty() {
                fix.push_str(
                    "axum-extra = { version = \"0.10\", features = [\"typed-header\"] }\n",
                );
            }
            if headers_missing {
                fix.push_str("headers = \"0.4\"\n");
            }
            steps.push(VerifyInstallStep {
                id: "axum_extra_cookies",
                title,
                ok: false,
                looked_in: vec![cargo_toml.clone()],
                fix: Some(fix.trim_end().to_string()),
                fix_location: Some(
                    "[dependencies] table in Cargo.toml — guard with `[target.'cfg(not(target_arch = \"wasm32\"))'.dependencies]` if you only need them server-side"
                        .into(),
                ),
            });
        }
    }

    // src/sockets/ ⇒ WebSocket realtime. The generated socket code uses
    // web_sys::WebSocket on the wasm side and tokio::sync::broadcast on the
    // server side. Both sets of deps are required.
    if signals.has_sockets {
        let websys_missing = manifest.missing_features(
            "web-sys",
            &["WebSocket", "MessageEvent", "BinaryType", "ErrorEvent"],
        );
        let has_wasm_bindgen = manifest.has("wasm-bindgen");
        if websys_missing.is_empty() && has_wasm_bindgen {
            steps.push(VerifyInstallStep {
                id: "realtime_wasm_deps",
                title: "`web-sys` (WebSocket features) + `wasm-bindgen` for socket clients",
                ok: true,
                looked_in: vec![cargo_toml.clone()],
                fix: None,
                fix_location: None,
            });
        } else {
            let mut fix = String::new();
            if !manifest.has("web-sys") || !websys_missing.is_empty() {
                fix.push_str(
                    "web-sys = { version = \"0.3\", features = [\"WebSocket\", \"MessageEvent\", \"BinaryType\", \"ErrorEvent\"] }\n",
                );
            }
            if !has_wasm_bindgen {
                fix.push_str("wasm-bindgen = \"0.2\"\n");
            }
            steps.push(VerifyInstallStep {
                id: "realtime_wasm_deps",
                title: "`web-sys` (WebSocket features) + `wasm-bindgen` for socket clients",
                ok: false,
                looked_in: vec![cargo_toml.clone()],
                fix: Some(fix.trim_end().to_string()),
                fix_location: Some(
                    "wasm-only deps under `[target.'cfg(target_arch = \"wasm32\")'.dependencies]` keep the server build lean"
                        .into(),
                ),
            });
        }

        // Server side of realtime: tokio sync + time features power the
        // broadcast::Sender + reconnect-with-backoff loops.
        let tokio_missing = manifest.missing_features("tokio", &["sync", "time"]);
        if tokio_missing.is_empty() {
            steps.push(VerifyInstallStep {
                id: "realtime_tokio_deps",
                title: "`tokio` with `sync` + `time` features for broadcast::Sender",
                ok: true,
                looked_in: vec![cargo_toml.clone()],
                fix: None,
                fix_location: None,
            });
        } else {
            steps.push(VerifyInstallStep {
                id: "realtime_tokio_deps",
                title: "`tokio` with `sync` + `time` features for broadcast::Sender",
                ok: false,
                looked_in: vec![cargo_toml.clone()],
                fix: Some(format!(
                    "tokio = {{ version = \"1\", features = [\"sync\", \"time\"] }}  # add missing: {}",
                    tokio_missing.join(", ")
                )),
                fix_location: Some(
                    "`[dependencies]` for shared use, or `[target.'cfg(not(target_arch = \"wasm32\"))'.dependencies]` if only the server uses tokio"
                        .into(),
                ),
            });
        }
    }

    // Uuid in source ⇒ uuid crate with v4 + serde when the project also
    // serializes models. We only require serde when uuid is used alongside
    // serde-derived types (the common case in 0.7 fullstack apps).
    if signals.uses_uuid {
        let needs_serde = signals.has_server || manifest.has("serde") || manifest.has("serde_json");
        let required: &[&str] = if needs_serde {
            &["v4", "serde"]
        } else {
            &["v4"]
        };
        let missing = manifest.missing_features("uuid", required);
        if missing.is_empty() {
            steps.push(VerifyInstallStep {
                id: "uuid_dep",
                title: "`uuid` with required features",
                ok: true,
                looked_in: vec![cargo_toml.clone()],
                fix: None,
                fix_location: None,
            });
        } else {
            steps.push(VerifyInstallStep {
                id: "uuid_dep",
                title: "`uuid` with required features",
                ok: false,
                looked_in: vec![cargo_toml.clone()],
                fix: Some(format!(
                    "uuid = {{ version = \"1\", features = {:?} }}",
                    required
                )),
                fix_location: Some("[dependencies] table in Cargo.toml".into()),
            });
        }
    }
}

async fn resolve_crate_root(
    state: &Arc<State>,
    override_: Option<&str>,
) -> Result<PathBuf, String> {
    if let Some(root) = override_ {
        let info = crate::project::ProjectInfo::detect(Path::new(root));
        return info
            .manifest_dir()
            .ok_or_else(|| format!("no Cargo.toml found at or above `{root}`"));
    }
    let info = state.project.lock().await;
    info.manifest_dir()
        .ok_or_else(|| "no Cargo.toml found in the detected project root".into())
}

fn check_mod_components(main_rs: &Path, lib_rs: &Path) -> VerifyInstallStep {
    // Either main.rs or lib.rs satisfies the check — bin and lib crates wire
    // the module declaration in different files. We look in both so a lib-only
    // crate doesn't fail just because main.rs is absent.
    let candidates = [main_rs, lib_rs];
    let hits: Vec<PathBuf> = candidates
        .iter()
        .filter(|p| p.exists())
        .filter(|p| {
            std::fs::read_to_string(p)
                .map(|s| has_mod_components_decl(&s))
                .unwrap_or(false)
        })
        .map(|p| p.to_path_buf())
        .collect();
    let looked_in: Vec<PathBuf> = candidates
        .iter()
        .filter(|p| p.exists())
        .map(|p| p.to_path_buf())
        .collect();
    if !hits.is_empty() {
        VerifyInstallStep {
            id: "mod_components",
            title: "`mod components;` declared in crate root",
            ok: true,
            looked_in: hits,
            fix: None,
            fix_location: None,
        }
    } else {
        VerifyInstallStep {
            id: "mod_components",
            title: "`mod components;` declared in crate root",
            ok: false,
            looked_in,
            fix: Some("mod components;".into()),
            fix_location: Some(
                "near the top of src/main.rs (or src/lib.rs), alongside the other `mod` lines"
                    .into(),
            ),
        }
    }
}

fn has_mod_components_decl(src: &str) -> bool {
    // Tolerate leading whitespace, attributes on the previous line, and `pub`.
    // Reject only the trivial cases (commented out, no semicolon).
    for line in src.lines() {
        let t = line.trim();
        if t.starts_with("//") {
            continue;
        }
        // `mod components;` or `pub mod components;`. We don't care about
        // `mod components { ... }` — that's an inline module body which won't
        // resolve `src/components/<name>/component.rs` files anyway.
        let stripped = t.trim_start_matches("pub ").trim_start();
        if stripped == "mod components;" {
            return true;
        }
    }
    false
}

fn check_theme_asset(src_dir: &Path) -> VerifyInstallStep {
    // We don't know which file the user mounted the asset in — projects pick
    // their own App location. Scan the whole src/ tree (cheap; src is small)
    // for any `asset!("/assets/dx-components-theme.css")` reference. The
    // catalog template ships exactly that filename so a substring match is
    // accurate enough to use as a wiring proxy.
    let mut hits: Vec<PathBuf> = Vec::new();
    let mut walked: Vec<PathBuf> = Vec::new();
    walk_rs_files(src_dir, &mut walked, 6);
    for path in &walked {
        if let Ok(text) = std::fs::read_to_string(path)
            && text.contains("dx-components-theme.css")
        {
            hits.push(path.clone());
        }
    }
    if !hits.is_empty() {
        VerifyInstallStep {
            id: "theme_asset",
            title: "catalog theme stylesheet mounted via `asset!`",
            ok: true,
            looked_in: hits,
            fix: None,
            fix_location: None,
        }
    } else {
        VerifyInstallStep {
            id: "theme_asset",
            title: "catalog theme stylesheet mounted via `asset!`",
            ok: false,
            // Surface a small slice of paths the scan touched so the caller
            // can see we actually looked, without dumping the whole src tree.
            looked_in: walked.into_iter().take(8).collect(),
            fix: Some(
                r#"document::Link { rel: "stylesheet", href: asset!("/assets/dx-components-theme.css") }"#
                    .into(),
            ),
            fix_location: Some(
                "at the top of the rsx! body in your `App` component (sibling to the rest of \
                 your route / layout content)"
                    .into(),
            ),
        }
    }
}

fn check_components_dir(dir: &Path) -> VerifyInstallStep {
    // A `src/components/` directory should exist *after* the first
    // `dx components add` call. If it doesn't, the catalog hasn't been
    // installed yet — surfaced as a separate step so the caller can
    // distinguish "wiring missing" from "nothing installed yet".
    let exists = dir.exists() && dir.is_dir();
    if exists {
        VerifyInstallStep {
            id: "components_dir",
            title: "`src/components/` exists",
            ok: true,
            looked_in: vec![dir.to_path_buf()],
            fix: None,
            fix_location: None,
        }
    } else {
        VerifyInstallStep {
            id: "components_dir",
            title: "`src/components/` exists",
            ok: false,
            looked_in: vec![dir.to_path_buf()],
            fix: Some("dx components add <name>".into()),
            fix_location: Some(
                "run from the crate root for any catalog widget (e.g. `dx components add button`)"
                    .into(),
            ),
        }
    }
}

/// Returns true when the source mentions `headers::Cookie` (or `use headers::`)
/// as a top-level path, NOT as the tail of `axum_extra::headers::Cookie` /
/// `axum_extra::{..., headers::Cookie}`. Lookback at the byte before the
/// occurrence picks out the qualified form cheaply without a regex.
fn mentions_bare_headers_path(text: &str) -> bool {
    let needles = ["headers::Cookie", "use headers::"];
    for needle in needles {
        let mut start = 0usize;
        while let Some(idx) = text[start..].find(needle) {
            let pos = start + idx;
            let prefix_qualified = text[..pos]
                .chars()
                .next_back()
                .map(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
                .unwrap_or(false);
            // `axum_extra::{ headers::Cookie }` has a brace-or-comma-or-space
            // before the path, so the lookback is whitespace/`{`/`,` — not
            // qualified by character. But the *enclosing* `use` line still
            // qualifies the path. Detect that by walking the rest of the line
            // backward to find the nearest `use ` token, and checking if a
            // qualifying segment precedes the brace.
            if !prefix_qualified {
                let line_start = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
                let line_prefix = &text[line_start..pos];
                let line_qualified = line_prefix.contains("::{") || line_prefix.contains("::");
                if !line_qualified {
                    return true;
                }
                // The `use axum_extra::{..., headers::Cookie};` form: the
                // `::{` (or `::`) before us in the same logical statement
                // means this `headers::` is a re-export tail, not a bare
                // import. Skip it.
            }
            start = pos + needle.len();
        }
    }
    false
}

fn walk_rs_files(root: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && depth > 0 {
            walk_rs_files(&path, out, depth - 1);
        } else if path.extension().and_then(|x| x.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(p: &Path, body: &str) {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn fresh_crate_with_no_catalog_signals_reports_hand_rolled() {
        // The historical "fresh crate" shape — main.rs with a stub App, no
        // components/, no theme css — used to flag all three catalog steps.
        // With the hand_rolled archetype, this is recognized as "the user
        // isn't using the catalog" and the three steps are suppressed.
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("src/main.rs"),
            "fn main() { dioxus::launch(App); }\n#[component]\nfn App() -> Element { rsx!{} }\n",
        );
        let r = run(dir.path());
        assert_eq!(r.archetype, "hand_rolled");
        assert!(r.fully_wired, "no catalog usage → nothing to wire");
        assert!(
            r.missing.is_empty(),
            "hand_rolled archetype suppresses catalog steps, got missing = {:?}",
            r.missing
        );
        // Sanity: the three catalog steps must NOT appear in the report.
        for id in ["mod_components", "theme_asset", "components_dir"] {
            assert!(
                !r.steps.iter().any(|s| s.id == id),
                "{id} should be suppressed for hand_rolled, got steps = {:?}",
                r.steps.iter().map(|s| s.id).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn partial_catalog_signals_keeps_basic_archetype_and_flags_remaining_steps() {
        // The user has wired `mod components;` AND imports an actual catalog
        // widget — but hasn't mounted the theme stylesheet or created the
        // `src/components/` dir yet. Both gaps should surface; the catalog
        // import is the positive evidence theme_asset requires.
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("src/main.rs"),
            "mod components;\nuse crate::components::button::Button;\nfn main() {}\n",
        );
        let r = run(dir.path());
        assert_eq!(r.archetype, "basic");
        assert!(!r.fully_wired);
        assert!(r.missing.contains(&"theme_asset"));
        assert!(r.missing.contains(&"components_dir"));
    }

    #[test]
    fn theme_asset_suppressed_when_components_dir_is_hand_rolled() {
        // The user has their own `src/components/` and `mod components;` for
        // hand-rolled UI — no catalog widget is consumed (no
        // `use crate::components::<catalog_name>` import, no directory matching
        // a catalog entry). The theme stylesheet should NOT be flagged: it
        // wouldn't apply to hand-rolled UI anyway.
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("src/main.rs"),
            "mod components;\nfn main() {}\n",
        );
        write(
            &dir.path().join("src/components/protected/mod.rs"),
            "// hand-rolled, not from the catalog\n",
        );
        let r = run(dir.path());
        assert!(
            !r.missing.contains(&"theme_asset"),
            "theme_asset should be suppressed for hand-rolled components dirs; got missing = {:?}",
            r.missing
        );
        assert!(
            !r.steps.iter().any(|s| s.id == "theme_asset"),
            "theme_asset step shouldn't even appear when no catalog widget is used; got = {:?}",
            r.steps.iter().map(|s| s.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn axum_extra_without_separate_headers_dep_passes_when_typed_header_is_enabled() {
        // The `typed-header` feature on axum-extra re-exports `axum_extra::headers::Cookie`
        // — so a project that imports `axum_extra::headers::Cookie` doesn't
        // need a top-level `headers` dep. Verify_install used to demand both
        // unconditionally; now it gates on whether the source touches the
        // bare `headers::Cookie` path.
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("src/server/auth.rs"),
            r#"use axum_extra::{TypedHeader, headers::Cookie};
pub async fn check(_c: TypedHeader<Cookie>) {}
"#,
        );
        write(&dir.path().join("src/main.rs"), "fn main() {}\n");
        write(
            &dir.path().join("Cargo.toml"),
            r#"[package]
name = "x"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = { version = "0.7", features = ["fullstack"] }
axum-extra = { version = "0.10", features = ["typed-header"] }
"#,
        );
        let r = run(dir.path());
        let step = r
            .steps
            .iter()
            .find(|s| s.id == "axum_extra_cookies")
            .expect("axum_extra_cookies step should be present");
        assert!(
            step.ok,
            "axum-extra w/ typed-header should be enough (the typed-header feature re-exports headers::Cookie); got: {step:?}",
        );
    }

    #[test]
    fn uuid_features_pulled_through_feature_alias_are_detected() {
        // uuid is declared `optional = true` and its features are added via
        // `[features]` aliases (`server = ["uuid/v4", "uuid/serde"]`). The
        // parser should fold those into uuid's known features so the
        // verify_install step doesn't false-positive.
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("src/model/todo.rs"),
            "pub struct Todo { pub id: Uuid }\n",
        );
        write(&dir.path().join("src/main.rs"), "fn main() {}\n");
        write(
            &dir.path().join("Cargo.toml"),
            r#"[package]
name = "x"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = { version = "0.7", features = ["fullstack"] }
uuid = { version = "1", optional = true }

[features]
server = ["uuid/v4", "uuid/serde"]
"#,
        );
        let r = run(dir.path());
        let step = r.steps.iter().find(|s| s.id == "uuid_dep").unwrap();
        assert!(
            step.ok,
            "uuid features pulled via [features] alias should be recognized; got: {step:?}",
        );
    }

    #[test]
    fn picks_up_mod_components_in_main_rs() {
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("src/main.rs"),
            "mod components;\nfn main() {}\n",
        );
        let r = run(dir.path());
        let step = step_by_id(&r, "mod_components");
        assert!(step.ok);
        assert_eq!(step.looked_in.len(), 1);
        assert!(step.looked_in[0].ends_with("src/main.rs"));
    }

    #[test]
    fn picks_up_mod_components_in_lib_rs_when_no_main() {
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("src/lib.rs"),
            "pub mod components;\npub fn run() {}\n",
        );
        let r = run(dir.path());
        let step = step_by_id(&r, "mod_components");
        assert!(step.ok);
        assert!(step.looked_in[0].ends_with("src/lib.rs"));
    }

    #[test]
    fn ignores_commented_out_mod_decl() {
        // We need at least one positive catalog signal so the hand_rolled
        // suppression doesn't kick in and drop the step we want to inspect.
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("src/main.rs"),
            "// mod components;\nfn main() {}\n",
        );
        // Adding the components/ dir is enough to keep the archetype `basic`
        // and surface the catalog steps.
        write(&dir.path().join("src/components/mod.rs"), "");
        let r = run(dir.path());
        assert!(!step_by_id(&r, "mod_components").ok);
    }

    #[test]
    fn picks_up_theme_asset_anywhere_under_src() {
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("src/main.rs"),
            "mod components;\nfn main() {}\n",
        );
        write(
            &dir.path().join("src/components/app.rs"),
            r#"#[component]
fn App() -> Element {
    rsx! {
        document::Link { rel: "stylesheet", href: asset!("/assets/dx-components-theme.css") }
    }
}
"#,
        );
        let r = run(dir.path());
        let step = step_by_id(&r, "theme_asset");
        assert!(step.ok);
        assert!(
            step.looked_in
                .iter()
                .any(|p| p.ends_with("src/components/app.rs"))
        );
    }

    #[test]
    fn detects_components_dir_present() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/components")).unwrap();
        write(&dir.path().join("src/main.rs"), "fn main() {}\n");
        let r = run(dir.path());
        assert!(step_by_id(&r, "components_dir").ok);
    }

    #[test]
    fn fully_wired_when_everything_present() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/components")).unwrap();
        write(
            &dir.path().join("src/main.rs"),
            r#"mod components;
fn main() {}
#[component]
fn App() -> Element {
    rsx! {
        document::Link { rel: "stylesheet", href: asset!("/assets/dx-components-theme.css") }
    }
}
"#,
        );
        let r = run(dir.path());
        assert!(r.fully_wired);
        assert!(r.missing.is_empty());
    }

    fn step_by_id<'a>(r: &'a VerifyInstallReport, id: &str) -> &'a VerifyInstallStep {
        r.steps.iter().find(|s| s.id == id).expect("step exists")
    }

    fn run(crate_root: &Path) -> VerifyInstallReport {
        let src = crate_root.join("src");
        let main_rs = src.join("main.rs");
        let lib_rs = src.join("lib.rs");
        let mod_step = check_mod_components(&main_rs, &lib_rs);
        let theme_step = check_theme_asset(&src);
        let dir_step = check_components_dir(&src.join("components"));
        let signals = ArchetypeSignals::scan(&src);
        let base_archetype = signals.archetype_label();
        let manifest_text =
            std::fs::read_to_string(crate_root.join("Cargo.toml")).unwrap_or_default();
        let manifest = parse_manifest(&manifest_text);

        // Mirror the hand_rolled gating from the real entry point.
        let no_dx_catalog = !mod_step.ok && !theme_step.ok && !dir_step.ok;
        let archetype = if no_dx_catalog && base_archetype == "basic" {
            "hand_rolled"
        } else {
            base_archetype
        };
        let mut steps = if no_dx_catalog {
            Vec::new()
        } else {
            let mut s = vec![mod_step];
            if signals.uses_dx_catalog || theme_step.ok {
                s.push(theme_step);
            }
            s.push(dir_step);
            s
        };
        extend_with_archetype_steps(&mut steps, &signals, &manifest, crate_root);
        let missing: Vec<&'static str> = steps.iter().filter(|s| !s.ok).map(|s| s.id).collect();
        let fully_wired = missing.is_empty();
        VerifyInstallReport {
            project_root: crate_root.to_path_buf(),
            fully_wired,
            archetype,
            steps,
            missing,
        }
    }

    #[test]
    fn basic_archetype_has_no_extra_dep_steps() {
        let dir = tempdir().unwrap();
        write(&dir.path().join("src/main.rs"), "fn main() {}\n");
        // Plant a catalog signal so hand_rolled suppression doesn't fire and
        // we can assert on the `basic` archetype label.
        write(&dir.path().join("src/components/mod.rs"), "");
        let r = run(dir.path());
        assert_eq!(r.archetype, "basic");
        let extra_ids: Vec<&str> = r
            .steps
            .iter()
            .map(|s| s.id)
            .filter(|id| !matches!(*id, "mod_components" | "theme_asset" | "components_dir"))
            .collect();
        assert!(
            extra_ids.is_empty(),
            "basic archetype should not gain archetype steps, got {extra_ids:?}"
        );
    }

    #[test]
    fn fullstack_archetype_flags_axum_extra_for_cookies() {
        let dir = tempdir().unwrap();
        // src/server/ triggers the fullstack signal, and a TypedHeader<Cookie>
        // reference is what we look for to add the axum-extra dep step.
        write(
            &dir.path().join("src/server/auth.rs"),
            r#"use axum_extra::TypedHeader;
use headers::Cookie;
pub async fn check(_c: TypedHeader<Cookie>) {}
"#,
        );
        write(&dir.path().join("src/main.rs"), "fn main() {}\n");
        write(
            &dir.path().join("Cargo.toml"),
            r#"[package]
name = "x"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = { version = "0.7", features = ["fullstack"] }
"#,
        );
        let r = run(dir.path());
        assert_eq!(r.archetype, "fullstack");
        let step = r
            .steps
            .iter()
            .find(|s| s.id == "axum_extra_cookies")
            .expect("axum_extra_cookies step should be present");
        assert!(!step.ok, "axum-extra missing should be flagged");
        let fix = step.fix.as_deref().unwrap_or("");
        assert!(
            fix.contains("axum-extra"),
            "fix should add axum-extra: {fix}"
        );
        assert!(
            fix.contains("typed-header"),
            "fix should enable typed-header feature: {fix}"
        );
    }

    #[test]
    fn realtime_archetype_flags_websys_and_tokio_features() {
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("src/sockets/board.rs"),
            "// generated socket file\n",
        );
        write(&dir.path().join("src/main.rs"), "fn main() {}\n");
        // Missing wasm bindings and tokio features.
        write(
            &dir.path().join("Cargo.toml"),
            r#"[package]
name = "x"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = { version = "0.7", features = ["fullstack"] }
tokio = "1"
"#,
        );
        let r = run(dir.path());
        assert_eq!(r.archetype, "fullstack-realtime");
        let websys = r
            .steps
            .iter()
            .find(|s| s.id == "realtime_wasm_deps")
            .expect("realtime_wasm_deps step should be present");
        assert!(!websys.ok);
        assert!(websys.fix.as_deref().unwrap_or("").contains("web-sys"));
        assert!(websys.fix.as_deref().unwrap_or("").contains("WebSocket"));
        let tokio = r
            .steps
            .iter()
            .find(|s| s.id == "realtime_tokio_deps")
            .expect("realtime_tokio_deps step should be present");
        assert!(!tokio.ok);
        assert!(tokio.fix.as_deref().unwrap_or("").contains("sync"));
        assert!(tokio.fix.as_deref().unwrap_or("").contains("time"));
    }

    #[test]
    fn realtime_archetype_passes_when_deps_correctly_declared() {
        let dir = tempdir().unwrap();
        write(&dir.path().join("src/sockets/board.rs"), "// socket\n");
        write(&dir.path().join("src/main.rs"), "fn main() {}\n");
        write(
            &dir.path().join("Cargo.toml"),
            r#"[package]
name = "x"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = { version = "0.7", features = ["fullstack"] }
tokio = { version = "1", features = ["sync", "time"] }

[target.'cfg(target_arch = "wasm32")'.dependencies]
web-sys = { version = "0.3", features = ["WebSocket", "MessageEvent", "BinaryType", "ErrorEvent"] }
wasm-bindgen = "0.2"
"#,
        );
        let r = run(dir.path());
        let websys = r
            .steps
            .iter()
            .find(|s| s.id == "realtime_wasm_deps")
            .unwrap();
        assert!(websys.ok, "expected realtime_wasm_deps ok, got: {websys:?}");
        let tokio = r
            .steps
            .iter()
            .find(|s| s.id == "realtime_tokio_deps")
            .unwrap();
        assert!(tokio.ok, "expected realtime_tokio_deps ok, got: {tokio:?}");
    }

    #[test]
    fn uuid_usage_triggers_uuid_dep_step() {
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("src/model/todo.rs"),
            "pub struct Todo { pub id: Uuid }\n",
        );
        write(&dir.path().join("src/main.rs"), "fn main() {}\n");
        write(
            &dir.path().join("Cargo.toml"),
            r#"[package]
name = "x"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = { version = "0.7", features = ["web"] }
"#,
        );
        let r = run(dir.path());
        let step = r.steps.iter().find(|s| s.id == "uuid_dep").unwrap();
        assert!(!step.ok);
        assert!(step.fix.as_deref().unwrap_or("").contains("uuid"));
        assert!(step.fix.as_deref().unwrap_or("").contains("v4"));
    }
}
