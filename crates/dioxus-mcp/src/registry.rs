//! Loads the theme/component/layout [`Registry`]: embedded built-in defaults,
//! overlaid by runtime descriptors found on disk. The defaults seed today's
//! hardcoded catalogs (5 layouts, the `dx` component catalog, a handful of
//! themes).
//!
//! Where descriptors live (later overrides earlier, per id/name):
//! 1. embedded built-ins (always present);
//! 2. **global** `~/.config/dioxus-mcp/registry/{themes,components,layouts}/` —
//!    the **canonical** place for descriptors you want in *every* project. The
//!    `dioxus` MCP is stdio with no `--project-root`, so `project_root` defaults
//!    to each session's launch cwd (`lib.rs`); the global dir sidesteps that and
//!    is read regardless of cwd;
//! 3. **project** `<project_root>/.dioxus-mcp/registry/...` — persistent (NOT
//!    under `target/`, so `cargo clean` won't wipe it; commit it to share with
//!    the team) and highest precedence; or point `DIOXUS_MCP_REGISTRY_DIR`
//!    anywhere to replace this search root.
//!
//! `State::registry()` calls this on demand (not cached), so descriptors
//! **hot-reload** — add or edit a file and the next call sees it.
//!
//! Loading is best-effort and mirrors the proposals store (see `proposal.rs`):
//! a missing dir is empty, a malformed file is logged at debug and skipped, and
//! nothing here ever fails the server. Local & trusted — no sandboxing; a
//! layout descriptor's codegen template is treated like the user's own source.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use dioxus_mcp_registry::{
    ComponentDescriptor, LayoutDescriptor, PreviewSkeleton, Registry, ThemeDescriptor, ThemeTokens,
};
use serde::de::DeserializeOwned;

/// Build the full registry for a project: built-ins, then the global user dir,
/// then the project dir (each layer overrides the previous by id/name).
pub fn load(project_root: &Path) -> Registry {
    let mut reg = builtin();
    if let Some(dir) = global_dir() {
        overlay_dir(&mut reg, &dir);
    }
    overlay_dir(&mut reg, &project_dir(project_root));
    reg
}

/// `DIOXUS_MCP_REGISTRY_DIR` overrides the per-project search root (mirrors the
/// `DIOXUS_MCP_UI_DIR` idiom); otherwise `<project>/.dioxus-mcp/registry` — a
/// persistent, committable location (NOT under `target/`, which `cargo clean`
/// wipes).
fn project_dir(project_root: &Path) -> PathBuf {
    std::env::var_os("DIOXUS_MCP_REGISTRY_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| project_root.join(".dioxus-mcp/registry"))
}

/// `~/.config/dioxus-mcp/registry` (XDG_CONFIG_HOME, else HOME/.config).
fn global_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("dioxus-mcp/registry"))
}

fn overlay_dir(reg: &mut Registry, dir: &Path) {
    for t in read_descriptors::<ThemeDescriptor>(&dir.join("themes")) {
        reg.themes.insert(t.id.clone(), t);
    }
    for c in read_descriptors::<ComponentDescriptor>(&dir.join("components")) {
        reg.components.insert(c.name.clone(), c);
    }
    for l in read_descriptors::<LayoutDescriptor>(&dir.join("layouts")) {
        reg.layouts.insert(l.id.clone(), l);
    }
}

/// Parse every `*.toml` in `dir` into `T`. Missing dir → empty; a file that
/// fails to read or parse is skipped with a debug log (never fatal).
fn read_descriptors<T: DeserializeOwned>(dir: &Path) -> Vec<T> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let parsed = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|s| toml::from_str::<T>(&s).map_err(|e| e.to_string()));
        match parsed {
            Ok(d) => out.push(d),
            Err(e) => {
                tracing::debug!(error = %e, path = %path.display(), "registry: skipping malformed descriptor")
            }
        }
    }
    out
}

/// The embedded defaults — today's hardcoded catalogs expressed as descriptors.
pub fn builtin() -> Registry {
    Registry {
        themes: builtin_themes(),
        components: builtin_components(),
        layouts: builtin_layouts(),
    }
}

/// Mirror the historical `DX_COMPONENT_CATALOG_ENTRIES` tuples 1:1 so the
/// catalog (and its sync test) is unchanged; the registry just adds overlay.
fn builtin_components() -> BTreeMap<String, ComponentDescriptor> {
    crate::tools::dsl::dx_components::DX_COMPONENT_CATALOG_ENTRIES
        .iter()
        .map(|(name, description, prop_hint)| {
            (
                name.to_string(),
                ComponentDescriptor {
                    name: name.to_string(),
                    description: description.to_string(),
                    prop_hint: prop_hint.to_string(),
                    keywords: Vec::new(),
                    audit_classes: Vec::new(),
                },
            )
        })
        .collect()
}

/// The 5 built-in layout kinds. In v1 the resource/crud kinds stay `complex`
/// (they keep their Rust sub-renderers); `empty` is the only purely
/// template-driven one. `label`/`nav_rank` mirror the navigator's current
/// hardcoded `label_for`/`kind_rank`. Preview skeletons are filled in Phase 6.
fn builtin_layouts() -> BTreeMap<String, LayoutDescriptor> {
    fn layout(id: &str, label: &str, nav_rank: u8, complex: bool) -> LayoutDescriptor {
        LayoutDescriptor {
            id: id.to_string(),
            label: label.to_string(),
            nav_rank,
            template: None,
            complex,
            context_vars: Vec::new(),
            preview: PreviewSkeleton::default(),
        }
    }
    [
        layout("resource_list", "List", 0, true),
        layout("resource_form", "New", 1, true),
        layout("resource_edit_form", "Edit", 2, true),
        // empty/client_crud have no flow label — the navigator shows the screen
        // name (label left blank signals "use the name").
        layout("client_crud", "", 3, true),
        layout("empty", "", 3, false),
    ]
    .into_iter()
    .map(|l| (l.id.clone(), l))
    .collect()
}

