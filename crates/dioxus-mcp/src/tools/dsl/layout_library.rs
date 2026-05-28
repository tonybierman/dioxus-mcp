//! The built-in library of *structural* screen layouts — page-level shells
//! (holy-grail, bento grid, split-screen, …) an agent can pick via a screen's
//! `template.kind`. Each is a `complex: false` [`LayoutDescriptor`]: a minijinja
//! `template` (rendered by `render_registry_layout` into a full component file,
//! exactly like the built-in `SCREEN_TPL`) plus a `preview` skeleton the cockpit
//! navigator tree-walks. They're seeded into the registry by
//! [`builtin_layouts`](crate::registry) so they ship with every project and show
//! up in `get_registry`.
//!
//! Styling is Tailwind utility classes (mirrors the `styled: tailwind` preset on
//! `client_crud`): the generated Rust compiles with or without Tailwind present;
//! Tailwind only decides whether it *looks* styled. The structural classes are
//! fixed in the template — `{{ root_class }}` (default `screen {snake}`, or the
//! screen's `class:` override) rides along as a customization hook. To restyle a
//! layout, edit the generated file; `class:` won't replace the structure.
//!
//! Templates use only the context the runtime-layout renderer provides
//! (`pascal`, `snake`, `wrap_pascal`, `root_class`); `wrap_with` is honored via
//! conditional wrapper braces so a guard like `ProtectedRoute` can sit outside
//! the layout root.

use dioxus_mcp_registry::{LayoutDescriptor, PreviewSkeleton, RenderNode};

