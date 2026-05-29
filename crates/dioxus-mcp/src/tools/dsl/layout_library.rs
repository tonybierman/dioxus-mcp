//! The built-in library of *structural* screen layouts — page-level shells
//! (holy-grail, bento grid, split-screen, …) an agent can pick via a screen's
//! `template.kind`. Each is a `complex: false` [`LayoutDescriptor`]: a minijinja
//! `template` (rendered by `render_registry_layout` into a full component file,
//! exactly like the built-in `SCREEN_TPL`) plus a `preview` skeleton the cockpit
//! navigator tree-walks. They're seeded into the registry by
//! [`builtin_layouts`](crate::registry) so they ship with every project and show
//! up in `get_registry`.
//!
//! Styling is **self-contained**: each layout's markup uses semantic class names
//! and the structure lives in the descriptor's `styles` CSS, written alongside
//! the screen as `assets/{snake}.css` on scaffold. So a structural screen
//! reproduces identically in any project regardless of CSS toolchain — none of
//! these depend on Tailwind. The sheets are mobile-first and responsive (a 48rem
//! breakpoint, mirroring `app-shell`'s) and read theme CSS vars with neutral
//! fallbacks so they adopt a project's theme when present and stand alone when
//! not. The shared token hooks are:
//!
//! - `--border`         hairlines / dividers                  (fallback `#e5e7eb`)
//! - `--surface`        raised panels (cards, drawer, popover) (`#ffffff`)
//! - `--surface-sunken` tiles, thumbnails, sticky fills        (`#f3f4f6`)
//! - `--text`           body copy                             (`#1f2937`)
//! - `--text-strong`    headings                              (`#111827`)
//! - `--text-muted`     secondary text                        (`#6b7280`)
//! - `--accent`         rules / emphasis                      (`#111827`)
//! - `--invert-bg`      dark sections (hero, split)           (`#111827`)
//! - `--invert-text`    text on dark                          (`#f9fafb`)
//! - `--invert-muted`   muted text on dark                    (`#d1d5db`)
//!
//! (`admin_console` predates this set and uses arium's `--primary-color-*` /
//! `--secondary-color-*` token names; both fall back to the same neutrals.)
//! `{{ root_class }}` (default `screen {snake}`, or the screen's `class:`
//! override) rides along on the root as a customization hook. To restyle a
//! layout, edit the generated component and its emitted sheet.
//!
//! Templates use only the context the runtime-layout renderer provides
//! (`pascal`, `snake`, `wrap_pascal`, `root_class`); `wrap_with` is honored via
//! conditional wrapper braces so a guard like `ProtectedRoute` can sit outside
//! the layout root.

use dioxus_mcp_registry::{LayoutDescriptor, PreviewSkeleton, RenderNode};

// ---------------------------------------------------------------------------
// Templates + their CSS. Each template emits a full component file via semantic
// class names; the paired `*_CSS` const defines those classes and is emitted as
// `assets/{snake}.css`. `{%- if wrap_pascal %}` wraps the layout root in the
// guard component without duplicating the markup.
// ---------------------------------------------------------------------------

const HOLY_GRAIL_TPL: &str = r#"use dioxus::prelude::*;
{%- if wrap_pascal %}
use crate::components::{{ wrap_pascal }};
{%- endif %}

#[component]
pub fn {{ pascal }}() -> Element {
    rsx! {
{%- if wrap_pascal %}
        {{ wrap_pascal }} {
{%- endif %}
        div { class: "{{ root_class }} holy-grail",
            header { class: "hg-header", "{{ pascal }}" }
            nav { class: "hg-nav", "Sidebar" }
            main { class: "hg-main", "Main content" }
            aside { class: "hg-aside", "Aside" }
            footer { class: "hg-footer", "Footer" }
        }
{%- if wrap_pascal %}
        }
{%- endif %}
    }
}
"#;

const HOLY_GRAIL_CSS: &str = r#"/* Holy grail: header/footer span the full width; nav + aside flank the main
   column at the 48rem breakpoint and collapse away on phones. */
