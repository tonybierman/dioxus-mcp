use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslDoc {
    /// Spec version. Must equal "1".
    pub version: String,
    #[serde(default)]
    pub models: Vec<DslModel>,
    #[serde(default)]
    pub stores: Vec<DslStore>,
    #[serde(default)]
    pub client_stores: Vec<DslClientStore>,
    #[serde(default)]
    pub server_fns: Vec<DslServerFn>,
    #[serde(default)]
    pub signals: Vec<DslSignal>,
    #[serde(default)]
    pub sockets: Vec<DslSocket>,
    #[serde(default)]
    pub feeds: Vec<DslFeed>,
    #[serde(default)]
    pub components: Vec<DslComponent>,
    #[serde(default)]
    pub forms: Vec<DslForm>,
    #[serde(default)]
    pub lists: Vec<DslList>,
    #[serde(default)]
    pub tables: Vec<DslTable>,
    #[serde(default)]
    pub session_states: Vec<DslSessionState>,
    #[serde(default)]
    pub login_screens: Vec<DslLoginScreen>,
    #[serde(default)]
    pub protected_routes: Vec<DslProtectedRoute>,
    #[serde(default)]
    pub screens: Vec<DslScreen>,
    /// Meta-primitive: each entry fans out into one model, one store, five
    /// server fns, and two screens (list + new). Expanded before preflight.
    #[serde(default)]
    pub resources: Vec<DslResource>,
    /// In-place edits to items that already exist on disk. Useful when
    /// iterating: add a prop to a generated component, add an arg to a server
    /// fn, add a field to a model. Each entry is idempotent.
    #[serde(default)]
    pub modify: Vec<DslModify>,
    /// Delete entire on-disk items: a Routable variant (and its `#[route(...)]`
    /// attribute), a component file (and its `mod.rs` entry), a model, or a
    /// server fn. Useful when scaffolding into a starter template (`dx new`)
    /// to clear demo Route variants / Hero components before adding your own.
    /// Removes run *first* in an execute_code call — after preflight, before
    /// any create/modify steps — so a single doc can replace a demo
    /// component with your own.
    #[serde(default)]
    pub remove: Vec<DslRemove>,
    /// Official Dioxus 0.7 components from the `dx components add` catalog to
    /// pull into this project (e.g. `["button", "dialog", "calendar"]`). Names
    /// are validated against the 45-entry catalog surfaced by
    /// `get_dsl_spec { sections: [components] }`. execute_code shells out to
    /// `dx components add <name>` for each valid entry (per-command 180s
    /// timeout); on failure (`dx` missing, network error, non-zero exit) it
    /// falls back to surfacing the install command on `next_steps`. Dry-run
    /// emits `would run …` previews instead of installing. Either way the
    /// first-time `mod components;` + theme stylesheet reminders are appended
    /// to `next_steps`.
    #[serde(default)]
    pub dx_components: Vec<String>,
}