// ---------------------------------------------------------------------------
// Templates. Each emits a full component file. `{%- if wrap_pascal %}` wraps the
// layout root in the guard component without duplicating the markup.
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
        div { class: "{{ root_class }} min-h-screen grid grid-rows-[auto_1fr_auto] grid-cols-1 md:grid-cols-[200px_1fr_240px]",
            header { class: "md:col-span-3 border-b border-gray-200 p-4 font-semibold", "{{ pascal }}" }
            nav { class: "border-r border-gray-200 p-4 space-y-2 hidden md:block", "Sidebar" }
            main { class: "p-6", "Main content" }
            aside { class: "border-l border-gray-200 p-4 hidden md:block text-sm text-gray-500", "Aside" }
            footer { class: "md:col-span-3 border-t border-gray-200 p-4 text-sm text-gray-500", "Footer" }
        }
{%- if wrap_pascal %}
        }
{%- endif %}
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
        div { class: "{{ root_class }} p-6",
            h1 { class: "text-2xl font-bold mb-6", "{{ pascal }}" }
            div { class: "grid grid-cols-2 md:grid-cols-4 auto-rows-[150px] gap-4",
                div { class: "col-span-2 row-span-2 rounded-2xl bg-gray-100 p-5", "Feature" }
                div { class: "rounded-2xl bg-gray-100 p-5", "Tile" }
                div { class: "rounded-2xl bg-gray-100 p-5", "Tile" }
                div { class: "col-span-2 rounded-2xl bg-gray-100 p-5", "Wide tile" }
                div { class: "rounded-2xl bg-gray-100 p-5", "Tile" }
                div { class: "rounded-2xl bg-gray-100 p-5", "Tile" }
            }
        }
{%- if wrap_pascal %}
        }
{%- endif %}
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
        div { class: "{{ root_class }} p-6",
            h1 { class: "text-2xl font-bold mb-6", "{{ pascal }}" }
            div { class: "columns-2 md:columns-3 gap-4 [&>*]:mb-4 [&>*]:break-inside-avoid",
                div { class: "rounded-xl bg-gray-100 p-4 h-32", "Item" }
                div { class: "rounded-xl bg-gray-100 p-4 h-56", "Item" }
                div { class: "rounded-xl bg-gray-100 p-4 h-40", "Item" }
                div { class: "rounded-xl bg-gray-100 p-4 h-48", "Item" }
                div { class: "rounded-xl bg-gray-100 p-4 h-28", "Item" }
                div { class: "rounded-xl bg-gray-100 p-4 h-44", "Item" }
            }
        }
{%- if wrap_pascal %}
        }
{%- endif %}
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
        div { class: "{{ root_class }} w-full",
            section { class: "w-full bg-gray-900 text-white px-8 py-24 text-center",
                h1 { class: "text-4xl font-bold", "{{ pascal }}" }
            }
            section { class: "w-full px-8 py-16",
                p { class: "text-gray-700", "Full-bleed content stretches edge to edge — no max-width container." }
            }
        }
{%- if wrap_pascal %}
        }
{%- endif %}
    }
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
        div { class: "{{ root_class }} flex min-h-screen",
            aside { class: "w-64 shrink-0 sticky top-0 h-screen overflow-y-auto border-r border-gray-200 p-4",
                nav { class: "space-y-2 text-sm",
                    a { class: "block", "Overview" }
                    a { class: "block", "Details" }
                    a { class: "block", "Settings" }
                }
            }
            main { class: "flex-1 p-8 space-y-6", "Scrolling content (the sidebar stays put)." }
        }
{%- if wrap_pascal %}
        }
{%- endif %}
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
        div { class: "{{ root_class }}",
            header { class: "relative border-b border-gray-200",
                nav { class: "flex items-center gap-6 p-4",
                    span { class: "font-semibold", "{{ pascal }}" }
                    button { class: "font-medium", onclick: move |_| open.set(!open()), "Products" }
                }
                if open() {
                    div { class: "absolute left-0 right-0 top-full grid grid-cols-2 md:grid-cols-4 gap-6 border-b border-gray-200 bg-white p-8 shadow-lg",
                        div { class: "space-y-2",
                            h3 { class: "text-sm font-semibold text-gray-500", "Column" }
                            a { class: "block", "Link" }
                            a { class: "block", "Link" }
                        }
                        div { class: "space-y-2",
                            h3 { class: "text-sm font-semibold text-gray-500", "Column" }
                            a { class: "block", "Link" }
                            a { class: "block", "Link" }
                        }
                    }
                }
            }
            main { class: "p-6", "Page content" }
        }
{%- if wrap_pascal %}
        }
{%- endif %}
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
        div { class: "{{ root_class }} relative min-h-screen",
            header { class: "flex items-center gap-3 border-b border-gray-200 p-4",
                button { class: "text-xl leading-none", onclick: move |_| open.set(!open()), "\u{2630}" }
                span { class: "font-semibold", "{{ pascal }}" }
            }
            if open() {
                div { class: "fixed inset-0 z-40 bg-black/40", onclick: move |_| open.set(false) }
            }
            aside {
                class: if open() {
                    "fixed inset-y-0 left-0 z-50 w-72 translate-x-0 transition-transform border-r border-gray-200 bg-white p-4"
                } else {
                    "fixed inset-y-0 left-0 z-50 w-72 -translate-x-full transition-transform border-r border-gray-200 bg-white p-4"
                },
                nav { class: "space-y-2 text-sm",
                    a { class: "block", "Home" }
                    a { class: "block", "Library" }
                    a { class: "block", "Settings" }
                }
            }
            main { class: "p-6", "Content" }
        }
{%- if wrap_pascal %}
        }
{%- endif %}
    }
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
        div { class: "{{ root_class }} p-6",
            h1 { class: "text-2xl font-bold mb-6", "{{ pascal }}" }
            div { class: "grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-6",
                for _ in 0..6 {
                    div { class: "rounded-xl border border-gray-200 p-5 shadow-sm",
                        div { class: "h-24 rounded-lg bg-gray-100 mb-4" }
                        h3 { class: "font-semibold", "Card title" }
                        p { class: "text-sm text-gray-500", "Card body text." }
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
        article { class: "{{ root_class }} mx-auto max-w-4xl p-6",
            h1 { class: "text-4xl font-bold tracking-tight", "{{ pascal }}" }
            p { class: "mt-2 text-lg text-gray-500", "Standfirst — a short dek that sets up the piece." }
            div { class: "mt-8 grid grid-cols-1 md:grid-cols-3 gap-8",
                div { class: "md:col-span-2 space-y-4 leading-relaxed text-gray-800",
                    p { "Body copy in the wide column." }
                    p { "More body copy, asymmetric to the aside." }
                }
                aside { class: "space-y-4",
                    blockquote { class: "border-l-4 border-gray-900 pl-4 text-xl italic", "A pull quote." }
                }
            }
        }
{%- if wrap_pascal %}
        }
{%- endif %}
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
        div { class: "{{ root_class }}",
            section { class: "flex min-h-screen flex-col items-center justify-center bg-gray-900 text-white text-center p-8",
                h1 { class: "text-5xl font-bold", "{{ pascal }}" }
                p { class: "mt-4 text-lg text-gray-300", "A large opening section above the fold." }
                button { class: "mt-8 rounded-full bg-white px-6 py-3 font-medium text-gray-900", "Get started" }
            }
            section { class: "mx-auto max-w-3xl px-6 py-20 space-y-6",
                p { class: "text-gray-700", "Content below the fold." }
            }
        }
{%- if wrap_pascal %}
        }
{%- endif %}
    }
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
        div { class: "{{ root_class }} grid min-h-screen grid-cols-1 md:grid-cols-2",
            div { class: "flex items-center justify-center bg-gray-900 text-white p-12",
                h1 { class: "text-3xl font-bold", "{{ pascal }}" }
            }
            div { class: "flex items-center justify-center p-12",
                p { class: "text-gray-700", "Right panel" }
            }
        }
{%- if wrap_pascal %}
        }
{%- endif %}
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
        div { class: "{{ root_class }} mx-auto max-w-6xl p-6",
            h1 { class: "text-2xl font-bold mb-8", "{{ pascal }}" }
            div { class: "grid grid-cols-1 md:grid-cols-2 gap-12",
                div { class: "space-y-[60vh]",
                    section { "Step one — text scrolls past the sticky panel." }
                    section { "Step two." }
                    section { "Step three." }
                }
                div { class: "hidden md:block",
                    div { class: "sticky top-16 h-[70vh] rounded-2xl bg-gray-100 flex items-center justify-center text-gray-500", "Sticky visual" }
                }
            }
        }
{%- if wrap_pascal %}
        }
{%- endif %}
    }
}
"#;