.holy-grail {
  min-height: 100vh;
  display: grid;
  grid-template-rows: auto 1fr auto;
  grid-template-columns: 1fr;
}
.hg-header {
  padding: 1rem;
  font-weight: 600;
  border-bottom: 1px solid var(--border, #e5e7eb);
}
.hg-nav {
  display: none;
  padding: 1rem;
  border-right: 1px solid var(--border, #e5e7eb);
}
.hg-main {
  padding: 1.5rem;
}
.hg-aside {
  display: none;
  padding: 1rem;
  font-size: 0.875rem;
  color: var(--text-muted, #6b7280);
  border-left: 1px solid var(--border, #e5e7eb);
}
.hg-footer {
  padding: 1rem;
  font-size: 0.875rem;
  color: var(--text-muted, #6b7280);
  border-top: 1px solid var(--border, #e5e7eb);
}
@media (min-width: 48rem) {
  .holy-grail {
    grid-template-columns: 200px 1fr 240px;
  }
  .hg-header,
  .hg-footer {
    grid-column: 1 / -1;
  }
  .hg-nav,
  .hg-aside {
    display: block;
  }
}
"#;

const BENTO_GRID_TPL: &str = r#"use dioxus::prelude::*;
{%- if wrap_pascal %}
use crate::components::{{ wrap_pascal }};
{%- endif %}

#[component]
pub fn {{ pascal }}() -> Element {
    rsx! {
{%- if wrap_pascal %}
        {{ wrap_pascal }} {
{%- endif %}
        div { class: "{{ root_class }} bento",
            h1 { class: "bento-title", "{{ pascal }}" }
            div { class: "bento-grid",
                div { class: "bento-tile bento-feature", "Feature" }
                div { class: "bento-tile", "Tile" }
                div { class: "bento-tile", "Tile" }
                div { class: "bento-tile bento-wide", "Wide tile" }
                div { class: "bento-tile", "Tile" }
                div { class: "bento-tile", "Tile" }
            }
        }
{%- if wrap_pascal %}
        }
{%- endif %}
    }
}
"#;

const BENTO_GRID_CSS: &str = r#"/* Bento: a 2-up grid of fixed-height tiles that becomes 4-up at 48rem; the
   feature tile spans 2x2 and the wide tile spans two columns. */
.bento {
  padding: 1.5rem;
}
.bento-title {
  margin: 0 0 1.5rem;
  font-size: 1.5rem;
  font-weight: 700;
  color: var(--text-strong, #111827);
}
.bento-grid {
  display: grid;
  grid-template-columns: repeat(2, 1fr);
  grid-auto-rows: 150px;
  gap: 1rem;
}
.bento-tile {
  padding: 1.25rem;
  border-radius: 1rem;
  background: var(--surface-sunken, #f3f4f6);
}
.bento-feature {
  grid-column: span 2;
  grid-row: span 2;
}
.bento-wide {
  grid-column: span 2;
}
@media (min-width: 48rem) {
  .bento-grid {
    grid-template-columns: repeat(4, 1fr);
  }
}
"#;

const MASONRY_TPL: &str = r#"use dioxus::prelude::*;
{%- if wrap_pascal %}
use crate::components::{{ wrap_pascal }};
{%- endif %}

#[component]
pub fn {{ pascal }}() -> Element {
    rsx! {
{%- if wrap_pascal %}
        {{ wrap_pascal }} {
{%- endif %}
        div { class: "{{ root_class }} masonry",
            h1 { class: "masonry-title", "{{ pascal }}" }
            div { class: "masonry-grid",
                div { class: "masonry-item", "Item" }
                div { class: "masonry-item", "Item" }
                div { class: "masonry-item", "Item" }
                div { class: "masonry-item", "Item" }
                div { class: "masonry-item", "Item" }
                div { class: "masonry-item", "Item" }
            }
        }
{%- if wrap_pascal %}
        }
{%- endif %}
    }
}
"#;

const MASONRY_CSS: &str = r#"/* Masonry: CSS multi-column flow (2 columns, 3 at 48rem). Items keep their
   intrinsic height and never break across columns; the nth-child heights just
   sketch the staggered look in the skeleton. */
.masonry {
  padding: 1.5rem;
}
.masonry-title {
  margin: 0 0 1.5rem;
  font-size: 1.5rem;
  font-weight: 700;
  color: var(--text-strong, #111827);
}
.masonry-grid {
  columns: 2;
  column-gap: 1rem;
}
.masonry-item {
  break-inside: avoid;
  margin-bottom: 1rem;
  padding: 1rem;
  border-radius: 0.75rem;
  background: var(--surface-sunken, #f3f4f6);
}
.masonry-item:nth-child(2) { min-height: 14rem; }
.masonry-item:nth-child(3) { min-height: 10rem; }
.masonry-item:nth-child(4) { min-height: 12rem; }
.masonry-item:nth-child(6) { min-height: 11rem; }
@media (min-width: 48rem) {
  .masonry-grid {
    columns: 3;
  }
}
"#;

const FULL_BLEED_TPL: &str = r#"use dioxus::prelude::*;
{%- if wrap_pascal %}
use crate::components::{{ wrap_pascal }};
{%- endif %}

#[component]
pub fn {{ pascal }}() -> Element {
    rsx! {
{%- if wrap_pascal %}
        {{ wrap_pascal }} {
{%- endif %}
        div { class: "{{ root_class }} full-bleed",
            section { class: "fb-hero",
                h1 { class: "fb-title", "{{ pascal }}" }
            }
            section { class: "fb-body",
                p { class: "fb-copy", "Full-bleed content stretches edge to edge — no max-width container." }
            }
        }
{%- if wrap_pascal %}
        }
{%- endif %}
    }
}
"#;

const FULL_BLEED_CSS: &str = r#"/* Full-bleed: sections stretch edge to edge with no max-width container; the
   hero is an inverted band. Padding tightens on phones. */
.full-bleed {
  width: 100%;
}
.fb-hero {
  width: 100%;
  padding: 6rem 2rem;
  text-align: center;
  background: var(--invert-bg, #111827);
  color: var(--invert-text, #f9fafb);
}
.fb-title {
  margin: 0;
  font-size: 2.25rem;
  font-weight: 700;
}
.fb-body {
  width: 100%;
  padding: 4rem 2rem;
}
.fb-copy {
  margin: 0;
  color: var(--text, #1f2937);
}
@media (max-width: 48rem) {
  .fb-hero { padding: 4rem 1rem; }
  .fb-body { padding: 2rem 1rem; }
}
"#;

const STICKY_SIDEBAR_TPL: &str = r#"use dioxus::prelude::*;
{%- if wrap_pascal %}
use crate::components::{{ wrap_pascal }};
{%- endif %}

#[component]
pub fn {{ pascal }}() -> Element {
    rsx! {
{%- if wrap_pascal %}
        {{ wrap_pascal }} {
{%- endif %}
        div { class: "{{ root_class }} sticky-sidebar",
            aside { class: "ss-nav",
                nav { class: "ss-nav-list",
                    a { class: "ss-nav-item", "Overview" }
                    a { class: "ss-nav-item", "Details" }
                    a { class: "ss-nav-item", "Settings" }
                }
            }
            main { class: "ss-main", "Scrolling content (the sidebar stays put)." }
        }
{%- if wrap_pascal %}
        }
{%- endif %}
    }
}
"#;

const STICKY_SIDEBAR_CSS: &str = r#"/* Sticky sidebar: a horizontal nav strip on phones that becomes a sticky,
   full-height left column at 48rem while the main pane scrolls. */
.sticky-sidebar {
  display: flex;
  flex-direction: column;
  min-height: 100vh;
}
.ss-nav {
  padding: 1rem;
  border-bottom: 1px solid var(--border, #e5e7eb);
}
.ss-nav-list {
  display: flex;
  flex-wrap: wrap;
  gap: 1rem;
  font-size: 0.875rem;
}
.ss-nav-item {
  color: var(--text, #1f2937);
  text-decoration: none;
}
.ss-main {
  flex: 1;
  padding: 2rem;
}
@media (min-width: 48rem) {
  .sticky-sidebar {
    flex-direction: row;
  }
  .ss-nav {
    width: 16rem;
    flex-shrink: 0;
    position: sticky;
    top: 0;
    height: 100vh;
    overflow-y: auto;
    border-bottom: none;
    border-right: 1px solid var(--border, #e5e7eb);
  }
  .ss-nav-list {
    flex-direction: column;
  }
}
"#;

const MEGA_MENU_TPL: &str = r#"use dioxus::prelude::*;
{%- if wrap_pascal %}
use crate::components::{{ wrap_pascal }};
{%- endif %}

#[component]
pub fn {{ pascal }}() -> Element {
    let mut open = use_signal(|| false);
    rsx! {
{%- if wrap_pascal %}
        {{ wrap_pascal }} {
{%- endif %}
        div { class: "{{ root_class }} mega-menu",
            header { class: "mm-header",
                nav { class: "mm-bar",
                    span { class: "mm-brand", "{{ pascal }}" }
                    button { class: "mm-trigger", onclick: move |_| open.set(!open()), "Products" }
                }
                if open() {
                    div { class: "mm-panel",
                        div { class: "mm-col",
                            h3 { class: "mm-col-title", "Column" }
                            a { class: "mm-link", "Link" }
                            a { class: "mm-link", "Link" }
                        }
                        div { class: "mm-col",
                            h3 { class: "mm-col-title", "Column" }
                            a { class: "mm-link", "Link" }
                            a { class: "mm-link", "Link" }
                        }
                    }
                }
            }
            main { class: "mm-main", "Page content" }
        }
{%- if wrap_pascal %}
        }
{%- endif %}
    }
}
"#;

const MEGA_MENU_CSS: &str = r#"/* Mega menu: the trigger drops a full-width panel anchored under the header
   bar (single column on phones, four columns at 48rem). */
.mm-header {
  position: relative;
  border-bottom: 1px solid var(--border, #e5e7eb);
}
.mm-bar {
  display: flex;
  align-items: center;
  gap: 1.5rem;
  padding: 1rem;
}
.mm-brand {
  font-weight: 600;
  color: var(--text-strong, #111827);
}
.mm-trigger {
  font: inherit;
  font-weight: 500;
  color: inherit;
  background: none;
  border: none;
  cursor: pointer;
}
.mm-panel {
  position: absolute;
  left: 0;
  right: 0;
  top: 100%;
  z-index: 20;
  display: grid;
  grid-template-columns: 1fr;
  gap: 1.5rem;
  padding: 2rem;
  background: var(--surface, #ffffff);
  border-bottom: 1px solid var(--border, #e5e7eb);
  box-shadow: 0 10px 15px -3px rgba(0, 0, 0, 0.1);
}
.mm-col {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}
.mm-col-title {
  margin: 0;
  font-size: 0.875rem;
  font-weight: 600;
  color: var(--text-muted, #6b7280);
}
.mm-link {
  color: var(--text, #1f2937);
  text-decoration: none;
}
.mm-main {
  padding: 1.5rem;
}
@media (min-width: 48rem) {
  .mm-panel {
    grid-template-columns: repeat(4, 1fr);
  }
}
"#;

const DRAWER_TPL: &str = r#"use dioxus::prelude::*;
{%- if wrap_pascal %}
use crate::components::{{ wrap_pascal }};
{%- endif %}

#[component]
pub fn {{ pascal }}() -> Element {
    let mut open = use_signal(|| false);
    rsx! {
{%- if wrap_pascal %}
        {{ wrap_pascal }} {
{%- endif %}
        div { class: "{{ root_class }} drawer",
            header { class: "drawer-header",
                button { class: "drawer-toggle", onclick: move |_| open.set(!open()), "\u{2630}" }
                span { class: "drawer-brand", "{{ pascal }}" }
            }
            if open() {
                div { class: "drawer-scrim", onclick: move |_| open.set(false) }
            }
            aside {
                class: if open() { "drawer-panel drawer-panel--open" } else { "drawer-panel" },
                nav { class: "drawer-nav",
                    a { class: "drawer-nav-item", "Home" }
                    a { class: "drawer-nav-item", "Library" }
                    a { class: "drawer-nav-item", "Settings" }
                }
            }
            main { class: "drawer-main", "Content" }
        }
{%- if wrap_pascal %}
        }
{%- endif %}
    }
}
"#;

const DRAWER_CSS: &str = r#"/* Drawer: an off-canvas panel that slides in from the left over a scrim at
   every viewport size (toggle-driven, unlike admin_console's responsive
   promotion to a column — so this sheet is deliberately breakpoint-free). */
.drawer {
  position: relative;
  min-height: 100vh;
}
.drawer-header {
  display: flex;
  align-items: center;
  gap: 0.75rem;
  padding: 1rem;
  border-bottom: 1px solid var(--border, #e5e7eb);
}
.drawer-toggle {
  font: inherit;
  font-size: 1.25rem;
  line-height: 1;
  background: none;
  border: none;
  cursor: pointer;
}
.drawer-brand {
  font-weight: 600;
  color: var(--text-strong, #111827);
}
.drawer-scrim {
  position: fixed;
  inset: 0;
  z-index: 40;
  background: rgba(0, 0, 0, 0.4);
}
.drawer-panel {
  position: fixed;
  top: 0;
  bottom: 0;
  left: 0;
  z-index: 50;
  width: 18rem;
  padding: 1rem;
  background: var(--surface, #ffffff);
  border-right: 1px solid var(--border, #e5e7eb);
  transform: translateX(-100%);
  transition: transform 0.2s ease;
}
.drawer-panel--open {
  transform: translateX(0);
}
.drawer-nav {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  font-size: 0.875rem;
}
.drawer-nav-item {
  color: var(--text, #1f2937);
  text-decoration: none;
}
.drawer-main {
  padding: 1.5rem;
}
"#;

const CARD_GRID_TPL: &str = r#"use dioxus::prelude::*;
{%- if wrap_pascal %}
use crate::components::{{ wrap_pascal }};
{%- endif %}

#[component]
pub fn {{ pascal }}() -> Element {
    rsx! {
{%- if wrap_pascal %}
        {{ wrap_pascal }} {
{%- endif %}
        div { class: "{{ root_class }} card-grid",
            h1 { class: "cg-title", "{{ pascal }}" }
            div { class: "cg-grid",
                for _ in 0..6 {
                    div { class: "cg-card",
                        div { class: "cg-thumb" }
                        h3 { class: "cg-card-title", "Card title" }
                        p { class: "cg-card-body", "Card body text." }
                    }
                }
            }
        }
{%- if wrap_pascal %}
        }
{%- endif %}
    }
}
"#;

const CARD_GRID_CSS: &str = r#"/* Card grid: one column on phones, two at 40rem, three at 64rem. */
.card-grid {
  padding: 1.5rem;
}
.cg-title {
  margin: 0 0 1.5rem;
  font-size: 1.5rem;
  font-weight: 700;
  color: var(--text-strong, #111827);
}
.cg-grid {
  display: grid;
  grid-template-columns: 1fr;
  gap: 1.5rem;
}
.cg-card {
  padding: 1.25rem;
  border: 1px solid var(--border, #e5e7eb);
  border-radius: 0.75rem;
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.05);
}
.cg-thumb {
  height: 6rem;
  margin-bottom: 1rem;
  border-radius: 0.5rem;
  background: var(--surface-sunken, #f3f4f6);
}
.cg-card-title {
  margin: 0 0 0.25rem;
  font-weight: 600;
  color: var(--text-strong, #111827);
}
.cg-card-body {
  margin: 0;
  font-size: 0.875rem;
  color: var(--text-muted, #6b7280);
}
@media (min-width: 40rem) {
  .cg-grid { grid-template-columns: repeat(2, 1fr); }
}
@media (min-width: 64rem) {
  .cg-grid { grid-template-columns: repeat(3, 1fr); }
}
"#;

const EDITORIAL_TPL: &str = r#"use dioxus::prelude::*;
{%- if wrap_pascal %}
use crate::components::{{ wrap_pascal }};
{%- endif %}

#[component]
pub fn {{ pascal }}() -> Element {
    rsx! {
{%- if wrap_pascal %}
        {{ wrap_pascal }} {
{%- endif %}
        article { class: "{{ root_class }} editorial",
            h1 { class: "ed-headline", "{{ pascal }}" }
            p { class: "ed-standfirst", "Standfirst — a short dek that sets up the piece." }
            div { class: "ed-body",
                div { class: "ed-copy",
                    p { "Body copy in the wide column." }
                    p { "More body copy, asymmetric to the aside." }
                }
                aside { class: "ed-aside",
                    blockquote { class: "ed-quote", "A pull quote." }
                }
            }
        }
{%- if wrap_pascal %}
        }
{%- endif %}
    }
}
"#;

const EDITORIAL_CSS: &str = r#"/* Editorial: a centered measure with a stacked body on phones that splits into
   an asymmetric 2fr/1fr copy + aside at 48rem. */
.editorial {
  max-width: 56rem;
  margin: 0 auto;
  padding: 1.5rem;
}
.ed-headline {
  margin: 0;
  font-size: 2.25rem;
  font-weight: 700;
  letter-spacing: -0.025em;
  color: var(--text-strong, #111827);
}
.ed-standfirst {
  margin: 0.5rem 0 0;
  font-size: 1.125rem;
  color: var(--text-muted, #6b7280);
}
.ed-body {
  margin-top: 2rem;
  display: grid;
  grid-template-columns: 1fr;
  gap: 2rem;
}
.ed-copy {
  display: flex;
  flex-direction: column;
  gap: 1rem;
  line-height: 1.7;
  color: var(--text, #1f2937);
}
.ed-copy p {
  margin: 0;
}
.ed-aside {
  margin: 0;
}
.ed-quote {
  margin: 0;
  padding-left: 1rem;
  font-size: 1.25rem;
  font-style: italic;
  border-left: 4px solid var(--accent, #111827);
}
@media (min-width: 48rem) {
  .ed-body {
    grid-template-columns: 2fr 1fr;
  }
}
"#;

const HERO_SCROLL_TPL: &str = r#"use dioxus::prelude::*;
{%- if wrap_pascal %}
use crate::components::{{ wrap_pascal }};
{%- endif %}

#[component]
pub fn {{ pascal }}() -> Element {
    rsx! {
{%- if wrap_pascal %}
        {{ wrap_pascal }} {
{%- endif %}
        div { class: "{{ root_class }} hero-scroll",
            section { class: "hs-hero",
                h1 { class: "hs-title", "{{ pascal }}" }
                p { class: "hs-sub", "A large opening section above the fold." }
                button { class: "hs-cta", "Get started" }
            }
            section { class: "hs-body",
                p { class: "hs-copy", "Content below the fold." }
            }
        }
{%- if wrap_pascal %}
        }
{%- endif %}
    }
}
"#;

const HERO_SCROLL_CSS: &str = r#"/* Hero: a full-viewport inverted opening section above the fold, then a
   centered content measure below. Title shrinks on phones. */
.hs-hero {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  min-height: 100vh;
  padding: 2rem;
  text-align: center;
  background: var(--invert-bg, #111827);
  color: var(--invert-text, #f9fafb);
}
.hs-title {
  margin: 0;
  font-size: 3rem;
  font-weight: 700;
}
.hs-sub {
  margin: 1rem 0 0;
  font-size: 1.125rem;
  color: var(--invert-muted, #d1d5db);
}
.hs-cta {
  margin-top: 2rem;
  padding: 0.75rem 1.5rem;
  font: inherit;
  font-weight: 500;
  border: none;
  border-radius: 9999px;
  background: var(--invert-text, #f9fafb);
  color: var(--invert-bg, #111827);
  cursor: pointer;
}
.hs-body {
  max-width: 48rem;
  margin: 0 auto;
  padding: 5rem 1.5rem;
}
.hs-copy {
  margin: 0;
  color: var(--text, #1f2937);
}
@media (max-width: 48rem) {
  .hs-title { font-size: 2.25rem; }
}
"#;

const SPLIT_SCREEN_TPL: &str = r#"use dioxus::prelude::*;
{%- if wrap_pascal %}
use crate::components::{{ wrap_pascal }};
{%- endif %}

#[component]
pub fn {{ pascal }}() -> Element {
    rsx! {
{%- if wrap_pascal %}
        {{ wrap_pascal }} {
{%- endif %}
        div { class: "{{ root_class }} split-screen",
            div { class: "split-left",
                h1 { class: "split-title", "{{ pascal }}" }
            }
            div { class: "split-right",
                p { class: "split-copy", "Right panel" }
            }
        }
{%- if wrap_pascal %}
        }
{%- endif %}
    }
}
"#;

const SPLIT_SCREEN_CSS: &str = r#"/* Split screen: stacked panes on phones, two equal columns at 48rem; the left
   pane is inverted. */
.split-screen {
  display: grid;
  min-height: 100vh;
  grid-template-columns: 1fr;
}
.split-left {
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 3rem;
  background: var(--invert-bg, #111827);
  color: var(--invert-text, #f9fafb);
}
.split-title {
  margin: 0;
  font-size: 1.875rem;
  font-weight: 700;
}
.split-right {
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 3rem;
}
.split-copy {
  margin: 0;
  color: var(--text, #1f2937);
}
@media (min-width: 48rem) {
  .split-screen {
    grid-template-columns: 1fr 1fr;
  }
}
"#;

const SCROLL_STICKY_TPL: &str = r#"use dioxus::prelude::*;
{%- if wrap_pascal %}
use crate::components::{{ wrap_pascal }};
{%- endif %}

#[component]
pub fn {{ pascal }}() -> Element {
    rsx! {
{%- if wrap_pascal %}
        {{ wrap_pascal }} {
{%- endif %}
        div { class: "{{ root_class }} scroll-sticky",
            h1 { class: "sticky-title", "{{ pascal }}" }
            div { class: "sticky-body",
                div { class: "sticky-steps",
                    section { class: "sticky-step", "Step one — text scrolls past the sticky panel." }
                    section { class: "sticky-step", "Step two." }
                    section { class: "sticky-step", "Step three." }
                }
                div { class: "sticky-visual-wrap",
                    div { class: "sticky-visual", "Sticky visual" }
                }
            }
        }
{%- if wrap_pascal %}
        }
{%- endif %}
    }
}
"#;

const SCROLL_STICKY_CSS: &str = r#"/* Scroll-sticky: tall scrolling steps beside a panel that pins in place. The
   visual column is hidden on phones (nothing to pin against) and appears beside
   the steps at 48rem. */
.scroll-sticky {
  max-width: 72rem;
  margin: 0 auto;
  padding: 1.5rem;
}
.sticky-title {
  margin: 0 0 2rem;
  font-size: 1.5rem;
  font-weight: 700;
  color: var(--text-strong, #111827);
}
.sticky-body {
  display: grid;
  grid-template-columns: 1fr;
  gap: 3rem;
}
.sticky-steps {
  display: flex;
  flex-direction: column;
  gap: 60vh;
}
.sticky-visual-wrap {
  display: none;
}
.sticky-visual {
  position: sticky;
  top: 4rem;
  height: 70vh;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 1rem;
  color: var(--text-muted, #6b7280);
  background: var(--surface-sunken, #f3f4f6);
}
@media (min-width: 48rem) {
  .sticky-body {
    grid-template-columns: 1fr 1fr;
  }
  .sticky-visual-wrap {
    display: block;
  }
}
"#;

// ---------------------------------------------------------------------------
// `admin_console`: a responsive admin shell — a slide-in drawer on phones that
// becomes a sticky sidebar column from the 48rem breakpoint up, with in-page
// section state (single route, active-nav highlighting, pane swaps). It predates
// the shared token set above and uses arium's `--primary-color-*` /
// `--secondary-color-*` names (same neutral fallbacks).
// ---------------------------------------------------------------------------

const ADMIN_CONSOLE_TPL: &str = r#"use dioxus::prelude::*;
{%- if wrap_pascal %}
use crate::components::{{ wrap_pascal }};
{%- endif %}

#[component]
pub fn {{ pascal }}() -> Element {
    // `open` toggles the mobile drawer; `section` is the selected pane. The
    // `@media (min-width: 48rem)` rules in the stylesheet win at desktop, so the
    // sidebar is always a sticky column there regardless of `open` (the drawer
    // transform only bites on phones).
    let mut open = use_signal(|| false);
    let mut section = use_signal(|| 0usize);
    let nav = ["Overview", "Details", "Settings"];
    rsx! {
{%- if wrap_pascal %}
        {{ wrap_pascal }} {
{%- endif %}
        div { class: "{{ root_class }} admin-layout",
            button {
                class: "admin-hamburger",
                onclick: move |_| open.set(!open()),
                "\u{2630} Menu"
            }
            if open() {
                div { class: "admin-scrim", onclick: move |_| open.set(false) }
            }
            aside {
                class: if open() { "admin-sidebar admin-sidebar--open" } else { "admin-sidebar" },
                h2 { class: "admin-brand", "{{ pascal }}" }
                nav { class: "admin-nav",
                    for (i, label) in nav.iter().enumerate() {
                        button {
                            key: "{i}",
                            class: if section() == i { "admin-nav-item is-active" } else { "admin-nav-item" },
                            onclick: move |_| { section.set(i); open.set(false); },
                            "{label}"
                        }
                    }
                }
            }
            main { class: "admin-content",
                match section() {
                    0 => rsx! { p { "Overview pane" } },
                    1 => rsx! { p { "Details pane" } },
                    _ => rsx! { p { "Settings pane" } },
                }
            }
        }
{%- if wrap_pascal %}
        }
{%- endif %}
    }
}
"#;

// Mobile-first: base rules describe the phone (drawer) state; the >=48rem media
// query promotes the sidebar to a sticky grid column and hides the mobile chrome.
const ADMIN_CONSOLE_CSS: &str = r#"/* Admin console layout: sticky sidebar on desktop, hamburger-toggled
   slide-in drawer on phones. */
.admin-layout {
  position: relative;
  max-width: 64rem;
  margin: 2rem auto;
  padding: 0 1rem;
}

/* Mobile-only toggle. Hidden at the desktop breakpoint. */
.admin-hamburger {
  display: inline-flex;
  align-items: center;
  gap: 0.5rem;
  margin-bottom: 1rem;
  padding: 0.5rem 0.875rem;
  font-size: 0.9375rem;
  color: var(--secondary-color-2, #374151);
  background: var(--primary-color-4, #f3f4f6);
  border: 1px solid var(--primary-color-7, #d1d5db);
  border-radius: 0.5rem;
  cursor: pointer;
}

/* Dimmer behind the open drawer (mobile only). */
.admin-scrim {
  position: fixed;
  inset: 0;
  z-index: 40;
  background: rgba(0, 0, 0, 0.5);
}

.admin-sidebar {
  position: fixed;
  top: 0;
  left: 0;
  bottom: 0;
  z-index: 50;
  width: 15rem;
  display: flex;
  flex-direction: column;
  gap: 1rem;
  padding: 1.5rem 1rem;
  background: var(--primary-color-3, #f9fafb);
  border-right: 1px solid var(--primary-color-7, #d1d5db);
  overflow-y: auto;
  transform: translateX(-100%);
  transition: transform 0.2s ease;
}

.admin-sidebar--open {
  transform: translateX(0);
}

.admin-brand {
  margin: 0;
  font-size: 1.125rem;
  color: var(--secondary-color, #111827);
}

.admin-nav {
  display: flex;
  flex-direction: column;
  gap: 0.25rem;
}

.admin-nav-item {
  text-align: left;
  padding: 0.5rem 0.75rem;
  font-size: 0.9375rem;
  color: var(--secondary-color-3, #4b5563);
  background: transparent;
  border: 1px solid transparent;
  border-radius: 0.5rem;
  cursor: pointer;
}

.admin-nav-item:hover {
  background: var(--primary-color-5, #e5e7eb);
}

.admin-nav-item.is-active {
  color: var(--secondary-color, #111827);
  background: var(--primary-color-5, #e5e7eb);
  border-color: var(--primary-color-7, #d1d5db);
}

.admin-content {
  min-width: 0;
}

@media (min-width: 48rem) {
  .admin-hamburger,
  .admin-scrim {
    display: none;
  }

  .admin-layout {
    display: grid;
    grid-template-columns: 15rem 1fr;
    gap: 2rem;
    align-items: start;
  }

  /* Back into normal flow as a sticky column; the drawer transform and
     fixed positioning no longer apply. */
  .admin-sidebar {
    position: sticky;
    top: 2rem;
    z-index: auto;
    width: auto;
    transform: none;
    border-right: none;
    border: 1px solid var(--primary-color-7, #d1d5db);
    border-radius: 0.75rem;
  }
}
"#;

// ---------------------------------------------------------------------------
// Preview-skeleton helpers. These produce the approximate `RenderNode` tree the
// cockpit navigator renders — a coarse box model of the layout, not the markup.
// ---------------------------------------------------------------------------

fn txt(s: &str) -> RenderNode {
    RenderNode::Text {
        text: s.to_string(),
    }
}

fn el(tag: &str, class: Option<&str>, children: Vec<RenderNode>) -> RenderNode {
    RenderNode::Element {
        tag: tag.to_string(),
        class: class.map(str::to_string),
        attrs: Default::default(),
        children,
    }
}

/// A small filler box used to sketch a region in a preview skeleton.
fn box_(class: &str, label: &str) -> RenderNode {
    el("div", Some(class), vec![txt(label)])
}

/// The context vars every library template reads. Documented on each descriptor
/// so `get_registry` consumers can see what a layout's template expects.
fn common_context_vars() -> Vec<String> {
    vec![
        "pascal: PascalCase screen name".to_string(),
        "snake: snake_case screen name".to_string(),
        "wrap_pascal: optional wrapper component (from `wrap_with`)".to_string(),
        "root_class: root-element hook class (`screen {snake}` or your `class:` override)"
            .to_string(),
    ]
}

/// Build a self-contained library layout descriptor: a `complex: false` layout
/// whose markup uses semantic class names and whose structure travels in
/// `styles` (emitted as `assets/{snake}.css` on scaffold). `requires` stays
/// `None` — none of these need a CSS toolchain.
fn descriptor(
    id: &str,
    label: &str,
    nav_rank: u8,
    template: &str,
    styles: &str,
    preview_nodes: Vec<RenderNode>,
) -> LayoutDescriptor {
    LayoutDescriptor {
        id: id.to_string(),
        label: label.to_string(),
        nav_rank,
        template: Some(template.to_string()),
        complex: false,
        styles: Some(styles.to_string()),
        requires: None,
        context_vars: common_context_vars(),
        preview: PreviewSkeleton {
            nodes: preview_nodes,
            behavior: None,
        },
    }
}

/// The structural layout library, as registry descriptors. Seeded into the
/// registry by [`builtin_layouts`](crate::registry::builtin) so every project
/// can scaffold a screen with `template: { kind: <id> }`.
pub(crate) fn library_layouts() -> Vec<LayoutDescriptor> {
    vec![
        descriptor(
            "holy_grail",
            "Holy Grail",
            10,
            HOLY_GRAIL_TPL,
            HOLY_GRAIL_CSS,
            vec![el(
                "div",
                Some("holy-grail"),
                vec![
                    box_("hg-header", "header"),
                    el(
                        "div",
                        Some("hg-body"),
                        vec![
                            box_("hg-nav", "nav"),
                            box_("hg-main", "main"),
                            box_("hg-aside", "aside"),
                        ],
                    ),
                    box_("hg-footer", "footer"),
                ],
            )],
        ),
        descriptor(
            "bento_grid",
            "Bento",
            11,
            BENTO_GRID_TPL,
            BENTO_GRID_CSS,
            vec![el(
                "div",
                Some("bento"),
                vec![
                    box_("bento-feature", "feature"),
                    box_("bento-tile", "tile"),
                    box_("bento-tile", "tile"),
                    box_("bento-wide", "wide"),
                ],
            )],
        ),
        descriptor(
            "masonry",
            "Masonry",
            12,
            MASONRY_TPL,
            MASONRY_CSS,
            vec![el(
                "div",
                Some("masonry"),
                vec![
                    box_("masonry-col", "col"),
                    box_("masonry-col", "col"),
                    box_("masonry-col", "col"),
                ],
            )],
        ),
        descriptor(
            "full_bleed",
            "Full-bleed",
            13,
            FULL_BLEED_TPL,
            FULL_BLEED_CSS,
            vec![el(
                "div",
                Some("full-bleed"),
                vec![box_("fb-hero", "hero"), box_("fb-body", "body")],
            )],
        ),
        descriptor(
            "sticky_sidebar",
            "Sticky Sidebar",
            14,
            STICKY_SIDEBAR_TPL,
            STICKY_SIDEBAR_CSS,
            vec![el(
                "div",
                Some("sticky-sidebar"),
                vec![box_("ss-nav", "nav"), box_("ss-main", "main")],
            )],
        ),
        descriptor(
            "mega_menu",
            "Mega Menu",
            15,
            MEGA_MENU_TPL,
            MEGA_MENU_CSS,
            vec![el(
                "div",
                Some("mega-menu"),
                vec![
                    box_("mm-bar", "nav bar"),
                    el(
                        "div",
                        Some("mm-panel"),
                        vec![box_("mm-col", "col"), box_("mm-col", "col")],
                    ),
                    box_("mm-main", "main"),
                ],
            )],
        ),
        descriptor(
            "drawer",
            "Drawer",
            16,
            DRAWER_TPL,
            DRAWER_CSS,
            vec![el(
                "div",
                Some("drawer"),
                vec![
                    box_("drawer-header", "header"),
                    box_("drawer-panel", "drawer"),
                    box_("drawer-main", "main"),
                ],
            )],
        ),
        descriptor(
            "card_grid",
            "Card Grid",
            17,
            CARD_GRID_TPL,
            CARD_GRID_CSS,
            vec![el(
                "div",
                Some("card-grid"),
                vec![
                    box_("card", "card"),
                    box_("card", "card"),
                    box_("card", "card"),
                ],
            )],
        ),
        descriptor(
            "editorial",
            "Editorial",
            18,
            EDITORIAL_TPL,
            EDITORIAL_CSS,
            vec![el(
                "div",
                Some("editorial"),
                vec![
                    box_("ed-headline", "headline"),
                    el(
                        "div",
                        Some("ed-body"),
                        vec![box_("ed-copy", "copy"), box_("ed-quote", "pull quote")],
                    ),
                ],
            )],
        ),
        descriptor(
            "hero_scroll",
            "Hero",
            19,
            HERO_SCROLL_TPL,
            HERO_SCROLL_CSS,
            vec![el(
                "div",
                Some("hero-scroll"),
                vec![box_("hs-hero", "hero"), box_("hs-body", "below the fold")],
            )],
        ),
        descriptor(
            "split_screen",
            "Split",
            20,
            SPLIT_SCREEN_TPL,
            SPLIT_SCREEN_CSS,
            vec![el(
                "div",
                Some("split-screen"),
                vec![box_("split-left", "left"), box_("split-right", "right")],
            )],
        ),
        descriptor(
            "scroll_sticky",
            "Sticky Sections",
            21,
            SCROLL_STICKY_TPL,
            SCROLL_STICKY_CSS,
            vec![el(
                "div",
                Some("scroll-sticky"),
                vec![
                    box_("sticky-steps", "steps"),
                    box_("sticky-visual", "sticky visual"),
                ],
            )],
        ),
        descriptor(
            "admin_console",
            "Admin Console",
            22,
            ADMIN_CONSOLE_TPL,
            ADMIN_CONSOLE_CSS,
            vec![el(
                "div",
                Some("admin-console"),
                vec![
                    box_("ac-hamburger", "menu"),
                    box_("ac-sidebar", "nav"),
                    box_("ac-main", "section"),
                ],
            )],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::tools::dsl::generate::build_screen_body;
    use crate::tools::dsl::render_model::build_render_models;
    use crate::tools::dsl::types::{DslDoc, DslScreen, DslScreenTemplate};

    fn template(kind: &str) -> DslScreenTemplate {
        DslScreenTemplate {
            kind: kind.into(),
            endpoint: None,
            item_type: None,
            on_submit: None,
            redirect_to: None,
            fields: vec![],
            store: None,
            label_field: None,
            checkbox_field: None,
            class: None,
            body: None,
            styled: None,
            compose_style: None,
            crud: None,
        }
    }

    fn screen(kind: &str) -> DslScreen {
        DslScreen {
            name: "DemoScreen".into(),
            route: "/demo".into(),
            wrap_with: None,
            template: Some(template(kind)),
            replace_route: false,
            route_params: vec![],
        }
    }

    fn layouts() -> BTreeMap<String, LayoutDescriptor> {
        library_layouts()
            .into_iter()
            .map(|l| (l.id.clone(), l))
            .collect()
    }

    #[test]
    fn every_library_layout_renders_a_valid_component() {
        let layouts = layouts();
        assert_eq!(layouts.len(), 13, "expected 13 library layouts");
        for id in layouts.keys() {
            let body =
                build_screen_body(std::env::temp_dir().as_path(), &screen(id), &[], &layouts)
                    .unwrap_or_else(|e| panic!("layout {id} failed to render: {e}"));
            assert!(body.contains("use dioxus::prelude::*;"), "{id}:\n{body}");
            assert!(body.contains("#[component]"), "{id}:\n{body}");
            assert!(
                body.contains("pub fn DemoScreen() -> Element"),
                "{id}:\n{body}"
            );
            assert!(body.contains("rsx!"), "{id}:\n{body}");
            // The root carries the hook class so `class:` overrides still apply.
            assert!(
                body.contains("screen demo_screen"),
                "{id} should keep the root_class hook:\n{body}"
            );
        }
    }

    #[test]
    fn interactive_layouts_wire_a_toggle_signal() {
        let layouts = layouts();
        for id in ["mega_menu", "drawer"] {
            let body =
                build_screen_body(std::env::temp_dir().as_path(), &screen(id), &[], &layouts)
                    .unwrap();
            assert!(
                body.contains("use_signal(|| false)"),
                "{id} should declare a toggle signal:\n{body}"
            );
        }
    }

    #[test]
    fn wrap_with_wraps_the_layout_root() {
        let layouts = layouts();
        let mut sc = screen("holy_grail");
        sc.wrap_with = Some("Protected".into());
        let body = build_screen_body(std::env::temp_dir().as_path(), &sc, &[], &layouts).unwrap();
        assert!(
            body.contains("use crate::components::Protected;"),
            "expected wrapper import:\n{body}"
        );
        assert!(
            body.contains("Protected {"),
            "expected wrapper element:\n{body}"
        );
    }

    #[test]
    fn every_library_layout_is_self_contained() {
        // The whole structural library carries its own CSS — semantic class names
        // defined in `styles`, emitted as `assets/{snake}.css` on scaffold — so no
        // layout depends on a CSS toolchain (`requires` stays None). That's the
        // contract `generate_screen` relies on to emit a sheet for every
        // structural screen. The sheet is also responsive: every layout reflows
        // at a breakpoint, except `drawer`, whose off-canvas overlay is
        // deliberately viewport-agnostic (that's what distinguishes it from
        // `admin_console`, which promotes to a column at 48rem).
        for l in library_layouts() {
            let styles = l.styles.as_deref().unwrap_or_default();
            assert!(
                !styles.trim().is_empty(),
                "{} must carry its structural CSS in `styles`",
                l.id
            );
            assert_eq!(
                l.requires, None,
                "{} is self-contained and should declare no toolchain `requires`",
                l.id
            );
            if l.id != "drawer" {
                assert!(
                    styles.contains("@media"),
                    "{} is a canonical shape and should be responsive (no @media found)",
                    l.id
                );
            }
        }
    }

    #[test]
    fn every_library_layout_has_a_preview_skeleton() {
        for l in library_layouts() {
            assert!(
                !l.preview.nodes.is_empty(),
                "{} should ship a non-empty preview skeleton",
                l.id
            );
        }
    }

    #[test]
    fn render_model_emits_preview_nodes_for_a_library_layout() {
        let doc: DslDoc = serde_yml::from_str(
            r#"version: "1"
screens:
  - name: Dashboard
    route: /dashboard
    template:
      kind: bento_grid
"#,
        )
        .unwrap();
        let models = build_render_models(&doc, &layouts());
        let m = models
            .iter()
            .find(|m| m.screen == "Dashboard")
            .expect("a render model for the bento_grid screen");
        assert_eq!(m.kind, "bento_grid");
        assert_eq!(m.layout, "bento_grid");
        assert!(
            !m.nodes.is_empty(),
            "the layout's preview skeleton should populate nodes"
        );
    }
}
