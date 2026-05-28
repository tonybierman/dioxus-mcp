# Making saved screen layouts structurally reproducible

## Problem

A layout saved to the screen-layout registry lost the CSS its structure depends
on. `admin_console` was authored in `arium/examples/dioxus-fullstack-example`
using semantic CSS classes (`.admin-layout`, `.admin-sidebar`,
`.admin-sidebar--open`, `.admin-nav-item.is-active`, `.admin-content`, …) whose
structure — grid columns, sticky desktop sidebar, the mobile-first drawer with a
`48rem` media query — lived in ~7 KB of hand-written rules in `assets/app.css`.
Arium uses **no Tailwind**; it styles via `asset!` / `css_module!` / `app.css`.

When the screen was saved, two lossy things happened:

1. **The real CSS could not be stored.** A `LayoutDescriptor` was `template`
   (RSX markup) + `preview` (skeleton). There was no field for CSS, so the rules
   that produced the structure were left behind in arium's `app.css`.
2. **The template was re-authored in Tailwind utilities** — an approximate
   re-derivation, not the original rules.

Re-applied in a non-Tailwind project, every utility class resolved to nothing →
an unstyled stack of `div`s. The registry modeled a layout's *markup* but not its
*style dependency*.

## Fix

Let a layout carry its own style dependency, and declare it when it can't.

### A1 — descriptor fields (`crates/dioxus-mcp-registry/src/layout.rs`)
- `styles: Option<String>` — raw CSS the markup depends on. A layout that styles
  via semantic class names carries the rules here, making it toolchain-independent.
- `requires: Option<String>` — styling-family hint (`tailwind` | `vanilla_css`)
  for layouts that ship *no* `styles` of their own. A hint, not a gate.

Both `#[serde(default, skip_serializing_if = "Option::is_none")]`, so existing
descriptors and the wasm-safe, serde-only registry crate are unaffected.

### A2 — emit carried CSS on scaffold (`generate/screen.rs`)
After writing the component, `generate_screen` looks up the chosen layout
descriptor. If it has non-empty `styles`, they are written to
`assets/{snake}.css` (skip-if-exists, never overwrite — mirrors the existing
`vanilla-css` starter path) and a `document::Stylesheet { href: asset!(…) }`
mount hint is pushed to `next_steps`.

### C — declare-and-verify advisory (`generate/screen.rs`)
If a layout ships no `styles` but declares `requires`, scaffold pushes an
advisory next-step ("make sure this project has a working `<x>` setup whose
content scan reaches `src/components/{snake}.rs`, or the screen renders
unstyled") so the silent-unstyled-divs failure becomes loud. The 12 built-in
library layouts now declare `requires = "tailwind"`.

### B — re-capture `admin_console`, then promote it to a built-in
`admin_console` was rewritten to use arium's semantic class names and to carry
the real `app.css` rules verbatim in `styles`. Theme tokens are kept as
`var(--token, <neutral>)` so the layout adopts a project's theme when present and
falls back to neutral grays standalone.

It was then **promoted from the machine-local global overlay into the built-in
library** (`tools/dsl/layout_library.rs`, via `library_layouts()`), so it ships
compiled into the binary with every project, is version-controlled, and is
maintained alongside the code. It is the library's first *self-contained* layout
(semantic classes + carried CSS, `requires: None`) — the reference example of
the `styles` mechanism, contrasting the twelve Tailwind-utility shells. The
redundant global `~/.config/dioxus-mcp/registry/layouts/admin_console.toml` was
removed so the in-repo definition is the single source of truth (a same-id
global overlay would otherwise shadow the built-in).

### D — make the whole structural library self-contained
Following on from B, all twelve remaining library shells (holy_grail, bento,
masonry, full_bleed, sticky_sidebar, mega_menu, drawer, card_grid, editorial,
hero_scroll, split_screen, scroll_sticky) were converted from Tailwind-utility
markup to semantic class names + carried, responsive CSS — so the entire library
is now toolchain-independent and no layout declares `requires`. Each sheet is
mobile-first with a 48rem breakpoint (card_grid also steps at 40/64rem;
`drawer` is intentionally breakpoint-free since an off-canvas overlay is the same
at every size). The sheets share a documented neutral token vocabulary
(`--border`, `--surface`, `--surface-sunken`, `--text`, `--text-strong`,
`--text-muted`, `--accent`, `--invert-bg`, `--invert-text`, `--invert-muted`) read
via `var(--token, <fallback>)`. The `descriptor()` helper now takes the CSS and
sets `styles: Some(..)`, `requires: None` for every layout.

## Tests
- `registry.rs::project_overlay_adds_and_overrides_by_id` — a `styles`-bearing
  overlay descriptor survives the TOML parse with `requires == None`.
- `layout_library.rs::every_library_layout_is_self_contained` — every built-in
  carries non-empty `styles`, declares no `requires`, and (except `drawer`) is
  responsive (`@media`).
- `layout_library.rs::every_library_layout_renders_a_valid_component` — all 13
  templates render to valid component bodies.
- `tests/client_crud.rs::registry_layout_with_styles_emits_sibling_stylesheet` —
  end-to-end: a registry layout with `styles` writes `assets/{snake}.css` with
  the structural rules and surfaces the mount hint.

## Known limitations / follow-ups
- Semantic class names are global, so two screens using the same custom layout
  share class names; the per-screen `assets/{snake}.css` files would collide on
  selectors. Matches arium's own global `.admin-layout` convention. A future
  option: scope rules under the screen's `root_class`.
- `requires` is advisory only; wiring it into `verify_install` to actively check
  the target project's toolchain is a natural next step.
