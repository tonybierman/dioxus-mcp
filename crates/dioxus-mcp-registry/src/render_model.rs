//! The structured, renderable description of a screen — the wire contract
//! between the server's dry-run and the playground's preview.
//!
//! Historically only the resource-synthesized screens (`resource_list` /
//! `resource_form` / `resource_edit_form`) got a [`RenderModel`], carrying typed
//! `columns`/`fields`. The registry work generalizes this to *every* layout via
//! a generic [`RenderNode`] tree plus a small closed [`Behavior`] set, so the
//! playground can render any layout from one interpreter. The new fields are
//! additive (`#[serde(default, skip_serializing_if)]`) so the existing wire
//! format is unchanged until the server starts populating them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A resolved, renderable description of one screen. Unlike raw RSX `previews`
/// (Rust text), this is structured data a browser client tree-walks directly.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RenderModel {
    /// PascalCase screen name.
    pub screen: String,
    /// Resolved layout id (`resource_list` | `resource_form` |
    /// `resource_edit_form` | `client_crud` | `empty` | …). Kept named `kind`
    /// for wire/back-compat with existing clients and the navigator.
    pub kind: String,
    /// The screen's route.
    #[serde(default)]
    pub route: String,
    /// PascalCase model/item type the screen is built around.
    #[serde(default)]
    pub item_type: String,
    /// Root `div` class the generated screen uses (e.g. `screen product_list`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_class: Option<String>,
    /// Table columns for `resource_list` (from the model's fields). `ty` is the
    /// Rust type, so the client can synthesize type-appropriate mock cells.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<RenderField>,
    /// Form inputs for `resource_form` / `resource_edit_form`. `ty` is the input
    /// kind (text/email/number/checkbox/textarea/…).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<RenderField>,
    /// `resource_list`: server fn that returns the rows (shown in the
    /// "mock data" note).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_endpoint: Option<String>,
    /// `resource_list`: route to the "new" screen, for the toolbar link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_route: Option<String>,

    // --- generic preview path (additive; populated as layouts move onto the
    // registry, empty until then so the existing wire format is unchanged) ---
    /// Explicit layout id (mirrors `kind`; present once a screen is rendered via
    /// the registry's generic path).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub layout: String,
    /// Active theme id at preview time, so the client can colorize from tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// Generic preview tree — a layout's `PreviewSkeleton` with its slots filled
    /// from resolved screen data. The single generic interpreter tree-walks this.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<RenderNode>,
    /// Interaction model the generic interpreter dispatches on (keeps e.g.
    /// `client_crud` live without a per-kind component).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior: Option<Behavior>,
}

/// A column or input field within a [`RenderModel`].
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RenderField {
    /// snake_case field name.
    pub name: String,
    /// Human-readable label (Title Case).
    pub label: String,
    /// Rust type (columns) or HTML input kind (fields).
    pub ty: String,
}

/// A node in a layout's generic preview tree. Internally tagged on `t` so a
/// descriptor author writes `{ t = "element", tag = "div", … }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum RenderNode {
    /// A DOM element.
    Element {
        tag: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        class: Option<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        attrs: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        children: Vec<RenderNode>,
    },
    /// A literal text node.
    Text { text: String },
    /// A structural placeholder the interpreter expands with live behavior or
    /// resolved screen data (form fields, table header/rows, the crud list).
    Slot { slot: Slot },
}

/// The structural placeholders a `PreviewSkeleton` can leave for the interpreter
/// to fill from a screen's resolved fields/columns + live behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Slot {
    /// Inputs from `RenderModel.fields`.
    FormFields,
    /// `<th>` cells from `RenderModel.columns`.
    TableHeader,
    /// A few mock `<tr>` rows synthesized from `RenderModel.columns`.
    TableMockRows,
    /// The live, signal-backed item list for `Behavior::ClientCrud`.
    CrudList,
}

/// The closed set of interactive behaviors the generic interpreter implements.
/// Anything not here renders statically from `nodes`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "behavior", rename_all = "snake_case")]
pub enum Behavior {
    /// No interactivity; render `nodes` as-is.
    Static,
    /// In-memory list with add/toggle/delete (today's `client_crud`).
    ClientCrud {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label_field: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkbox_field: Option<String>,
        #[serde(default)]
        enter_only: bool,
        #[serde(default)]
        item_label: String,
    },
    /// Mock-row list whose rows link to the edit screen via the fake router.
    ResourceList {
        #[serde(default)]
        edit_target: bool,
    },
    /// Form whose submit/back affordances navigate via the fake router.
    ResourceForm {
        #[serde(default)]
        back_to_list: bool,
    },
}