/// Top-level remove kinds. Each entry idempotently deletes the named on-disk
/// item. Targets that don't exist are silently skipped (no `if_missing` toggle
/// — removal of an absent thing is a no-op by definition).
//
// The `Remove*` prefix is intentional: variant names match the on-wire
// `kind: remove_*` discriminator (one-to-one via `rename_all = "snake_case"`)
// so a user reading the YAML sees the same shape clippy wants us to elide.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DslRemove {
    /// Remove a `#[derive(Routable)]` enum variant (and its `#[route(...)]`
    /// attribute). The Routable file is located via the same heuristics
    /// `create_route` uses.
    RemoveRoute {
        /// Variant name (any case — normalized to PascalCase).
        variant: String,
    },
    /// Delete `src/components/{snake}.rs` and remove the matching `pub mod` /
    /// `pub use` lines from `src/components/mod.rs`. Does NOT touch any
    /// Routable enum — pair with `remove_route` if a screen variant is left
    /// dangling.
    RemoveComponent {
        /// Component name (any case).
        component: String,
    },
    /// Delete `src/model/{snake}.rs` and its `mod.rs` entry.
    RemoveModel {
        /// Model name (any case).
        model: String,
    },
    /// Delete `src/server/{snake}.rs` and its `mod.rs` entry.
    RemoveServerFn {
        /// Server-fn name (any case).
        server_fn: String,
    },
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslModel {
    pub name: String,
    pub fields: Vec<DslModelField>,
    /// Extra derives beyond the defaults (Debug, Clone, PartialEq, Serialize,
    /// Deserialize). Duplicates with defaults are de-duplicated.
    #[serde(default)]
    pub derives: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslModelField {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslStore {
    pub name: String,
    /// Model name (PascalCase or snake_case) declared in the same doc — or
    /// synthesized by a `resources:` entry. The store provides typed CRUD over
    /// this type.
    pub resource: String,
    /// Backend. Currently only "in_memory" is implemented. Default: "in_memory".
    #[serde(default)]
    pub kind: Option<String>,
    /// Name of the integer id field on the model. Default: "id".
    #[serde(default)]
    pub id_field: Option<String>,
    /// Rust type of the id field. Default: "i64".
    #[serde(default)]
    pub id_type: Option<String>,
    /// Append a `#[cfg(test)] mod tests` block exercising the CRUD methods to
    /// the generated store file. The model must derive `Default`. Default:
    /// false. Set automatically by the `resources:` expansion (which forces
    /// `Default` on the synthesized model). Plain `stores:` users opt in by
    /// setting this to true and ensuring the referenced model has `Default`.
    #[serde(default)]
    pub emit_tests: Option<bool>,
}

/// Client-side reactive list. Generates `src/state/{snake}.rs` (no
/// `#[cfg(feature = "server")]` gate) exposing a `Signal<Vec<T>>`-backed
/// store as context. The companion `client_crud` Screen template wires
/// add/toggle/delete handlers against it.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslClientStore {
    pub name: String,
    /// Rust item type for the list (e.g. `Todo`). When the name matches a
    /// Model declared in the same doc the generated file emits a
    /// `use crate::model::{ItemType};`. Built-in types (`String`, integers)
    /// pass through unchanged.
    pub item_type: String,
    /// Rust expression for the initial Vec value. Defaults to `Vec::new()`.
    #[serde(default)]
    pub initial: Option<String>,
    /// Field name on the item type to use for `remove_by_id` / `update_by_id`.
    /// When unset those helpers are omitted.
    #[serde(default)]
    pub id_field: Option<String>,
    /// Rust type of the id field. Default: `i64`.
    #[serde(default)]
    pub id_type: Option<String>,
    /// When true, the store owns its own monotonic id allocator and exposes a
    /// `push_new(item)` helper that assigns the next id to `item.{id_field}`
    /// before pushing. Callers can omit the id field in the struct literal
    /// (the helper sets it). Requires `id_field` to be set and the id type
    /// to be a primitive integer (i8..i128/u8..u128/isize/usize). Default:
    /// false.
    #[serde(default)]
    pub auto_id: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslResource {
    pub name: String,
    pub fields: Vec<DslModelField>,
    /// Name of the integer id field on the model. Must exist in `fields`.
    /// Default: "id".
    #[serde(default)]
    pub id_field: Option<String>,
    /// URL base for the generated list/new screens. Default: "/{plural-snake}".
    /// `/products` and `/products/new` are appended automatically.
    #[serde(default)]
    pub route_base: Option<String>,
    /// Override the auto-pluralized form of the resource name (used to build
    /// the default `route_base` and the `list_{plural}` server-fn name).
    /// Provide the snake_case plural — e.g. `plural: people` for `Person`,
    /// `plural: mice` for `Mouse`. When unset, the built-in pluralizer is used
    /// (handles regular `+s`, `+es` for s/sh/ch/x/z endings, and `y → ies`
    /// after a consonant).
    #[serde(default)]
    pub plural: Option<String>,
    /// Extra derives forwarded to the synthesized Model.
    #[serde(default)]
    pub derives: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslScreenTemplate {
    /// One of: empty (default), resource_list, resource_form.
    pub kind: String,
    /// Server-fn name (snake_case) the screen calls. Required for resource_list
    /// and resource_form.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Type returned by the endpoint (resource_list) or accepted by it
    /// (resource_form).
    #[serde(default)]
    pub item_type: Option<String>,
    /// For resource_form: server-fn (snake_case) called on submit. When unset,
    /// `endpoint` is used.
    #[serde(default)]
    pub on_submit: Option<String>,
    /// For resource_form: route to navigate to on success.
    #[serde(default)]
    pub redirect_to: Option<String>,
    /// For resource_form: input fields { name, type } where type is one of
    /// text, email, password, number, checkbox, textarea.
    #[serde(default)]
    pub fields: Vec<DslFieldDef>,
    /// For client_crud: name of a `client_stores:` entry in this doc (any case)
    /// the screen reads/writes. Required for client_crud.
    #[serde(default)]
    pub store: Option<String>,
    /// For client_crud: field on the item type that the "add" input writes
    /// into, and that the rendered list item displays. Required.
    #[serde(default)]
    pub label_field: Option<String>,
    /// For client_crud: optional bool field on the item type rendered as a
    /// checkbox; toggling it flips the field via `update_by_id`. Omit for
    /// non-toggleable items.
    #[serde(default)]
    pub checkbox_field: Option<String>,
    /// Optional class name applied to the screen's root `div`, overriding the
    /// default `"screen {{ snake }}"` pair. Useful when the host project uses
    /// a design system / utility framework (e.g. Tailwind) and doesn't want
    /// the generated screens to leak `screen` / `{name}` classes. Applies to
    /// every screen template kind.
    #[serde(default)]
    pub class: Option<String>,
    /// Body shape switch. Currently only `kind: empty` honors this:
    ///   - unset (default): emits the historical placeholder `div { h1 { ... } }`
    ///   - `"empty"` / `"stub"`: emits a bare `rsx! {}` with the imports and
    ///     `use_<store>()` wiring intact, dropping the throwaway demo markup
    ///
    /// Use when you're about to rewrite the screen body anyway and don't want
    /// the placeholder to flash before your edit lands.
    #[serde(default)]
    pub body: Option<String>,
    /// Optional design-system preset that overrides the default unstyled
    /// markup with a sensible utility-class skeleton. Currently supported:
    ///   - `"tailwind"`: emits Tailwind-classed defaults on `client_crud`
    ///     screens (form, list, items, buttons, checkbox). Other template
    ///     kinds ignore this field.
    ///
    /// The presets are deliberately conservative — they pick a single
    /// reasonable layout (max-w container, spacing, neutral colors) so the
    /// generated screen looks intentional in a Tailwind project without
    /// committing to a specific theme.
    #[serde(default)]
    pub styled: Option<String>,
    /// Internal: rich-CRUD context populated by `expand_resources` so the list
    /// and form templates can emit a real table (with edit/delete actions) or
    /// an edit-by-id form. Not part of the user-facing spec.
    #[serde(skip)]
    pub crud: Option<CrudCtx>,
}

/// Internal context for resource-synthesized CRUD screens. Carries everything
/// needed for the rich list table and the edit-by-id form.
#[derive(Debug, Clone)]
pub struct CrudCtx {
    /// PascalCase model name (e.g. "Product").
    pub model_pascal: String,
    /// All model fields, with their original Rust types and optionality.
    pub model_fields: Vec<DslModelField>,
    /// snake_case name of the id field.
    pub id_field: String,
    /// Rust type of the id field (e.g. "i64").
    pub id_type: String,
    /// snake_case server fn that returns `Vec<Model>` for the list.
    pub list_endpoint: String,
    /// snake_case server fn that returns `Option<Model>` by id.
    pub get_endpoint: String,
    /// snake_case server fn that updates an existing item.
    pub update_endpoint: String,
    /// snake_case server fn that deletes by id, returning `bool`.
    pub delete_endpoint: String,
    /// Base route for the resource (e.g. "/products"). Used as the redirect
    /// target on submit and as the prefix when building edit links.
    pub list_route: String,
    /// Full route to the "new" screen (e.g. "/products/new").
    pub new_route: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslPropDef {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslServerFn {
    pub name: String,
    #[serde(default)]
    pub args: Vec<DslArgDef>,
    #[serde(default)]
    pub return_type: Option<String>,
    /// HTTP method: "get" or "post". Defaults to "post" when args is non-empty,
    /// "get" otherwise.
    #[serde(default)]
    pub method: Option<String>,
    /// Route path under which the server fn is exposed. Defaults to
    /// "/api/{snake_name}".
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslArgDef {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslComponent {
    pub name: String,
    #[serde(default)]
    pub props: Vec<DslPropDef>,
    /// Stub-body skeleton. One of: `empty` (default), `form`, `list`,
    /// `crud_table`, `resource_view`. Templates are structural starting
    /// points only — they don't wire data; pair with `props:` and edit after
    /// generation. For data-bound screens use `screens:` with a
    /// `template: { kind: resource_list | resource_form }` instead.
    #[serde(default)]
    pub template: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslScreen {
    pub name: String,
    pub route: String,
    /// Optional component name (e.g. a ProtectedRoute guard) that wraps the
    /// screen body. Imported from src/components and rendered around the page.
    #[serde(default)]
    pub wrap_with: Option<String>,
    /// Optional template selector. Without it, the screen renders an empty
    /// placeholder body. `kind: resource_list` emits a use_resource +
    /// loading/error/data match ladder against `endpoint`. `kind: resource_form`
    /// emits a controlled form whose submit handler calls `on_submit` (or
    /// `endpoint`) and navigates to `redirect_to`.
    #[serde(default)]
    pub template: Option<DslScreenTemplate>,
    /// When true, if `route:` is already mapped by a different variant in the
    /// on-disk Routable enum, the existing variant is removed first (as if you
    /// had added a matching `remove: [{kind: remove_route, variant: ...}]`
    /// entry) instead of failing pre-flight with a collision error. Use this
    /// to "take over" a route from a demo screen without a two-step edit.
    #[serde(default)]
    pub replace_route: bool,
    /// Internal: path-param fields for the Routable variant (e.g.
    /// `[("id", "i64")]` for `/items/:id`). Set by `expand_resources` for
    /// edit screens; not part of the user-facing spec.
    #[serde(skip)]
    pub route_params: Vec<(String, String)>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslFieldDef {
    pub name: String,
    /// One of: text, email, password, number, checkbox, textarea.
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub validation: Option<String>,
    /// Internal: original Rust type from the source model. Set by
    /// `expand_resources` so the screen-form template can emit the right
    /// signal-init, oninput, and submit-side parse / Some() wrapping. Not part
    /// of the user-facing spec.
    #[serde(skip)]
    pub rust_type: Option<String>,
    /// Internal: whether the original model field was `optional: true`. Drives
    /// `Some(...)` wrapping at submit time.
    #[serde(skip)]
    pub optional: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslForm {
    pub name: String,
    pub fields: Vec<DslFieldDef>,
    /// Server-fn (snake_case) called inside spawn on submit. When set together
    /// with `feeds_into`, a successful call also resets the form fields and
    /// bumps the target list's version signal.
    #[serde(default)]
    pub on_submit: Option<String>,
    /// Name of a List declared in the same doc that should refresh when this
    /// form succeeds. Wires a per-list version Signal<u32> shared via context.
    #[serde(default)]
    pub feeds_into: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslList {
    pub name: String,
    /// Server-fn (snake_case) that returns the items.
    pub endpoint: String,
    /// Item type rendered by the list (e.g. "User").
    pub item_type: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslColumnDef {
    pub name: String,
    pub label: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslTable {
    pub name: String,
    pub endpoint: String,
    pub item_type: String,
    pub columns: Vec<DslColumnDef>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslSignal {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    /// Rust expression used as the initial value (e.g. `0`, `String::new()`).
    pub initial: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslSocket {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslFeed {
    pub name: String,
    /// Socket name (snake_case) this feed subscribes to.
    pub socket: String,
    /// Item type appended to the feed (e.g. "String").
    pub item_type: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslSessionState {
    pub name: String,
    /// Type stored as the session payload (e.g. "User").
    pub user_type: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslLoginScreen {
    pub name: String,
    pub route: String,
    pub redirect_on_success: String,
    /// When true, behaves like `DslScreen.replace_route`: an on-disk variant
    /// that already maps to `route:` is removed first instead of triggering a
    /// pre-flight collision error.
    #[serde(default)]
    pub replace_route: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslProtectedRoute {
    pub name: String,
    /// Route URL the unauthenticated user is sent to.
    pub redirect_to: String,
    /// Name of a SessionState (snake_case) the guard should read. If omitted,
    /// the generator picks the first session_states entry; if none exist,
    /// emits a TODO-comment fallback.
    #[serde(default)]
    pub requires: Option<String>,
}

/// In-place edits to an existing on-disk item. Idempotent: a field/prop/arg
/// that's already present is skipped; identical re-runs produce no diff.
///
/// Each variant names the on-disk target by user-facing name (any case) and
/// carries the list of items to append or remove. Missing target files /
/// items error unless `if_missing: true` is set on `execute_code`, in which
/// case they are recorded under `collisions` and the run continues. The
/// remove kinds are symmetrically idempotent — a field/prop already absent is
/// silently skipped.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DslModify {
    /// Append fields to `crate::model::{Pascal}`'s struct. Requires the model
    /// to already exist on disk under `src/model/{snake}.rs`.
    AddModelField {
        /// Model name (any case). Resolved to `src/model/{snake}.rs`.
        model: String,
        fields: Vec<DslModelField>,
    },
    /// Append props to `{Pascal}Props` for an existing component under
    /// `src/components/{snake}.rs`. If the component file does not declare a
    /// `*Props` struct yet, the edit errors with a clear message — convert the
    /// component to take props first (e.g. by re-creating it with `props:`).
    AddComponentProp {
        /// Component name (any case). Resolved to `src/components/{snake}.rs`.
        component: String,
        props: Vec<DslPropDef>,
    },
    /// Append arguments to an existing server fn under
    /// `src/server/{snake}.rs`.
    AddServerFnArg {
        /// Server fn name (any case). Resolved to `src/server/{snake}.rs`.
        server_fn: String,
        args: Vec<DslArgDef>,
    },
    /// Delete named fields from `crate::model::{Pascal}`'s struct. Idempotent:
    /// names that aren't present in the struct are silently skipped.
    RemoveModelField {
        /// Model name (any case). Resolved to `src/model/{snake}.rs`.
        model: String,
        /// Field names to remove (snake_case at compare time).
        fields: Vec<String>,
    },
    /// Delete named props from `{Pascal}Props` for a component. Idempotent.
    /// Errors only when the file or the `*Props` struct itself is missing
    /// (handled like the Add* variants).
    RemoveComponentProp {
        /// Component name (any case). Resolved to `src/components/{snake}.rs`.
        component: String,
        /// Prop names to remove (snake_case at compare time).
        props: Vec<String>,
    },
}