/// Built-in themes. `dark` is the current cockpit palette; `light` a derived
/// variant; `tailwind`/`vanilla-css` are the two styling families (their
/// `class_styles`/tokens are filled in Phase 4 from the existing presets).
fn builtin_themes() -> BTreeMap<String, ThemeDescriptor> {
    fn colors(pairs: &[(&str, &str)]) -> ThemeTokens {
        ThemeTokens {
            color: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        }
    }
    fn theme(id: &str, label: &str, kind: &str, tokens: ThemeTokens) -> ThemeDescriptor {
        ThemeDescriptor {
            id: id.to_string(),
            label: label.to_string(),
            kind: kind.to_string(),
            tokens,
            class_styles: BTreeMap::new(),
        }
    }
    [
        theme(
            "dark",
            "Dark",
            "unstyled",
            colors(&[
                ("bg", "#0f1115"),
                ("panel", "#171a21"),
                ("border", "#2a2f3a"),
                ("text", "#d8dee9"),
                ("muted", "#8a92a3"),
                ("accent", "#6aa9ff"),
                ("error", "#ff6b6b"),
                ("code", "#11141a"),
            ]),
        ),
        theme(
            "light",
            "Light",
            "unstyled",
            colors(&[
                ("bg", "#ffffff"),
                ("panel", "#f4f6fa"),
                ("border", "#d4dae6"),
                ("text", "#1a1d24"),
                ("muted", "#5a6172"),
                ("accent", "#2f6fe0"),
                ("error", "#d04a4a"),
                ("code", "#eef1f6"),
            ]),
        ),
        theme("tailwind", "Tailwind", "tailwind", ThemeTokens::default()),
        theme(
            "vanilla-css",
            "Vanilla CSS",
            "vanilla_css",
            ThemeTokens::default(),
        ),
    ]
    .into_iter()
    .map(|t| (t.id.clone(), t))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_seeds_the_current_catalogs() {
        let reg = builtin();
        assert_eq!(
            reg.components.len(),
            crate::tools::dsl::dx_components::DX_COMPONENT_CATALOG_ENTRIES.len()
        );
        assert_eq!(reg.layouts.len(), 5);
        assert_eq!(reg.themes.len(), 4);
        assert!(reg.layouts.contains_key("resource_list"));
        assert!(reg.themes.contains_key("dark"));
        assert!(reg.components.contains_key("button"));
    }

    #[test]
    fn project_overlay_adds_and_overrides_by_id() {
        let dir = tempfile::tempdir().unwrap();
        let layouts = dir.path().join("layouts");
        std::fs::create_dir_all(&layouts).unwrap();
        std::fs::write(
            layouts.join("kanban.toml"),
            "id = \"kanban\"\nlabel = \"Board\"\nnav_rank = 5\ncomplex = false\n",
        )
        .unwrap();
        std::fs::write(
            layouts.join("override.toml"),
            "id = \"empty\"\nlabel = \"Blank\"\n",
        )
        .unwrap();

        let mut reg = builtin();
        overlay_dir(&mut reg, dir.path());

        assert!(reg.layouts.contains_key("kanban"), "new layout added");
        assert_eq!(reg.layouts["empty"].label, "Blank", "existing layout overridden");
        assert_eq!(reg.layouts.len(), 6);
    }

    #[test]
    fn reload_reflects_descriptors_added_after_a_first_load() {
        // Hot-reload: `State::registry()` calls `load()` on every access (no
        // cache), so a descriptor written after one load shows up on the next.
        // Presence assertions (unique ids) keep this robust to any real global
        // descriptors on the host.
        let root = tempfile::tempdir().unwrap();
        let layouts = root.path().join(".dioxus-mcp/registry/layouts");
        std::fs::create_dir_all(&layouts).unwrap();
        std::fs::write(layouts.join("a.toml"), "id = \"hot_alpha\"\ncomplex = false\n").unwrap();

        let first = load(root.path());
        assert!(first.layouts.contains_key("hot_alpha"), "project dir is .dioxus-mcp/registry");
        assert!(!first.layouts.contains_key("hot_beta"));

        std::fs::write(layouts.join("b.toml"), "id = \"hot_beta\"\ncomplex = false\n").unwrap();
        let second = load(root.path());
        assert!(second.layouts.contains_key("hot_alpha"));
        assert!(second.layouts.contains_key("hot_beta"), "reload picks up the new file");
    }

    #[test]
    fn malformed_descriptor_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let themes = dir.path().join("themes");
        std::fs::create_dir_all(&themes).unwrap();
        std::fs::write(themes.join("bad.toml"), "= = = not valid = = =").unwrap();
        std::fs::write(themes.join("ok.toml"), "id = \"solarized\"\nlabel = \"Solarized\"\n").unwrap();

        let got = read_descriptors::<ThemeDescriptor>(&themes);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "solarized");
    }
}
