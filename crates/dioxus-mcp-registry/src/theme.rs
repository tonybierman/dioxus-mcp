//! Theme descriptors — design tokens plus an optional class→style map. A theme
//! is pure data: the cockpit injects `tokens` as CSS custom properties, and the
//! generated `vanilla-css` starter sheet is emitted from the same tokens.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Design tokens. String values keep the schema open (a color is `#0f1115` or
/// `var(--x)`, a space is `0.5rem`, etc.) and trivially serializable to CSS.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ThemeTokens {
    /// Named colors: `bg`, `panel`, `border`, `text`, `muted`, `accent`,
    /// `error`, `code`, `surface`, `on_accent`, …
    #[serde(default)]
    pub color: BTreeMap<String, String>,
    /// Spacing scale: `1`..`6` → rem.
    #[serde(default)]
    pub space: BTreeMap<String, String>,
    /// Corner radii: `sm`/`md`/`lg`.
    #[serde(default)]
    pub radius: BTreeMap<String, String>,
    /// Font tokens: `family`, `mono`, `size_base`.
    #[serde(default)]
    pub font: BTreeMap<String, String>,
}

/// One theme. `kind` is the styling family that drives which codegen path a
/// `client_crud` screen takes (`unstyled` | `tailwind` | `vanilla_css`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ThemeDescriptor {
    /// Stable id (`dark`, `light`, `tailwind`, `vanilla-css`).
    pub id: String,
    /// Human label for the cockpit theme selector.
    #[serde(default)]
    pub label: String,
    /// Styling family: `unstyled` | `tailwind` | `vanilla_css`.
    #[serde(default)]
    pub kind: String,
    /// Design tokens, injected as CSS vars / emitted into generated CSS.
    #[serde(default)]
    pub tokens: ThemeTokens,
    /// Optional class → inline-style map, used to colorize preview nodes and to
    /// emit the `vanilla-css` starter rules (e.g. `.compose` → `display:flex;…`).
    #[serde(default)]
    pub class_styles: BTreeMap<String, String>,
}