// ---------------------------------------------------------------------------
// `admin_console` is the library's *self-contained* layout: unlike the twelve
// Tailwind-utility shells above, it styles via semantic class names and carries
// the CSS that defines them in `ADMIN_CONSOLE_CSS`, emitted alongside the screen
// as `assets/{snake}.css` on scaffold. That makes it structurally reproducible
// in any project regardless of CSS toolchain — the reference example of a
// `styles`-bearing layout (`requires: None`). Responsive: a slide-in drawer on
// phones that becomes a sticky sidebar column from the 48rem breakpoint up, with
// in-page section state (single route, active-nav highlighting, pane swaps).
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

// The structural CSS the markup depends on. Theme tokens are kept as
// `var(--token, <neutral>)` so the layout adopts a project's theme when present
// and falls back to neutral grays standalone. Mobile-first: base rules describe
// the phone (drawer) state; the >=48rem media query promotes the sidebar to a
// sticky grid column and hides the mobile-only chrome.
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

fn descriptor(
    id: &str,
    label: &str,
    nav_rank: u8,
    template: &str,
    preview_nodes: Vec<RenderNode>,
) -> LayoutDescriptor {
    LayoutDescriptor {
        id: id.to_string(),
        label: label.to_string(),
        nav_rank,
        template: Some(template.to_string()),
        complex: false,
        // Default to the utility-styled convention: Tailwind classes baked into
        // the template, no CSS carried, the toolchain declared instead. The
        // self-contained `admin_console` overrides `styles`/`requires` after
        // construction (it styles via semantic classes + carried CSS).
        styles: None,
        requires: Some("tailwind".to_string()),
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
            vec![el(
                "div",
                Some("scroll-sticky"),
                vec![
                    box_("sticky-steps", "steps"),
                    box_("sticky-visual", "sticky visual"),
                ],
            )],
        ),
        admin_console(),
    ]
}

/// The self-contained admin-console shell. Built like the others but carries its
/// own structural CSS (`styles`) and so declares no toolchain `requires` — the
/// scaffold emits `assets/{snake}.css` so the structure survives in any project.
fn admin_console() -> LayoutDescriptor {
    let mut d = descriptor(
        "admin_console",
        "Admin Console",
        22,
        ADMIN_CONSOLE_TPL,
        vec![el(
            "div",
            Some("admin-console"),
            vec![
                box_("ac-hamburger", "menu"),
                box_("ac-sidebar", "nav"),
                box_("ac-main", "section"),
            ],
        )],
    );
    d.styles = Some(ADMIN_CONSOLE_CSS.to_string());
    d.requires = None;
    d
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
    fn every_library_layout_is_style_well_formed() {
        // Each library layout is styled exactly one way: either it bakes Tailwind
        // utilities into the markup (so it declares `requires: tailwind` and
        // carries no CSS), or it styles via semantic classes and carries the CSS
        // in `styles` (so it needs no toolchain `requires`). Never both, never
        // neither — that's the contract scaffold relies on to either emit a
        // sheet or surface a toolchain advisory.
        for l in library_layouts() {
            let carries_css = l.styles.as_deref().is_some_and(|s| !s.trim().is_empty());
            let declares_toolchain = l.requires.is_some();
            assert!(
                carries_css ^ declares_toolchain,
                "{} must either carry `styles` or declare `requires`, not both/neither \
                 (styles: {carries_css}, requires: {declares_toolchain})",
                l.id
            );
            if l.id == "admin_console" {
                assert!(carries_css, "admin_console is the self-contained layout");
            } else {
                assert_eq!(
                    l.requires.as_deref(),
                    Some("tailwind"),
                    "{} is utility-styled and should declare its Tailwind dependency",
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
