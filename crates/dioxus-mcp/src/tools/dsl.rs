//! Declarative-DSL scaffolding tools.
//!
//! `get_dsl_spec` returns the YAML vocabulary describing every DSL primitive.
//! `execute_code` parses a YAML doc and materializes the corresponding Dioxus
//! 0.7 source files in one shot.
//!
//! Single source of truth: each primitive has a colocated `&'static str` spec
//! block AND a Rust struct used both for serde deserialization and to drive
//! the per-primitive generator. The `spec_examples_round_trip` unit test
//! enforces that every spec example deserializes into its struct.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use heck::{ToPascalCase, ToSnakeCase};
use minijinja::{Environment, context};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::state::State;
use crate::tools::scaffold::{
    self, ArgSpec, CreateRouteParams, CreateServerFnParams, ModUpsert, PropSpec, ScaffoldResult,
    upsert_mod_entry,
};

// ===========================================================================
// DSL data model
// ===========================================================================

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
/// carries the list of items to append. Missing target files / items error
/// unless `if_missing: true` is set on `execute_code`, in which case they are
/// recorded under `collisions` and the run continues.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[allow(clippy::enum_variant_names)] // variants intentionally map to add_* serde tags
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
}

// ===========================================================================
// Per-primitive YAML spec blocks (single source of truth, examples are
// round-trip tested against the structs above).
// ===========================================================================

const SPEC_VERSION: &str = "1";

const CORE_PREAMBLE: &str = r#"# Dioxus-MCP DSL spec
#
# Author a YAML doc using these primitives, then call execute_code with the
# whole doc as a string. The tool parses, pre-flights collisions, and emits
# Dioxus 0.7 source files in one shot.
#
# Top-level shape:
#   version: "1"
#   <primitive_section>: [ ... ]   # see core/extensions below
#
# All field names are case-sensitive. Unknown fields are rejected.
#
# File layout (the blast radius of one execute_code call):
#   src/components/{snake}.rs        Component, Form, List, Table, Feed,
#                                    LoginScreen, ProtectedRoute, Screen
#   src/components/mod.rs            entries added (sorted), file created if missing
#   src/model/{snake}.rs             Model (struct with serde derives)
#   src/model/mod.rs                 entries added (sorted)
#   src/state/{snake}.rs             Store (server-feature gated CRUD helper)
#   src/state/mod.rs                 entries added (sorted)
#   src/server/{snake}.rs            ServerFn (incl. resource-synthesized fns)
#   src/server/mod.rs                entries added (sorted)
#   src/signals/{snake}.rs           Signal
#   src/signals/mod.rs               entries added (sorted)
#   src/sockets/{snake}.rs           Socket
#   src/sockets/mod.rs               entries added (sorted)
#   src/auth/{snake}.rs              SessionState
#   src/auth/mod.rs                  entries added (sorted)
#   the project's #[derive(Routable)] enum file
#                                    new variants inserted for Screen / LoginScreen
#                                    (deduplicated by variant name; resources
#                                    emit a list + new screen per entry)
#
# Re-run semantics:
#   - By default execute_code REFUSES to overwrite an existing leaf file —
#     models, components, server fns, signals, sockets, stores, and session
#     states all hard-error with the conflicting path when their target
#     already exists. Pass `if_missing: true` to silently skip every
#     already-present primitive instead; the response lists each skipped
#     path under `collisions`. Re-runs adding one new field to a model
#     while leaving every other primitive in place are safe with
#     `if_missing: true`.
#   - Route inserts are name-keyed and idempotent: a Screen / LoginScreen whose
#     variant name already exists is skipped.
#   - mod.rs entries are inserted alphabetically; re-runs produce stable diffs.
#   - `modify:` entries are idempotent: a field/prop/arg already present in the
#     target item is skipped, and a re-run with no new additions writes nothing.
#     Missing target files error unless `if_missing: true` (then recorded under
#     `collisions`). Modify runs *after* all create steps in the same call.
#   - Pass `dry_run: true` to compute `would_create` + `would_modify` (plus any
#     `collisions`) without touching disk.
"#;

const CORE_MODEL: &str = r#"  Model:
    description: A shared data type with serde derives. Generates src/model/{snake}.rs and exposes the struct as crate::model::{Pascal}. Server fns can name it in their return_type (e.g. `Vec<Product>`); forthcoming `store:` and `resource:` primitives will consume it directly. Project must depend on `serde = { version = "1", features = ["derive"] }`.
    fields:
      - {name: name, type: string, required: true}
      - {name: fields, type: "ModelField[] — each {name, type, optional?}", required: true}
      - {name: derives, type: "string[] — extra derives appended after Debug, Clone, PartialEq, Serialize, Deserialize", required: false}
    example:
      models:
        - name: Product
          fields:
            - {name: id, type: i64}
            - {name: name, type: String}
            - {name: description, type: String, optional: true}
          derives: [Eq, Hash]
"#;

const CORE_COMPONENT: &str = r#"  Component:
    description: "A reusable UI element. Generates src/components/{snake}.rs. The `template` field picks a stub-body skeleton — omit (or `empty`) for the historical placeholder div. Other kinds: `form` (form + submit handler), `list` (ul with empty-state), `crud_table` (table + toolbar), `resource_view` (article with field list + edit/delete actions). Templates are structural only — they don't bind to any data; pair with `props:` and edit afterwards. For data-bound screens use `screens:` with a Screen template instead."
    fields:
      - {name: name, type: string, required: true}
      - {name: props, type: "PropDef[]", required: false}
      - {name: template, type: "empty|form|list|crud_table|resource_view (default: empty)", required: false}
    template_kinds: [empty, form, list, crud_table, resource_view]
    example:
      components:
        - name: UserCard
          props:
            - {name: id, type: i32}
            - {name: label, type: String, optional: true}
        - name: ProductTable
          template: crud_table
"#;

const CORE_SCREEN: &str = r#"  Screen:
    description: "A top-level routed view. Generates a component file and inserts a route variant in src/router.rs. The `wrap_with` field lets a guard like ProtectedRoute sit at the route layer. The `template` field selects the emitted body — omit it for an empty placeholder; kind=resource_list / kind=resource_form bind to server fns (use these for backend-backed CRUD); kind=client_crud binds to a `client_stores:` entry and emits add/toggle/delete handlers entirely client-side (no server fn needed — ideal for in-memory apps like todo lists)."
    fields:
      - {name: name, type: string, required: true}
      - {name: route, type: string, required: true}
      - {name: wrap_with, type: "ComponentName (e.g. a ProtectedRoute guard)", required: false}
      - {name: template, type: "ScreenTemplate {kind, endpoint?, item_type?, on_submit?, redirect_to?, fields?, store?, label_field?, checkbox_field?}", required: false}
    template_kinds: [empty, resource_list, resource_form, client_crud]
    client_crud_fields:
      - {name: store, type: "ClientStore name in this doc", required: true}
      - {name: item_type, type: "Rust item type (Model in this doc or a built-in like String)", required: true}
      - {name: label_field, type: "Field on the item the add input writes / the row label reads", required: true}
      - {name: checkbox_field, type: "Optional bool field rendered as a per-row checkbox", required: false}
    example:
      screens:
        - name: HomeScreen
          route: /
          wrap_with: Dashboard
        - name: ProductListScreen
          route: /products
          template:
            kind: resource_list
            endpoint: list_products
            item_type: Product
        - name: TodoScreen
          route: /
          template:
            kind: client_crud
            store: TodoStore
            item_type: Todo
            label_field: title
            checkbox_field: done
"#;

const CORE_SERVER_FN: &str = r#"  ServerFn:
    description: An Axum-backed server fn using Dioxus 0.7's #[get/post("/path")] attribute. Requires fullstack feature on the dioxus dep.
    fields:
      - {name: name, type: string, required: true}
      - {name: args, type: "ArgDef[]", required: false}
      - {name: return_type, type: "string — the INNER type only; do NOT wrap in Result<_, ServerFnError> or ServerFnResult<_>, the template adds that wrapper for you. Wrapping is rejected with a clear error.", required: false}
      - {name: method, type: "get|post (defaults: post if args else get)", required: false}
      - {name: path, type: "string (default: /api/{snake_name})", required: false}
    example:
      server_fns:
        - name: fetch_users
          args:
            - {name: limit, type: u32}
          # Pass the inner type, not Result<Vec<String>, ServerFnError>.
          return_type: "Vec<String>"
          method: post
          path: /api/users
"#;

const CORE_STORE: &str = r#"  Store:
    description: "A typed in-memory CRUD helper over a Model. Generates src/state/{snake}.rs as a server-only file (gated with `#![cfg(feature = \"server\")]`) exposing `{Pascal}Store::global()` with list/get/create/update/delete methods. The model must have an integer id field (default name `id`, default type `i64`). Pair with server fns that call into `{Pascal}Store::global()` to expose the store over the wire. The top-level `resources` primitive emits a model+store+server-fn slice in one entry — and forces `emit_tests: true` for the synthesized store."
    fields:
      - {name: name, type: string, required: true}
      - {name: resource, type: "Model name in this doc (or synthesized by resources:)", required: true}
      - {name: kind, type: "in_memory (default). sqlite is reserved.", required: false}
      - {name: id_field, type: "string (default \"id\")", required: false}
      - {name: id_type, type: "string (default \"i64\")", required: false}
      - {name: emit_tests, type: "bool (default false) — appends a `#[cfg(test)] mod tests` block exercising the CRUD methods. Requires the referenced model to derive Default. Set automatically by `resources:`.", required: false}
    example:
      models:
        - name: Product
          fields:
            - {name: id, type: i64}
            - {name: name, type: String}
          derives: [Default]
      stores:
        - name: ProductStore
          resource: Product
          emit_tests: true
"#;

const CORE_CLIENT_STORE: &str = r#"  ClientStore:
    description: "A typed client-side reactive list. Generates `src/state/{snake}.rs` (no server feature gate) exposing a `Signal<Vec<T>>`-backed store via context — call `provide_{snake}()` once in your root component and `use_{snake}()` from any descendant. Emits `push`, `clear`, and (when `id_field` is set) `remove_by_id` and `update_by_id` helpers. Pair with a Screen template `kind: client_crud` for one-call todo-style apps. NO server fn round-trip — ideal for in-memory state, todo lists, drafts, ephemeral UI selections."
    fields:
      - {name: name, type: string, required: true}
      - {name: item_type, type: "Rust type (Model in this doc OR a built-in like String / i32). When it matches a Model name, the file emits `use crate::model::{ItemType};`.", required: true}
      - {name: initial, type: "Rust expression for the initial Vec value (default `Vec::new()`)", required: false}
      - {name: id_field, type: "Field name to use for remove_by_id / update_by_id helpers (e.g. `id`). Omit for primitive item types.", required: false}
      - {name: id_type, type: "Rust type of the id field (default `i64`)", required: false}
    example:
      models:
        - name: Todo
          fields:
            - {name: id, type: i64}
            - {name: title, type: String}
            - {name: done, type: bool}
      client_stores:
        - name: TodoStore
          item_type: Todo
          id_field: id
          id_type: i64
"#;

const CORE_RESOURCE: &str = r#"  Resource:
    description: "A meta-primitive that fans out into a Model + Store + 5 server fns (list/get/create/update/delete) + 3 screens (list at `{route_base}`, new at `{route_base}/new`, edit at `{route_base}/:id/edit`). One entry yields a full CRUD slice. The list screen renders a rich table with edit/delete actions; the new screen submits via create_{snake} and redirects to the list; the edit screen takes an `id` URL param, fetches via get_{snake}, and submits via update_{snake}. The model must declare an integer id field (defaults to `id: i64`)."
    fields:
      - {name: name, type: "PascalCase resource name (Product, Order, …)", required: true}
      - {name: fields, type: "ModelField[] — must contain the id field", required: true}
      - {name: id_field, type: "string (default \"id\")", required: false}
      - {name: route_base, type: "string (default \"/{plural-snake}\"); plural follows `plural` if set, else the built-in algorithm (regular `+s`; `+es` for s/sh/ch/x/z endings; `y → ies` after a consonant)", required: false}
      - {name: plural, type: "string — override the auto-pluralized form for irregular nouns (Person → people, Mouse → mice). Affects the default route_base and the list_{plural} server-fn name.", required: false}
      - {name: derives, type: "string[] forwarded to the synthesized Model", required: false}
    example:
      resources:
        - name: Product
          fields:
            - {name: id, type: i64}
            - {name: name, type: String}
            - {name: description, type: String, optional: true}
        - name: Person
          plural: people
          fields:
            - {name: id, type: i64}
            - {name: name, type: String}
"#;

const CORE_MODIFY: &str = r#"  Modify:
    description: "In-place edits to items that already exist on disk. Each entry is idempotent — fields/props/args already present are skipped and identical re-runs produce no diff. Targets must exist on disk; pass `if_missing: true` on execute_code to skip missing targets (they are recorded under `collisions`) instead of erroring. Currently three edit kinds are supported."
    kinds:
      add_model_field:
        description: "Append fields to `crate::model::{Pascal}`'s struct (src/model/{snake}.rs)."
        fields:
          - {name: kind, type: "literal `add_model_field`", required: true}
          - {name: model, type: "Model name (any case)", required: true}
          - {name: fields, type: "ModelField[] — each {name, type, optional?}", required: true}
      add_component_prop:
        description: "Append props to `{Pascal}Props` for a component (src/components/{snake}.rs). Errors if the component doesn't already declare a Props struct — recreate it with `props:` first."
        fields:
          - {name: kind, type: "literal `add_component_prop`", required: true}
          - {name: component, type: "Component name (any case)", required: true}
          - {name: props, type: "PropDef[] — each {name, type, optional?}", required: true}
      add_server_fn_arg:
        description: "Append arguments to a server fn's parameter list (src/server/{snake}.rs)."
        fields:
          - {name: kind, type: "literal `add_server_fn_arg`", required: true}
          - {name: server_fn, type: "Server-fn name (any case)", required: true}
          - {name: args, type: "ArgDef[] — each {name, type}", required: true}
    example:
      modify:
        - kind: add_model_field
          model: Product
          fields:
            - {name: sku, type: String}
        - kind: add_component_prop
          component: UserCard
          props:
            - {name: avatar_url, type: String, optional: true}
        - kind: add_server_fn_arg
          server_fn: fetch_users
          args:
            - {name: page, type: u32}
"#;

const CRUD_FORM: &str = r#"  Form:
    description: A controlled form component. One use_signal per field, oninput wires to the signal. When on_submit names a server_fn, the form spawns it with the field values; when feeds_into names a List in the same doc, success also resets the form and bumps that list's version signal so it refetches.
    fields:
      - {name: name, type: string, required: true}
      - {name: fields, type: "FieldDef[]", required: true}
      - {name: on_submit, type: "server_fn name (snake_case)", required: false}
      - {name: feeds_into, type: "List name in this doc", required: false}
    field_types: [text, email, password, number, checkbox, textarea]
    example:
      forms:
        - name: SignupForm
          fields:
            - {name: email, type: email, validation: required}
            - {name: password, type: password, validation: required}
          on_submit: handle_signup
          feeds_into: UserList
"#;

const CRUD_LIST: &str = r#"  List:
    description: A list backed by a server fn. Uses use_resource + `match items()` and renders loading/error/empty states. If any Form in the same doc has feeds_into pointing at this list, the generator also emits provide_{snake}_version()/use_{snake}_version() helpers and re-runs the resource when the version signal bumps.
    fields:
      - {name: name, type: string, required: true}
      - {name: endpoint, type: string, required: true}
      - {name: item_type, type: string, required: true}
    example:
      lists:
        - name: UserList
          endpoint: fetch_users
          item_type: String
"#;

const CRUD_TABLE: &str = r#"  Table:
    description: A tabular display backed by a server fn with sortable columns (sort signal scaffolded).
    fields:
      - {name: name, type: string, required: true}
      - {name: endpoint, type: string, required: true}
      - {name: item_type, type: string, required: true}
      - {name: columns, type: "ColumnDef[]", required: true}
    example:
      tables:
        - name: UserTable
          endpoint: fetch_users
          item_type: String
          columns:
            - {name: id, label: ID}
            - {name: name, label: Name}
"#;

const REALTIME_SIGNAL: &str = r#"  Signal:
    description: A global Signal<T> exposed via context. Generates src/signals/{snake}.rs with provider + accessor.
    fields:
      - {name: name, type: string, required: true}
      - {name: type, type: string, required: true}
      - {name: initial, type: "rust expr", required: true}
    example:
      signals:
        - name: counter
          type: i32
          initial: "0"
"#;

const REALTIME_SOCKET: &str = r#"  Socket:
    description: A WebSocket binding (web-sys based). Generates src/sockets/{snake}.rs. Add `web-sys = { version = "0.3", features = ["WebSocket", "MessageEvent", "BinaryType", "ErrorEvent"] }` to your Cargo.toml.
    fields:
      - {name: name, type: string, required: true}
      - {name: url, type: string, required: true}
    example:
      sockets:
        - name: chat
          url: wss://example.test/chat
"#;

const REALTIME_FEED: &str = r#"  Feed:
    description: A live-updating list component subscribed to a Socket. Generates src/components/{snake}.rs with a Vec<T> signal and onmessage append.
    fields:
      - {name: name, type: string, required: true}
      - {name: socket, type: string, required: true}
      - {name: item_type, type: string, required: true}
    example:
      feeds:
        - name: ChatFeed
          socket: chat
          item_type: String
"#;

const AUTH_SESSION: &str = r#"  SessionState:
    description: Global Signal<Option<UserType>> exposed via context for current session. Generates src/auth/{snake}.rs.
    fields:
      - {name: name, type: string, required: true}
      - {name: user_type, type: string, required: true}
    example:
      session_states:
        - name: session
          user_type: String
"#;

const AUTH_LOGIN: &str = r#"  LoginScreen:
    description: A login form component plus a route variant. Submitting redirects to redirect_on_success.
    fields:
      - {name: name, type: string, required: true}
      - {name: route, type: string, required: true}
      - {name: redirect_on_success, type: string, required: true}
    example:
      login_screens:
        - name: Login
          route: /login
          redirect_on_success: /
"#;

const AUTH_PROTECTED: &str = r#"  ProtectedRoute:
    description: A guard component that calls navigator()+use_effect to redirect to redirect_to when the session is None, otherwise renders children. With `requires` set (or any SessionState present in the doc) the guard imports use_{session}() automatically; otherwise it emits a TODO-comment fallback against a placeholder Signal<bool> context.
    fields:
      - {name: name, type: string, required: true}
      - {name: redirect_to, type: string, required: true}
      - {name: requires, type: "SessionState name in this doc", required: false}
    example:
      protected_routes:
        - name: Dashboard
          redirect_to: /login
          requires: session
"#;

// ===========================================================================
// Code-generation templates
// ===========================================================================

const SCREEN_TPL: &str = r#"use dioxus::prelude::*;
{%- if wrap_pascal %}
use crate::components::{{ wrap_pascal }};
{%- endif %}

#[component]
pub fn {{ pascal }}() -> Element {
    rsx! {
{%- if wrap_pascal %}
        {{ wrap_pascal }} {
            div { class: "screen {{ snake }}",
                h1 { "{{ pascal }}" }
            }
        }
{%- else %}
        div { class: "screen {{ snake }}",
            h1 { "{{ pascal }}" }
        }
{%- endif %}
    }
}
"#;

const FORM_TPL: &str = r#"use dioxus::prelude::*;
{%- if needs_handler_import %}
use crate::server::{{ handler }};
{%- endif %}
{%- if feeds_into_snake %}
use crate::components::{{ feeds_into_snake }}::use_{{ feeds_into_snake }}_version;
{%- endif %}

#[component]
pub fn {{ pascal }}() -> Element {
{%- for f in fields %}
    let mut {{ f.name }} = use_signal(|| {{ f.initial }});
{%- endfor %}
{%- if feeds_into_snake %}
    let mut version = use_{{ feeds_into_snake }}_version();
{%- endif %}

    rsx! {
        form {
            onsubmit: move |evt: FormEvent| {
                evt.prevent_default();
{{ on_submit_body }}
            },
{%- for f in fields %}
            label { "{{ f.label }}" }{% if f.validation %} // validation: {{ f.validation }}{% endif %}
            {{ f.tag }} {
{%- if f.tag == "input" %}
                r#type: "{{ f.input_type }}",
{%- endif %}
                value: "{{ '{' }}{{ f.name }}(){{ '}' }}",
                oninput: move |e| {{ f.name }}.set(e.value()),
            }
{%- endfor %}
            button { r#type: "submit", "Submit" }
        }
    }
}
"#;

const LIST_TPL: &str = r#"use dioxus::prelude::*;
use crate::server::{{ endpoint }};
{%- if versioned %}

#[derive(Copy, Clone)]
pub struct {{ pascal }}Version(pub Signal<u32>);

pub fn provide_{{ snake }}_version() -> {{ pascal }}Version {
    use_context_provider(|| {{ pascal }}Version(Signal::new(0u32)))
}

pub fn use_{{ snake }}_version() -> Signal<u32> {
    use_context::<{{ pascal }}Version>().0
}
{%- endif %}

#[component]
pub fn {{ pascal }}() -> Element {
{%- if versioned %}
    let version = use_{{ snake }}_version();
    let items = use_resource(move || async move {
        let _ = version();
        {{ endpoint }}().await
    });
{%- else %}
    let items = use_resource(move || async move { {{ endpoint }}().await });
{%- endif %}

    rsx! {
        match items() {
            None => rsx! { div { "Loading..." } },
            Some(Err(e)) => rsx! { div { class: "error", "Error: {e}" } },
            Some(Ok(rows)) if rows.is_empty() => rsx! { div { "No items." } },
            Some(Ok(rows)) => rsx! {
                ul { class: "{{ snake }}",
                    for item in rows.iter() {
                        li { "{item:?}" }
                    }
                }
            },
        }
    }
}
"#;

const TABLE_TPL: &str = r#"use dioxus::prelude::*;
use crate::server::{{ endpoint }};

#[component]
pub fn {{ pascal }}() -> Element {
    let items = use_resource(move || async move { {{ endpoint }}().await });
    let mut sort_by = use_signal(|| String::new());

    rsx! {
        match items() {
            None => rsx! { div { "Loading..." } },
            Some(Err(e)) => rsx! { div { class: "error", "Error: {e}" } },
            Some(Ok(rows)) => rsx! {
                table { class: "{{ snake }}",
                    thead {
                        tr {
{%- for c in columns %}
                            th {
                                onclick: move |_| sort_by.set("{{ c.name }}".into()),
                                "{{ c.label }}"
                            }
{%- endfor %}
                        }
                    }
                    tbody {
                        for row in rows.iter() {
                            tr {
{%- for c in columns %}
                                td { "{row:?}" }
{%- endfor %}
                            }
                        }
                    }
                }
            },
        }
    }
}
"#;

const SIGNAL_TPL: &str = r#"use dioxus::prelude::*;

pub fn provide_{{ snake }}() -> Signal<{{ ty }}> {
    use_context_provider(|| Signal::new({{ initial }}))
}

pub fn use_{{ snake }}() -> Signal<{{ ty }}> {
    use_context::<Signal<{{ ty }}>>()
}
"#;

const SOCKET_TPL: &str = r#"// Generated WebSocket binding (web-sys).
// Add to your Cargo.toml:
//   web-sys = { version = "0.3", features = ["WebSocket", "MessageEvent", "BinaryType", "ErrorEvent"] }
//   wasm-bindgen = "0.2"
#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{MessageEvent, WebSocket};

pub const {{ upper }}_URL: &str = "{{ url }}";

pub struct {{ pascal }}Socket {
    inner: WebSocket,
    _on_msg: Closure<dyn FnMut(MessageEvent)>,
}

impl {{ pascal }}Socket {
    pub fn connect(mut on_message: impl FnMut(String) + 'static) -> Result<Self, JsValue> {
        let ws = WebSocket::new({{ upper }}_URL)?;
        let cb = Closure::wrap(Box::new(move |evt: MessageEvent| {
            if let Some(text) = evt.data().as_string() {
                on_message(text);
            }
        }) as Box<dyn FnMut(MessageEvent)>);
        ws.set_onmessage(Some(cb.as_ref().unchecked_ref()));
        Ok(Self { inner: ws, _on_msg: cb })
    }

    pub fn send(&self, msg: &str) -> Result<(), JsValue> {
        self.inner.send_with_str(msg)
    }
}
"#;

const FEED_TPL: &str = r#"use dioxus::prelude::*;
#[cfg(target_arch = "wasm32")]
use crate::sockets::{{ socket }}::{{ socket_pascal }}Socket;

#[component]
pub fn {{ pascal }}() -> Element {
    let mut items = use_signal::<Vec<{{ item_type }}>>(Vec::new);

    #[cfg(target_arch = "wasm32")]
    use_effect(move || {
        let _ = {{ socket_pascal }}Socket::connect(move |msg| {
            items.write().push(msg);
        });
    });

    rsx! {
        ul { class: "{{ snake }}",
            for it in items.read().iter() {
                li { "{it:?}" }
            }
        }
    }
}
"#;

const SESSION_TPL: &str = r#"use dioxus::prelude::*;

pub fn provide_{{ snake }}() -> Signal<Option<{{ user_type }}>> {
    use_context_provider(|| Signal::new(None::<{{ user_type }}>))
}

pub fn use_{{ snake }}() -> Signal<Option<{{ user_type }}>> {
    use_context::<Signal<Option<{{ user_type }}>>>()
}
"#;

const LOGIN_TPL: &str = r#"use dioxus::prelude::*;

#[component]
pub fn {{ pascal }}() -> Element {
    let mut email = use_signal(|| String::new());
    let mut password = use_signal(|| String::new());

    rsx! {
        form {
            onsubmit: move |evt: FormEvent| {
                evt.prevent_default();
                // TODO authenticate, then navigate to {{ redirect }}.
            },
            label { "Email" }
            input {
                r#type: "email",
                value: "{email()}",
                oninput: move |e| email.set(e.value()),
            }
            label { "Password" }
            input {
                r#type: "password",
                value: "{password()}",
                oninput: move |e| password.set(e.value()),
            }
            button { r#type: "submit", "Sign in" }
        }
    }
}
"#;

const PROTECTED_TPL: &str = r#"use dioxus::prelude::*;
{%- if session_snake %}
use crate::auth::{{ session_snake }}::use_{{ session_snake }};
{%- endif %}

#[component]
pub fn {{ pascal }}(children: Element) -> Element {
{%- if session_snake %}
    let session = use_{{ session_snake }}();
    let nav = navigator();

    use_effect(move || {
        if session.read().is_none() {
            nav.push("{{ redirect_to }}");
        }
    });

    if session.read().is_some() {
        rsx! { {children} }
    } else {
        rsx! { div { class: "auth-redirect", "Redirecting to {{ redirect_to }}..." } }
    }
{%- else %}
    // TODO replace with your real session accessor; this guard redirects to
    // {{ redirect_to }} when unauthenticated. Add a SessionState to the DSL doc
    // (or call use_context for whatever signal your app uses) to wire this.
    let authenticated = use_context::<Signal<bool>>();
    let nav = navigator();
    use_effect(move || {
        if !*authenticated.read() {
            nav.push("{{ redirect_to }}");
        }
    });
    if *authenticated.read() {
        rsx! { {children} }
    } else {
        rsx! { div { class: "auth-redirect", "Redirecting to {{ redirect_to }}..." } }
    }
{%- endif %}
}
"#;

const MODEL_TPL: &str = r#"use serde::{Deserialize, Serialize};

#[derive({{ derives }})]
pub struct {{ pascal }} {
{%- for f in fields %}
{%- if f.optional %}
    pub {{ f.name }}: Option<{{ f.ty }}>,
{%- else %}
    pub {{ f.name }}: {{ f.ty }},
{%- endif %}
{%- endfor %}
}
"#;

const STORE_TPL: &str = r#"#![cfg(feature = "server")]
//! In-memory CRUD store for {{ res_pascal }}. Tied to the server feature so
//! the wasm bundle does not pull it in.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::model::{{ res_pascal }};

pub struct {{ store_pascal }} {
    items: Mutex<Vec<{{ res_pascal }}>>,
    next_id: AtomicI64,
}

impl {{ store_pascal }} {
    fn new() -> Self {
        Self {
            items: Mutex::new(Vec::new()),
            next_id: AtomicI64::new(1),
        }
    }

    pub fn global() -> &'static {{ store_pascal }} {
        static INSTANCE: OnceLock<{{ store_pascal }}> = OnceLock::new();
        INSTANCE.get_or_init({{ store_pascal }}::new)
    }

    pub fn list(&self) -> Vec<{{ res_pascal }}> {
        self.items.lock().unwrap().clone()
    }

    pub fn get(&self, id: {{ id_type }}) -> Option<{{ res_pascal }}> {
        self.items
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.{{ id_field }} == id)
            .cloned()
    }

    pub fn create(&self, mut item: {{ res_pascal }}) -> {{ res_pascal }} {
        item.{{ id_field }} = self.next_id.fetch_add(1, Ordering::SeqCst) as {{ id_type }};
        self.items.lock().unwrap().push(item.clone());
        item
    }

    pub fn update(&self, item: {{ res_pascal }}) -> Option<{{ res_pascal }}> {
        let mut items = self.items.lock().unwrap();
        if let Some(slot) = items.iter_mut().find(|r| r.{{ id_field }} == item.{{ id_field }}) {
            *slot = item.clone();
            Some(item)
        } else {
            None
        }
    }

    pub fn delete(&self, id: {{ id_type }}) -> bool {
        let mut items = self.items.lock().unwrap();
        let before = items.len();
        items.retain(|r| r.{{ id_field }} != id);
        items.len() < before
    }
}
{%- if emit_tests %}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each test gets its own store so they don't share state through
    /// `global()`'s `OnceLock`.
    fn fresh() -> {{ store_pascal }} {
        {{ store_pascal }}::new()
    }

    #[test]
    fn create_assigns_id_and_appends_to_list() {
        let s = fresh();
        let item = s.create({{ res_pascal }}::default());
        assert_eq!(item.{{ id_field }}, 1);
        assert_eq!(s.list().len(), 1);

        let next = s.create({{ res_pascal }}::default());
        assert_eq!(next.{{ id_field }}, 2);
        assert_eq!(s.list().len(), 2);
    }

    #[test]
    fn get_returns_item_when_id_matches_otherwise_none() {
        let s = fresh();
        let created = s.create({{ res_pascal }}::default());
        let fetched = s.get(created.{{ id_field }}).expect("just-created item");
        assert_eq!(fetched.{{ id_field }}, created.{{ id_field }});
        assert!(s.get(created.{{ id_field }} + 999).is_none());
    }

    #[test]
    fn update_replaces_when_id_matches_returns_none_when_not_found() {
        let s = fresh();
        let created = s.create({{ res_pascal }}::default());
        assert!(s.update(created.clone()).is_some());
        assert_eq!(s.list().len(), 1);

        let mut ghost = {{ res_pascal }}::default();
        ghost.{{ id_field }} = created.{{ id_field }} + 999;
        assert!(s.update(ghost).is_none());
    }

    #[test]
    fn delete_removes_matching_item_and_is_idempotent() {
        let s = fresh();
        let created = s.create({{ res_pascal }}::default());
        assert!(s.delete(created.{{ id_field }}));
        assert!(s.list().is_empty());
        // Second delete returns false — nothing to remove.
        assert!(!s.delete(created.{{ id_field }}));
    }

    #[test]
    fn list_returns_a_clone_callers_can_mutate_independently() {
        let s = fresh();
        s.create({{ res_pascal }}::default());
        let mut snap = s.list();
        snap.clear();
        assert_eq!(s.list().len(), 1, "store should be unaffected by snapshot mutation");
    }
}
{%- endif %}
"#;

const SCREEN_RESOURCE_LIST_TPL: &str = r#"use dioxus::prelude::*;
{%- if wrap_pascal %}
use crate::components::{{ wrap_pascal }};
{%- endif %}
use crate::server::{{ endpoint }};

#[component]
pub fn {{ pascal }}() -> Element {
    let items = use_resource(move || async move { {{ endpoint }}().await });

    rsx! {
{%- if wrap_pascal %}
        {{ wrap_pascal }} {
            div { class: "screen {{ snake }}",
                h1 { "{{ pascal }}" }
                match &*items.read_unchecked() {
                    None => rsx! { div { "Loading..." } },
                    Some(Err(e)) => rsx! { div { class: "error", "Error: {e}" } },
                    Some(Ok(rows)) if rows.is_empty() => rsx! { div { "No items." } },
                    Some(Ok(rows)) => rsx! {
                        ul { class: "{{ snake }}-items",
                            for item in rows.iter() {
                                li { "{item:?}" }
                            }
                        }
                    },
                }
            }
        }
{%- else %}
        div { class: "screen {{ snake }}",
            h1 { "{{ pascal }}" }
            match &*items.read_unchecked() {
                None => rsx! { div { "Loading..." } },
                Some(Err(e)) => rsx! { div { class: "error", "Error: {e}" } },
                Some(Ok(rows)) if rows.is_empty() => rsx! { div { "No items." } },
                Some(Ok(rows)) => rsx! {
                    ul { class: "{{ snake }}-items",
                        for item in rows.iter() {
                            li { "{item:?}" }
                        }
                    }
                },
            }
        }
{%- endif %}
    }
}
"#;

const SCREEN_RESOURCE_FORM_TPL: &str = r#"use dioxus::prelude::*;
{%- if wrap_pascal %}
use crate::components::{{ wrap_pascal }};
{%- endif %}
use crate::server::{{ submit }};
{%- if item_type %}
use crate::model::{{ item_type }};
{%- endif %}

#[component]
pub fn {{ pascal }}() -> Element {
{%- for f in fields %}
    let mut {{ f.name }} = use_signal(|| {{ f.initial }});
{%- endfor %}
{%- if redirect_to %}
    let nav = navigator();
{%- endif %}

    rsx! {
{%- if wrap_pascal %}
        {{ wrap_pascal }} {
            div { class: "screen {{ snake }}",
                form {
                    onsubmit: move |evt: FormEvent| {
                        evt.prevent_default();
{{ submit_body }}
                    },
{%- for f in fields %}
                    label { "{{ f.label }}" }
                    {{ f.tag }} {
{%- if f.tag == "input" %}
                        r#type: "{{ f.input_type }}",
{%- endif %}
{%- if f.is_bool %}
                        checked: "{{ '{' }}{{ f.name }}(){{ '}' }}",
                        oninput: move |e| {{ f.name }}.set(e.value() == "true"),
{%- else %}
                        value: "{{ '{' }}{{ f.name }}(){{ '}' }}",
                        oninput: move |e| {{ f.name }}.set(e.value()),
{%- endif %}
                    }
{%- endfor %}
                    button { r#type: "submit", "Submit" }
                }
            }
        }
{%- else %}
        div { class: "screen {{ snake }}",
            form {
                onsubmit: move |evt: FormEvent| {
                    evt.prevent_default();
{{ submit_body }}
                },
{%- for f in fields %}
                label { "{{ f.label }}" }
                {{ f.tag }} {
{%- if f.tag == "input" %}
                    r#type: "{{ f.input_type }}",
{%- endif %}
{%- if f.is_bool %}
                    checked: "{{ '{' }}{{ f.name }}(){{ '}' }}",
                    oninput: move |e| {{ f.name }}.set(e.value() == "true"),
{%- else %}
                    value: "{{ '{' }}{{ f.name }}(){{ '}' }}",
                    oninput: move |e| {{ f.name }}.set(e.value()),
{%- endif %}
                }
{%- endfor %}
                button { r#type: "submit", "Submit" }
            }
        }
{%- endif %}
    }
}
"#;

/// Client-side reactive list, exposed via context. NOT gated on the server
/// feature — runs anywhere Dioxus runs. Helpers mirror the spec: `push`,
/// `clear`, and (when `id_field` is set) `remove_by_id` + `update_by_id`.
const CLIENT_STORE_TPL: &str = r#"use dioxus::prelude::*;
{%- if needs_model_import %}
use crate::model::{{ item_type }};
{%- endif %}

#[derive(Copy, Clone)]
pub struct {{ pascal }} {
    pub items: Signal<Vec<{{ item_type }}>>,
}

impl {{ pascal }} {
    pub fn push(self, item: {{ item_type }}) {
        let mut items = self.items;
        items.write().push(item);
    }

    pub fn clear(self) {
        let mut items = self.items;
        items.write().clear();
    }
{%- if id_field %}

    /// Returns true if an item was removed.
    pub fn remove_by_id(self, id: {{ id_type }}) -> bool {
        let mut items = self.items;
        let before = items.read().len();
        items.write().retain(|x| x.{{ id_field }} != id);
        let after = items.read().len();
        after < before
    }

    pub fn update_by_id(self, id: {{ id_type }}, f: impl FnOnce(&mut {{ item_type }})) {
        let mut items = self.items;
        let mut guard = items.write();
        if let Some(item) = guard.iter_mut().find(|x| x.{{ id_field }} == id) {
            f(item);
        }
    }
{%- endif %}
}

pub fn provide_{{ snake }}() -> {{ pascal }} {
    use_context_provider(|| {{ pascal }} {
        items: Signal::new({{ initial }}),
    })
}

pub fn use_{{ snake }}() -> {{ pascal }} {
    use_context::<{{ pascal }}>()
}
"#;

/// Screen template that wires an "add input + list with delete (and optional
/// checkbox)" UI to a ClientStore. No server fn round-trip — all state lives
/// in the Signal-backed context store.
const CLIENT_CRUD_SCREEN_TPL: &str = r#"use dioxus::prelude::*;
{%- if wrap_pascal %}
use crate::components::{{ wrap_pascal }};
{%- endif %}
use crate::state::{{ store_snake }}::use_{{ store_snake }};
{%- if needs_model_import %}
use crate::model::{{ item_type }};
{%- endif %}

#[component]
pub fn {{ pascal }}() -> Element {
    let store = use_{{ store_snake }}();
    let mut draft = use_signal(|| String::new());
{%- if has_id %}
    let mut next_id = use_signal(|| 1{{ id_type_suffix }});
{%- endif %}

    rsx! {
{%- if wrap_pascal %}
        {{ wrap_pascal }} {
            div { class: "screen {{ snake }}",
{{ body }}
            }
        }
{%- else %}
        div { class: "screen {{ snake }}",
{{ body }}
        }
{%- endif %}
    }
}
"#;

/// Resource-synthesized list screen with a real table: column headers from the
/// model fields, keyed rows, per-row Edit link, Delete button (calls the
/// delete server-fn and bumps a local version signal to refetch), and an
/// empty-state CTA. Used when `crud_ctx` is set on a `resource_list` template.
const SCREEN_RESOURCE_CRUD_LIST_TPL: &str = r#"use dioxus::prelude::*;
{%- if wrap_pascal %}
use crate::components::{{ wrap_pascal }};
{%- endif %}
use crate::server::{{ list_endpoint }};
use crate::server::{{ delete_endpoint }};
{%- if route_link %}
use {{ route_link.import_path }};
{%- endif %}

#[component]
pub fn {{ pascal }}() -> Element {
    let mut version = use_signal(|| 0u32);
    let items = use_resource(move || async move {
        let _ = version();
        {{ list_endpoint }}().await
    });

    rsx! {
{%- if wrap_pascal %}
        {{ wrap_pascal }} {
            div { class: "screen {{ snake }}",
                div { class: "toolbar",
{%- if route_link %}
                    Link { to: {{ route_link.enum_name }}::{{ route_link.new_variant }} {}, "New {{ humanized }}" }
{%- else %}
                    a { href: "{{ new_route }}", "New {{ humanized }}" }
{%- endif %}
                }
                match &*items.read_unchecked() {
                    None => rsx! { div { "Loading..." } },
                    Some(Err(e)) => rsx! { div { class: "error", "Error: {e}" } },
                    Some(Ok(rows)) if rows.is_empty() => rsx! {
                        div { class: "empty",
                            p { "No items yet." }
{%- if route_link %}
                            Link { to: {{ route_link.enum_name }}::{{ route_link.new_variant }} {}, "Add your first {{ humanized }}" }
{%- else %}
                            a { href: "{{ new_route }}", "Add your first {{ humanized }}" }
{%- endif %}
                        }
                    },
                    Some(Ok(rows)) => rsx! {
                        table { class: "{{ snake }}-table",
                            thead {
                                tr {
{%- for col in columns %}
                                    th { "{{ col.label }}" }
{%- endfor %}
                                    th { "" }
                                }
                            }
                            tbody {
                                for row in rows.iter() {
                                    tr { key: "{{ '{' }}row.{{ id_field }}{{ '}' }}",
{%- for col in columns %}
                                        td { "{{ col.cell }}" }
{%- endfor %}
                                        td {
{%- if route_link %}
                                            Link { to: {{ route_link.enum_name }}::{{ route_link.edit_variant }} { {{ route_link.id_field }}: row.{{ id_field }}.clone() }, "Edit" }
{%- else %}
                                            a { href: "{{ list_route }}/{{ '{' }}row.{{ id_field }}{{ '}' }}/edit", "Edit" }
{%- endif %}
                                            " "
                                            button {
                                                onclick: {
                                                    let row_id = row.{{ id_field }}.clone();
                                                    move |_| {
                                                        let row_id = row_id.clone();
                                                        spawn(async move {
                                                            if {{ delete_endpoint }}(row_id).await.is_ok() {
                                                                *version.write() += 1;
                                                            }
                                                        });
                                                    }
                                                },
                                                "Delete"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    },
                }
            }
        }
{%- else %}
        div { class: "screen {{ snake }}",
            div { class: "toolbar",
{%- if route_link %}
                Link { to: {{ route_link.enum_name }}::{{ route_link.new_variant }} {}, "New {{ humanized }}" }
{%- else %}
                a { href: "{{ new_route }}", "New {{ humanized }}" }
{%- endif %}
            }
            match &*items.read_unchecked() {
                None => rsx! { div { "Loading..." } },
                Some(Err(e)) => rsx! { div { class: "error", "Error: {e}" } },
                Some(Ok(rows)) if rows.is_empty() => rsx! {
                    div { class: "empty",
                        p { "No items yet." }
{%- if route_link %}
                        Link { to: {{ route_link.enum_name }}::{{ route_link.new_variant }} {}, "Add your first {{ humanized }}" }
{%- else %}
                        a { href: "{{ new_route }}", "Add your first {{ humanized }}" }
{%- endif %}
                    }
                },
                Some(Ok(rows)) => rsx! {
                    table { class: "{{ snake }}-table",
                        thead {
                            tr {
{%- for col in columns %}
                                th { "{{ col.label }}" }
{%- endfor %}
                                th { "" }
                            }
                        }
                        tbody {
                            for row in rows.iter() {
                                tr { key: "{{ '{' }}row.{{ id_field }}{{ '}' }}",
{%- for col in columns %}
                                    td { "{{ col.cell }}" }
{%- endfor %}
                                    td {
{%- if route_link %}
                                        Link { to: {{ route_link.enum_name }}::{{ route_link.edit_variant }} { {{ route_link.id_field }}: row.{{ id_field }}.clone() }, "Edit" }
{%- else %}
                                        a { href: "{{ list_route }}/{{ '{' }}row.{{ id_field }}{{ '}' }}/edit", "Edit" }
{%- endif %}
                                        " "
                                        button {
                                            onclick: {
                                                let row_id = row.{{ id_field }}.clone();
                                                move |_| {
                                                    let row_id = row_id.clone();
                                                    spawn(async move {
                                                        if {{ delete_endpoint }}(row_id).await.is_ok() {
                                                            *version.write() += 1;
                                                        }
                                                    });
                                                }
                                            },
                                            "Delete"
                                        }
                                    }
                                }
                            }
                        }
                    }
                },
            }
        }
{%- endif %}
    }
}
"#;

/// Resource-synthesized edit screen. Outer component takes the id path-param,
/// fetches via the get_* server fn, and renders an inner Form sub-component
/// (defined in the same file) that takes the loaded item as a prop and
/// initializes signals from it. Submit constructs the model with the original
/// id preserved and calls the update_* server fn.
const SCREEN_RESOURCE_EDIT_FORM_TPL: &str = r#"use dioxus::prelude::*;
{%- if wrap_pascal %}
use crate::components::{{ wrap_pascal }};
{%- endif %}
use crate::server::{{ get_endpoint }};
use crate::server::{{ update_endpoint }};
use crate::model::{{ model_pascal }};

#[component]
pub fn {{ pascal }}(id: {{ id_type }}) -> Element {
    let resource = use_resource(move || {
        let id_v = id.clone();
        async move { {{ get_endpoint }}(id_v).await }
    });

    rsx! {
{%- if wrap_pascal %}
        {{ wrap_pascal }} {
            div { class: "screen {{ snake }}",
                match &*resource.read_unchecked() {
                    None => rsx! { div { "Loading..." } },
                    Some(Err(e)) => rsx! { div { class: "error", "Error: {e}" } },
                    Some(Ok(None)) => rsx! { div { "Not found" } },
                    Some(Ok(Some(item))) => rsx! {
                        {{ pascal }}Form { item: item.clone() }
                    },
                }
            }
        }
{%- else %}
        div { class: "screen {{ snake }}",
            match &*resource.read_unchecked() {
                None => rsx! { div { "Loading..." } },
                Some(Err(e)) => rsx! { div { class: "error", "Error: {e}" } },
                Some(Ok(None)) => rsx! { div { "Not found" } },
                Some(Ok(Some(item))) => rsx! {
                    {{ pascal }}Form { item: item.clone() }
                },
            }
        }
{%- endif %}
    }
}

#[component]
fn {{ pascal }}Form(item: {{ model_pascal }}) -> Element {
    let nav = navigator();
    let original_id = item.{{ id_field }}.clone();
{%- for f in fields %}
    let mut {{ f.name }} = use_signal(|| {{ f.signal_init_from_item }});
{%- endfor %}

    rsx! {
        form {
            onsubmit: move |evt: FormEvent| {
                evt.prevent_default();
{{ submit_body }}
            },
{%- for f in fields %}
            label { "{{ f.label }}" }
            {{ f.tag }} {
{%- if f.tag == "input" %}
                r#type: "{{ f.input_type }}",
{%- endif %}
{%- if f.is_bool %}
                checked: "{{ '{' }}{{ f.name }}(){{ '}' }}",
                oninput: move |e| {{ f.name }}.set(e.value() == "true"),
{%- else %}
                value: "{{ '{' }}{{ f.name }}(){{ '}' }}",
                oninput: move |e| {{ f.name }}.set(e.value()),
{%- endif %}
            }
{%- endfor %}
            button { r#type: "submit", "Save" }
        }
    }
}
"#;

const SERVER_FN_WITH_BODY_TPL: &str = r#"use dioxus::prelude::*;
{%- for u in extra_uses %}
{{ u }}
{%- endfor %}

#[{{ method }}("{{ path }}")]
pub async fn {{ snake }}(
{%- for a in args %}
    {{ a.name }}: {{ a.ty }},
{%- endfor %}
) -> Result<{{ ret }}, ServerFnError> {
{{ body }}
}
"#;

// ===========================================================================
// `get_dsl_spec`
// ===========================================================================

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GetDslSpecParams {
    /// Optional list of extension modules to include. One or more of:
    /// "crud", "realtime", "auth". Empty / omitted returns core only.
    #[serde(default)]
    pub extensions: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct GetDslSpecResult {
    pub spec: String,
}

pub async fn get_dsl_spec(
    _state: &Arc<State>,
    p: GetDslSpecParams,
) -> Result<GetDslSpecResult, String> {
    let mut out = String::new();
    out.push_str(CORE_PREAMBLE);
    out.push_str(&format!("\nversion: \"{SPEC_VERSION}\"\n"));
    out.push_str("\ncore:\n");
    out.push_str(CORE_MODEL);
    out.push_str(CORE_STORE);
    out.push_str(CORE_CLIENT_STORE);
    out.push_str(CORE_RESOURCE);
    out.push_str(CORE_COMPONENT);
    out.push_str(CORE_SCREEN);
    out.push_str(CORE_SERVER_FN);
    out.push_str(CORE_MODIFY);

    let want = |k: &str| p.extensions.iter().any(|e| e.eq_ignore_ascii_case(k));
    let any_ext = p.extensions.iter().any(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "crud" | "realtime" | "auth"
        )
    });

    for e in &p.extensions {
        let lc = e.to_ascii_lowercase();
        if !matches!(lc.as_str(), "crud" | "realtime" | "auth") {
            return Err(format!(
                "unknown extension {e:?}; valid: crud, realtime, auth"
            ));
        }
    }

    if any_ext {
        out.push_str("\nextensions:\n");
    }
    if want("crud") {
        out.push_str(" crud:\n");
        out.push_str(&indent(CRUD_FORM, " "));
        out.push_str(&indent(CRUD_LIST, " "));
        out.push_str(&indent(CRUD_TABLE, " "));
    }
    if want("realtime") {
        out.push_str(" realtime:\n");
        out.push_str(&indent(REALTIME_SIGNAL, " "));
        out.push_str(&indent(REALTIME_SOCKET, " "));
        out.push_str(&indent(REALTIME_FEED, " "));
    }
    if want("auth") {
        out.push_str(" auth:\n");
        out.push_str(&indent(AUTH_SESSION, " "));
        out.push_str(&indent(AUTH_LOGIN, " "));
        out.push_str(&indent(AUTH_PROTECTED, " "));
    }

    Ok(GetDslSpecResult { spec: out })
}

fn indent(block: &str, prefix: &str) -> String {
    block
        .lines()
        .map(|l| {
            if l.is_empty() {
                String::from("\n")
            } else {
                format!("{prefix}{l}\n")
            }
        })
        .collect()
}

// ===========================================================================
// `execute_code`
// ===========================================================================

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ExecuteCodeParams {
    /// A YAML doc conforming to the spec returned by get_dsl_spec.
    pub code: String,
    /// Absolute path to the Dioxus project root. Required when the MCP server
    /// was not started in the target project directory.
    pub project_root: Option<String>,
    /// When true, primitives whose leaf file already exists on disk are
    /// silently skipped (and reported in `collisions`) instead of erroring.
    /// Makes re-runs safe during iteration. Default: false (strict).
    #[serde(default)]
    pub if_missing: bool,
    /// When true, no files are written. The response contains `would_create`
    /// and `would_modify` lists describing what *would* happen, plus any
    /// collisions detected on disk. Default: false.
    #[serde(default)]
    pub dry_run: bool,
}

pub async fn execute_code(
    state: &Arc<State>,
    p: ExecuteCodeParams,
) -> Result<ScaffoldResult, String> {
    // Reject multi-document YAML — `serde_yml::from_str` would silently take
    // the first doc only and leave the rest dropped.
    if has_extra_documents(&p.code) {
        return Err(
            "execute_code: input must be a single YAML document; remove `---` separators".into(),
        );
    }
    let mut doc: DslDoc = serde_yml::from_str(&p.code).map_err(|e| format!("YAML parse: {e}"))?;
    if doc.version != SPEC_VERSION {
        return Err(format!(
            "execute_code: version must be {SPEC_VERSION:?}, got {:?}",
            doc.version
        ));
    }

    let synth_server_fns = expand_resources(&mut doc)?;

    let crate_root = scaffold::crate_root(state, p.project_root.as_deref()).await?;

    preflight(&doc, &synth_server_fns, &crate_root, p.if_missing)?;

    if p.dry_run {
        return Ok(plan_dsl(&doc, &synth_server_fns, &crate_root));
    }

    // Global preconditions that the per-primitive emitters used to discover
    // *after* writing files (and that left the project in a half-written state
    // on failure). Run these first so the call is atomic — either everything
    // applies or nothing does.
    let bootstrap = bootstrap_router_if_needed(&doc, &crate_root)?;

    let skip: BTreeSet<std::path::PathBuf> = if p.if_missing {
        skip_set(&doc, &synth_server_fns, &crate_root)
    } else {
        BTreeSet::new()
    };

    let versioned_lists: BTreeSet<String> = doc
        .forms
        .iter()
        .filter_map(|f| f.feeds_into.as_ref().map(|l| l.to_snake_case()))
        .collect();
    let session_names: BTreeSet<String> = doc
        .session_states
        .iter()
        .map(|s| s.name.to_snake_case())
        .collect();

    let mut result = ScaffoldResult::default();
    // Fold in any router-bootstrap output up front so files_created/modified
    // (and the wiring `next_step`) appear in the response even when the rest
    // of the call is a no-op re-run.
    result.files_created.extend(bootstrap.created);
    result.files_modified.extend(bootstrap.modified);
    if let Some(s) = bootstrap.next_step {
        result.next_steps.push(s);
    }

    // Order matters: models first (so server fn return types and stores can
    // resolve them), then server fns (fail-fast on fullstack gating), then
    // leaf primitives, then screens (which call create_route serially).
    for m in &doc.models {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/model", &m.name),
        ) {
            continue;
        }
        let r = generate_model(&crate_root, m)?;
        merge(&mut result, r);
    }

    for sf in &doc.server_fns {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/server", &sf.name),
        ) {
            continue;
        }
        let r = scaffold::create_server_fn(
            state,
            CreateServerFnParams {
                name: sf.name.clone(),
                args: sf
                    .args
                    .iter()
                    .map(|a| ArgSpec {
                        name: a.name.clone(),
                        ty: a.ty.clone(),
                    })
                    .collect(),
                return_type: sf.return_type.clone(),
                method: sf.method.clone(),
                path: sf.path.clone(),
                project_root: p.project_root.clone(),
            },
        )
        .await?;
        merge(&mut result, r);
    }

    for st in &doc.stores {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/state", &st.name),
        ) {
            continue;
        }
        let r = generate_store(&crate_root, st)?;
        merge(&mut result, r);
    }

    let model_names_for_imports: BTreeSet<String> =
        doc.models.iter().map(|m| m.name.to_snake_case()).collect();
    for cs in &doc.client_stores {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/state", &cs.name),
        ) {
            continue;
        }
        let r = generate_client_store(&crate_root, cs, &model_names_for_imports)?;
        merge(&mut result, r);
    }

    for sf in &synth_server_fns {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/server", &sf.name),
        ) {
            continue;
        }
        let r = generate_synth_server_fn(state, &crate_root, sf, p.project_root.as_deref()).await?;
        merge(&mut result, r);
    }

    for sig in &doc.signals {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/signals", &sig.name),
        ) {
            continue;
        }
        let r = generate_signal(&crate_root, sig)?;
        merge(&mut result, r);
    }

    let mut needs_websys = false;
    for s in &doc.sockets {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/sockets", &s.name),
        ) {
            continue;
        }
        let r = generate_socket(&crate_root, s)?;
        merge(&mut result, r);
        needs_websys = true;
    }

    for f in &doc.feeds {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/components", &f.name),
        ) {
            continue;
        }
        let r = generate_feed(&crate_root, f)?;
        merge(&mut result, r);
    }

    for c in &doc.components {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/components", &c.name),
        ) {
            continue;
        }
        let r = scaffold::create_component(
            state,
            scaffold::CreateComponentParams {
                name: c.name.clone(),
                props: c
                    .props
                    .iter()
                    .map(|p| PropSpec {
                        name: p.name.clone(),
                        ty: p.ty.clone(),
                        optional: p.optional,
                    })
                    .collect(),
                path: None,
                template: c.template.clone(),
                project_root: p.project_root.clone(),
            },
        )
        .await?;
        merge(&mut result, r);
    }

    for f in &doc.forms {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/components", &f.name),
        ) {
            continue;
        }
        let r = generate_form(&crate_root, f)?;
        merge(&mut result, r);
    }

    for l in &doc.lists {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/components", &l.name),
        ) {
            continue;
        }
        let v = versioned_lists.contains(&l.name.to_snake_case());
        let r = generate_list(&crate_root, l, v)?;
        merge(&mut result, r);
    }

    for t in &doc.tables {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/components", &t.name),
        ) {
            continue;
        }
        let r = generate_table(&crate_root, t)?;
        merge(&mut result, r);
    }

    for s in &doc.session_states {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/auth", &s.name),
        ) {
            continue;
        }
        let r = generate_session(&crate_root, s)?;
        merge(&mut result, r);
    }

    for ls in &doc.login_screens {
        let leaf = leaf_for(&crate_root, "src/components", &ls.name);
        if skip.contains(&leaf) {
            // Body already on disk; still run the idempotent route insert so
            // a re-run after a partial failure finishes the wiring. Without
            // this, the response on rerun says `next_steps: []` even though
            // the Routable variant was never added.
            result.collisions.push(leaf);
            let route = scaffold::create_route(
                state,
                CreateRouteParams {
                    path: ls.route.clone(),
                    component: ls.name.to_pascal_case(),
                    router_file: None,
                    project_root: p.project_root.clone(),
                    params: Vec::new(),
                },
            )
            .await?;
            merge(&mut result, route);
            continue;
        }
        let r = generate_login_screen(state, &crate_root, ls, p.project_root.as_deref()).await?;
        merge(&mut result, r);
    }

    for pr in &doc.protected_routes {
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/components", &pr.name),
        ) {
            continue;
        }
        let r = generate_protected_route(&crate_root, pr, &session_names)?;
        merge(&mut result, r);
    }

    for sc in &doc.screens {
        let leaf = leaf_for(&crate_root, "src/components", &sc.name);
        if skip.contains(&leaf) {
            // See login_screens loop above: idempotent route insert on skip.
            result.collisions.push(leaf);
            let route = scaffold::create_route(
                state,
                CreateRouteParams {
                    path: sc.route.clone(),
                    component: sc.name.to_pascal_case(),
                    router_file: None,
                    project_root: p.project_root.clone(),
                    params: sc.route_params.clone(),
                },
            )
            .await?;
            merge(&mut result, route);
            continue;
        }
        let r = generate_screen(
            state,
            &crate_root,
            sc,
            &doc.client_stores,
            p.project_root.as_deref(),
        )
        .await?;
        merge(&mut result, r);
    }

    for m in &doc.modify {
        apply_modify(&crate_root, m, p.if_missing, &mut result)?;
    }

    if needs_websys {
        result.next_steps.push(
            "add `web-sys = { version = \"0.3\", features = [\"WebSocket\", \"MessageEvent\", \"BinaryType\", \"ErrorEvent\"] }` and `wasm-bindgen = \"0.2\"` to your Cargo.toml for the generated socket(s)".into(),
        );
    }

    // Auto-declare top-level modules in the crate root (src/main.rs or
    // src/lib.rs) for every subdir we wrote into. Skips quietly if no crate
    // root is found (e.g. workspace-only layout); the generated files will
    // still be on disk and a next_steps hint covers the manual case.
    let touched_top_mods = top_level_modules_touched(&result, &crate_root);
    for module in &touched_top_mods {
        match scaffold::upsert_crate_mod(&crate_root, module) {
            Ok(Some(path)) => result.files_modified.push(path),
            Ok(None) => {}
            Err(e) => {
                result.next_steps.push(format!(
                    "could not auto-declare `pub mod {module};` in crate root: {e} — add it yourself in src/main.rs or src/lib.rs"
                ));
            }
        }
    }
    if scaffold::find_crate_root_file(&crate_root).is_none() && !touched_top_mods.is_empty() {
        let mods = touched_top_mods.join(", ");
        result.next_steps.push(format!(
            "no src/main.rs or src/lib.rs found — declare `pub mod {{{mods}}};` in your crate root manually"
        ));
    }

    // Patch Cargo.toml whenever the doc declares models — not just when a
    // model file was emitted this run. A re-run with `if_missing: true` skips
    // every model write but still needs the serde dep to be in place; without
    // this, a first-call partial failure followed by a successful re-run could
    // leave Cargo.toml unpatched.
    if !doc.models.is_empty() {
        match ensure_serde_in_cargo_toml(&crate_root) {
            Ok(SerdePatch::AlreadyOk) => {}
            Ok(SerdePatch::Patched(path)) => {
                result.files_modified.push(path);
                result
                    .next_steps
                    .push("patched Cargo.toml to add `serde = { version = \"1\", features = [\"derive\"] }` (required by the generated model(s))".into());
            }
            Ok(SerdePatch::PresentWithoutDeriveFeature) => {
                result.next_steps.push(
                    "your Cargo.toml has `serde` but not the `derive` feature — add `features = [\"derive\"]` so the generated model(s) compile".into(),
                );
            }
            Ok(SerdePatch::NoCargoToml) => {
                result.next_steps.push(
                    "no Cargo.toml found at the crate root — ensure `serde = { version = \"1\", features = [\"derive\"] }` is declared somewhere upstream for the generated model(s)".into(),
                );
            }
            Err(e) => {
                result.next_steps.push(format!(
                    "could not auto-patch Cargo.toml for serde: {e} — add `serde = {{ version = \"1\", features = [\"derive\"] }}` manually"
                ));
            }
        }
    }

    dedup_paths(&mut result.files_created);
    dedup_paths(&mut result.files_modified);
    dedup_paths(&mut result.collisions);

    // Surface hand-edit hotspots: for every newly-created file the scaffolder
    // wrote, find `// TODO` markers and add one `next_steps` entry per
    // occurrence, formatted `path:line — message`. Lets the caller jump
    // straight to the body lines that still need a human (TODO4 §4.2).
    append_todo_next_steps(&mut result, &crate_root);

    // High-level outcome so callers don't have to interpret three vector
    // lengths. `no_changes` means everything collided (a totally idempotent
    // re-run); `partial` means at least one primitive was skipped while the
    // rest applied; `applied` is the clean-run case.
    let touched = !result.files_created.is_empty() || !result.files_modified.is_empty();
    let collided = !result.collisions.is_empty();
    result.status = Some(match (touched, collided) {
        (false, true) => "no_changes".into(),
        (true, true) => "partial".into(),
        _ => "applied".into(),
    });

    Ok(result)
}

/// Scan every freshly-created file for `// TODO` markers and surface
/// `path:line — message` entries on `next_steps`. Paths are emitted relative
/// to the crate root so they paste cleanly into editors.
fn append_todo_next_steps(result: &mut ScaffoldResult, crate_root: &Path) {
    let mut hotspots: Vec<String> = Vec::new();
    for path in &result.files_created {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix("// TODO") {
                let message = rest.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
                let rel = path.strip_prefix(crate_root).unwrap_or(path);
                let entry = if message.is_empty() {
                    format!("{}:{} — TODO", rel.display(), i + 1)
                } else {
                    format!("{}:{} — TODO {}", rel.display(), i + 1, message)
                };
                hotspots.push(entry);
            }
        }
    }
    // Stable order: by path then line — the per-file scan above already gives
    // us this, but if multiple files emit hits we sort to keep output reviewable.
    hotspots.sort();
    if !hotspots.is_empty() {
        result.next_steps.push(format!(
            "{} hand-edit hotspot(s) marked `// TODO` in generated files:",
            hotspots.len()
        ));
        result.next_steps.extend(hotspots);
    }
}

enum SerdePatch {
    AlreadyOk,
    Patched(std::path::PathBuf),
    PresentWithoutDeriveFeature,
    NoCargoToml,
}

/// Check whether the crate's Cargo.toml already pulls in `serde` with the
/// `derive` feature. If not present at all, append a serde dep line under
/// `[dependencies]`. If present without the derive feature, return a marker so
/// the caller can emit a manual-fix hint (re-writing an existing dep table
/// entry risks clobbering other settings the user authored).
fn ensure_serde_in_cargo_toml(crate_root: &Path) -> Result<SerdePatch, String> {
    let path = crate_root.join("Cargo.toml");
    if !path.exists() {
        return Ok(SerdePatch::NoCargoToml);
    }
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let parsed: toml::Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;

    let serde_value = parsed
        .get("dependencies")
        .and_then(|d| d.as_table())
        .and_then(|t| t.get("serde"));
    match serde_value {
        Some(v) => {
            // Either a bare version string (no features) or a table — both need
            // a `derive` feature for `#[derive(Serialize, Deserialize)]`.
            let has_derive = v
                .as_table()
                .and_then(|t| t.get("features"))
                .and_then(|f| f.as_array())
                .map(|arr| arr.iter().any(|x| x.as_str() == Some("derive")))
                .unwrap_or(false);
            if has_derive {
                Ok(SerdePatch::AlreadyOk)
            } else {
                Ok(SerdePatch::PresentWithoutDeriveFeature)
            }
        }
        None => {
            let new_text = append_dep_to_cargo_toml(
                &text,
                "serde",
                r#"serde = { version = "1", features = ["derive"] }"#,
            )?;
            std::fs::write(&path, new_text).map_err(|e| e.to_string())?;
            Ok(SerdePatch::Patched(path))
        }
    }
}

/// Append a new dep line into an existing `[dependencies]` table (or create
/// the table at the end of the file if it doesn't exist). Preserves the
/// user's existing formatting elsewhere — we only inject a single new line.
fn append_dep_to_cargo_toml(text: &str, dep_name: &str, line: &str) -> Result<String, String> {
    let lines: Vec<&str> = text.lines().collect();
    // Find the `[dependencies]` header; only the literal `[dependencies]` table
    // (not `[dependencies.foo]` sub-tables, which write a single dep each).
    let header_idx = lines.iter().position(|l| l.trim() == "[dependencies]");
    if let Some(idx) = header_idx {
        // Insert right after the header (top of the table block).
        let mut new_lines: Vec<String> = lines.iter().map(|s| (*s).to_string()).collect();
        // Skip past contiguous blank lines just after the header to land below
        // any header-attached blank line.
        let mut insert_at = idx + 1;
        while insert_at < new_lines.len() && new_lines[insert_at].trim().is_empty() {
            insert_at += 1;
        }
        new_lines.insert(insert_at, line.to_string());
        let mut out = new_lines.join("\n");
        if text.ends_with('\n') && !out.ends_with('\n') {
            out.push('\n');
        }
        Ok(out)
    } else {
        // No [dependencies] section at all — append one.
        let mut out = text.to_string();
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("\n[dependencies]\n");
        out.push_str(line);
        out.push('\n');
        let _ = dep_name;
        Ok(out)
    }
}

/// Order-preserving dedup. `files_modified` in particular accumulates one
/// entry per route/component insertion (e.g. src/main.rs and src/components/mod.rs
/// show up dozens of times in a resource scaffold); deduping keeps the response
/// scannable.
fn dedup_paths(v: &mut Vec<std::path::PathBuf>) {
    let mut seen: BTreeSet<std::path::PathBuf> = BTreeSet::new();
    v.retain(|p| seen.insert(p.clone()));
}

/// Return the unique set of top-level src/{module}/ subdirs that received at
/// least one emitted file. Used to drive crate-root `pub mod` injection.
fn top_level_modules_touched(result: &ScaffoldResult, crate_root: &Path) -> Vec<String> {
    let src = crate_root.join("src");
    let mut out: BTreeSet<String> = BTreeSet::new();
    let scan = |paths: &Vec<std::path::PathBuf>, out: &mut BTreeSet<String>| {
        for p in paths {
            let Ok(rel) = p.strip_prefix(&src) else {
                continue;
            };
            let mut comps = rel.components();
            let Some(first) = comps.next() else { continue };
            // Only count entries that are *inside* a subdir (i.e. there's
            // another component after the first) — a bare `src/main.rs` edit
            // isn't a module subdir.
            if comps.next().is_none() {
                continue;
            }
            if let std::path::Component::Normal(name) = first
                && let Some(n) = name.to_str()
            {
                out.insert(n.to_string());
            }
        }
    };
    scan(&result.files_created, &mut out);
    scan(&result.files_modified, &mut out);
    out.into_iter().collect()
}

fn has_extra_documents(yaml: &str) -> bool {
    // A leading "---" is a valid single-document marker; multiple "---" lines
    // (or any "---" after non-whitespace content) means multi-document.
    let mut seen_content = false;
    for line in yaml.lines() {
        let trimmed = line.trim_end();
        if trimmed == "---" {
            if seen_content {
                return true;
            }
        } else if !line.trim().is_empty() && !line.trim_start().starts_with('#') {
            seen_content = true;
        }
    }
    false
}

fn merge(into: &mut ScaffoldResult, from: ScaffoldResult) {
    into.files_created.extend(from.files_created);
    into.files_modified.extend(from.files_modified);
    into.next_steps.extend(from.next_steps);
    into.collisions.extend(from.collisions);
    into.would_create.extend(from.would_create);
    into.would_modify.extend(from.would_modify);
}

fn leaf_for(crate_root: &Path, subdir: &str, name: &str) -> std::path::PathBuf {
    let snake = name.to_snake_case();
    crate_root.join(subdir).join(format!("{snake}.rs"))
}

/// If `target` is in the skip set, record it as a collision and return true.
fn skip_or_record(
    skip: &BTreeSet<std::path::PathBuf>,
    result: &mut ScaffoldResult,
    target: std::path::PathBuf,
) -> bool {
    if skip.contains(&target) {
        result.collisions.push(target);
        true
    } else {
        false
    }
}

/// Walk the doc and return the set of leaf files that already exist on disk —
/// the primitives whose target file should be skipped in `if_missing` mode.
fn skip_set(
    doc: &DslDoc,
    synth_server_fns: &[SynthServerFn],
    crate_root: &Path,
) -> BTreeSet<std::path::PathBuf> {
    let mut s = BTreeSet::new();
    let mut maybe_add = |subdir: &str, name: &str| {
        let p = leaf_for(crate_root, subdir, name);
        if p.exists() {
            s.insert(p);
        }
    };
    for c in &doc.components {
        maybe_add("src/components", &c.name);
    }
    for f in &doc.forms {
        maybe_add("src/components", &f.name);
    }
    for l in &doc.lists {
        maybe_add("src/components", &l.name);
    }
    for t in &doc.tables {
        maybe_add("src/components", &t.name);
    }
    for f in &doc.feeds {
        maybe_add("src/components", &f.name);
    }
    for ls in &doc.login_screens {
        maybe_add("src/components", &ls.name);
    }
    for pr in &doc.protected_routes {
        maybe_add("src/components", &pr.name);
    }
    for sc in &doc.screens {
        maybe_add("src/components", &sc.name);
    }
    for sf in &doc.server_fns {
        maybe_add("src/server", &sf.name);
    }
    for sig in &doc.signals {
        maybe_add("src/signals", &sig.name);
    }
    for sk in &doc.sockets {
        maybe_add("src/sockets", &sk.name);
    }
    for ss in &doc.session_states {
        maybe_add("src/auth", &ss.name);
    }
    for m in &doc.models {
        maybe_add("src/model", &m.name);
    }
    for st in &doc.stores {
        maybe_add("src/state", &st.name);
    }
    for cs in &doc.client_stores {
        maybe_add("src/state", &cs.name);
    }
    for sf in synth_server_fns {
        maybe_add("src/server", &sf.name);
    }
    s
}

/// Compute the would-be plan for a dry-run: for every primitive in `doc`,
/// classify its leaf file as `would_create` (path is free) or `collisions`
/// (path already exists), and classify the parent `mod.rs` plus any touched
/// router file as `would_create` / `would_modify`.
fn plan_dsl(doc: &DslDoc, synth_server_fns: &[SynthServerFn], crate_root: &Path) -> ScaffoldResult {
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

fn modify_target_path(m: &DslModify, crate_root: &Path) -> std::path::PathBuf {
    match m {
        DslModify::AddModelField { model, .. } => leaf_for(crate_root, "src/model", model),
        DslModify::AddComponentProp { component, .. } => {
            leaf_for(crate_root, "src/components", component)
        }
        DslModify::AddServerFnArg { server_fn, .. } => {
            leaf_for(crate_root, "src/server", server_fn)
        }
    }
}

// ---------- pre-flight ----------

fn preflight(
    doc: &DslDoc,
    synth_server_fns: &[SynthServerFn],
    crate_root: &Path,
    if_missing: bool,
) -> Result<(), String> {
    // 1. Collect every snake_case name across every primitive and reject dups
    //    that would land in the same target directory.
    let mut comp_names: BTreeSet<String> = BTreeSet::new();
    let mut sig_names: BTreeSet<String> = BTreeSet::new();
    let mut sock_names: BTreeSet<String> = BTreeSet::new();
    let mut srv_names: BTreeSet<String> = BTreeSet::new();
    let mut sess_names: BTreeSet<String> = BTreeSet::new();
    let mut model_names: BTreeSet<String> = BTreeSet::new();
    let mut store_names: BTreeSet<String> = BTreeSet::new();

    let mut comp_dup = |name: &str| -> Result<(), String> {
        let s = name.to_snake_case();
        if !comp_names.insert(s.clone()) {
            return Err(format!("duplicate component-target name: {s}"));
        }
        Ok(())
    };

    for c in &doc.components {
        comp_dup(&c.name)?;
    }
    for f in &doc.forms {
        comp_dup(&f.name)?;
    }
    for l in &doc.lists {
        comp_dup(&l.name)?;
    }
    for t in &doc.tables {
        comp_dup(&t.name)?;
    }
    for f in &doc.feeds {
        comp_dup(&f.name)?;
    }
    for ls in &doc.login_screens {
        comp_dup(&ls.name)?;
    }
    for pr in &doc.protected_routes {
        comp_dup(&pr.name)?;
    }
    for sc in &doc.screens {
        comp_dup(&sc.name)?;
    }

    for s in &doc.signals {
        if !sig_names.insert(s.name.to_snake_case()) {
            return Err(format!("duplicate signal name: {}", s.name));
        }
    }
    for s in &doc.sockets {
        if !sock_names.insert(s.name.to_snake_case()) {
            return Err(format!("duplicate socket name: {}", s.name));
        }
    }
    for s in &doc.server_fns {
        if !srv_names.insert(s.name.to_snake_case()) {
            return Err(format!("duplicate server_fn name: {}", s.name));
        }
    }
    for s in synth_server_fns {
        if !srv_names.insert(s.name.to_snake_case()) {
            return Err(format!(
                "resources: expansion produced server_fn {:?} which collides with an explicit `server_fns:` entry — rename or remove the conflict",
                s.name
            ));
        }
    }
    for s in &doc.stores {
        if !store_names.insert(s.name.to_snake_case()) {
            return Err(format!("duplicate store name: {}", s.name));
        }
    }
    let mut client_store_names: BTreeSet<String> = BTreeSet::new();
    for cs in &doc.client_stores {
        let snake = cs.name.to_snake_case();
        if !client_store_names.insert(snake.clone()) {
            return Err(format!("duplicate client_store name: {}", cs.name));
        }
        if store_names.contains(&snake) {
            return Err(format!(
                "client_store {:?} collides with store {:?} — both write to src/state/{snake}.rs; rename one",
                cs.name, cs.name
            ));
        }
    }
    for s in &doc.session_states {
        if !sess_names.insert(s.name.to_snake_case()) {
            return Err(format!("duplicate session_state name: {}", s.name));
        }
    }
    for m in &doc.models {
        let snake = m.name.to_snake_case();
        if !model_names.insert(snake.clone()) {
            return Err(format!("duplicate model name: {}", m.name));
        }
        let mut seen_field: BTreeSet<String> = BTreeSet::new();
        for f in &m.fields {
            let fs = f.name.to_snake_case();
            if !seen_field.insert(fs) {
                return Err(format!(
                    "model {:?} declares duplicate field {:?}",
                    m.name, f.name
                ));
            }
        }
    }

    // 2. Verify cross-references exist within the doc.
    for f in &doc.feeds {
        if !sock_names.contains(&f.socket.to_snake_case()) {
            return Err(format!(
                "feed {:?} references unknown socket {:?}",
                f.name, f.socket
            ));
        }
    }
    for l in &doc.lists {
        if !srv_names.contains(&l.endpoint.to_snake_case()) {
            return Err(format!(
                "list {:?} references unknown server_fn {:?}; declare it under server_fns",
                l.name, l.endpoint
            ));
        }
    }
    for t in &doc.tables {
        if !srv_names.contains(&t.endpoint.to_snake_case()) {
            return Err(format!(
                "table {:?} references unknown server_fn {:?}; declare it under server_fns",
                t.name, t.endpoint
            ));
        }
    }
    let list_names: BTreeSet<String> = doc.lists.iter().map(|l| l.name.to_snake_case()).collect();
    for f in &doc.forms {
        if let Some(target) = &f.feeds_into
            && !list_names.contains(&target.to_snake_case())
        {
            return Err(format!(
                "form {:?} feeds_into unknown list {:?}; declare it under lists",
                f.name, target
            ));
        }
    }
    for pr in &doc.protected_routes {
        if let Some(req) = &pr.requires
            && !sess_names.contains(&req.to_snake_case())
        {
            return Err(format!(
                "protected_route {:?} requires unknown session_state {:?}; declare it under session_states",
                pr.name, req
            ));
        }
    }
    for s in &doc.stores {
        if !model_names.contains(&s.resource.to_snake_case()) {
            return Err(format!(
                "store {:?} references unknown model {:?}; declare it under models",
                s.name, s.resource
            ));
        }
    }
    for sc in &doc.screens {
        if let Some(tpl) = &sc.template
            && tpl.kind == "client_crud"
        {
            let store = tpl.store.as_deref().ok_or_else(|| {
                format!(
                    "screen {:?} kind=client_crud requires `store:` (a client_stores name)",
                    sc.name
                )
            })?;
            if !client_store_names.contains(&store.to_snake_case()) {
                return Err(format!(
                    "screen {:?} references unknown client_store {:?}; declare it under client_stores",
                    sc.name, store
                ));
            }
            if tpl.label_field.is_none() {
                return Err(format!(
                    "screen {:?} kind=client_crud requires `label_field`",
                    sc.name
                ));
            }
        }
    }

    // 3. Validate `modify:` entries — non-empty, no duplicate field/arg/prop
    //    names within a single entry. Cross-doc references aren't required:
    //    the target item is allowed to exist only on disk.
    for (i, m) in doc.modify.iter().enumerate() {
        let (kind, names): (&str, Vec<String>) = match m {
            DslModify::AddModelField { fields, .. } => {
                if fields.is_empty() {
                    return Err(format!(
                        "modify[{i}] add_model_field: `fields` is empty — nothing to add"
                    ));
                }
                (
                    "add_model_field",
                    fields.iter().map(|f| f.name.to_snake_case()).collect(),
                )
            }
            DslModify::AddComponentProp { props, .. } => {
                if props.is_empty() {
                    return Err(format!(
                        "modify[{i}] add_component_prop: `props` is empty — nothing to add"
                    ));
                }
                (
                    "add_component_prop",
                    props.iter().map(|p| p.name.to_snake_case()).collect(),
                )
            }
            DslModify::AddServerFnArg { args, .. } => {
                if args.is_empty() {
                    return Err(format!(
                        "modify[{i}] add_server_fn_arg: `args` is empty — nothing to add"
                    ));
                }
                (
                    "add_server_fn_arg",
                    args.iter().map(|a| a.name.to_snake_case()).collect(),
                )
            }
        };
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for n in &names {
            if !seen.insert(n.clone()) {
                return Err(format!(
                    "modify[{i}] {kind}: duplicate name {n:?} in the entry"
                ));
            }
        }
    }

    // 4. Pre-check files that would collide with what's already on disk for
    //    each component-target name. (server_fn / signal / socket / state
    //    dirs may not exist yet; existence isn't an error there.) Suppressed
    //    when `if_missing` is set — those collisions become skip entries
    //    instead.
    if !if_missing {
        let comp_dir = crate_root.join("src/components");
        for n in &comp_names {
            if comp_dir.join(format!("{n}.rs")).exists() {
                return Err(format!(
                    "src/components/{n}.rs already exists; refusing to overwrite. \
                     Pass `if_missing: true` to skip existing primitives instead of erroring."
                ));
            }
        }
        let state_dir = crate_root.join("src/state");
        for n in &store_names {
            if state_dir.join(format!("{n}.rs")).exists() {
                return Err(format!(
                    "src/state/{n}.rs already exists; refusing to overwrite. \
                     Pass `if_missing: true` to skip existing primitives instead of erroring."
                ));
            }
        }
        for n in &client_store_names {
            if state_dir.join(format!("{n}.rs")).exists() {
                return Err(format!(
                    "src/state/{n}.rs already exists; refusing to overwrite. \
                     Pass `if_missing: true` to skip existing primitives instead of erroring."
                ));
            }
        }
    }

    Ok(())
}

/// If the doc declares any routable primitive (Screen, LoginScreen) and no
/// Routable enum exists anywhere under src/, write a minimal `src/router.rs`
/// seeded with every declared route, and inject `pub mod router;` into the
/// crate root. Makes `dx new` → `execute_code` runnable in one call instead
/// of erroring on the first screen with "no Routable enum on disk".
///
/// Returns the list of paths created/modified by the bootstrap (caller merges
/// these into the top-level result so the response stays honest).
fn bootstrap_router_if_needed(doc: &DslDoc, crate_root: &Path) -> Result<BootstrapRouter, String> {
    if scaffold::find_routable(crate_root).is_some() {
        return Ok(BootstrapRouter::default());
    }
    // Order matches declaration order in the doc: login_screens first (so the
    // login route lands before any post-auth screens), then screens.
    struct SeedRoute {
        variant: String,
        path: String,
        params: Vec<(String, String)>,
    }
    let mut entries: Vec<SeedRoute> = Vec::new();
    for ls in &doc.login_screens {
        entries.push(SeedRoute {
            variant: ls.name.to_pascal_case(),
            path: ls.route.clone(),
            params: Vec::new(),
        });
    }
    for sc in &doc.screens {
        entries.push(SeedRoute {
            variant: sc.name.to_pascal_case(),
            path: sc.route.clone(),
            params: sc.route_params.clone(),
        });
    }
    if entries.is_empty() {
        return Ok(BootstrapRouter::default());
    }
    let mut body = String::from("use dioxus::prelude::*;\n\n");
    body.push_str("#[derive(Routable, Clone, PartialEq)]\n");
    body.push_str("pub enum Route {\n");
    for SeedRoute {
        variant,
        path,
        params,
    } in &entries
    {
        let field_inner = if params.is_empty() {
            String::new()
        } else {
            format!(
                " {} ",
                params
                    .iter()
                    .map(|(n, t)| format!("{n}: {t}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        body.push_str(&format!("    #[route(\"{path}\")]\n"));
        body.push_str(&format!("    {variant} {{{field_inner}}},\n"));
    }
    body.push_str("}\n");

    let router_path = crate_root.join("src/router.rs");
    if let Some(parent) = router_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&router_path, body).map_err(|e| e.to_string())?;

    let mut out = BootstrapRouter {
        created: vec![router_path],
        modified: Vec::new(),
        next_step: Some(
            "auto-created `src/router.rs` with a Routable enum seeded from the declared screens — \
             mount it in your App component as `Router::<crate::router::Route> {}` (and make sure \
             your Cargo.toml's `dioxus` dep includes the `router` feature, which `dx new` enables \
             via `fullstack`)."
                .into(),
        ),
    };
    if let Some(p) = scaffold::upsert_crate_mod(crate_root, "router")? {
        out.modified.push(p);
    }
    Ok(out)
}

#[derive(Default)]
struct BootstrapRouter {
    created: Vec<std::path::PathBuf>,
    modified: Vec<std::path::PathBuf>,
    next_step: Option<String>,
}

// ---------- per-primitive generators ----------

fn render(name: &str, tpl: &str, ctx: minijinja::Value) -> Result<String, String> {
    let mut env = Environment::new();
    env.add_template(name, tpl).map_err(|e| e.to_string())?;
    env.get_template(name)
        .map_err(|e| e.to_string())?
        .render(ctx)
        .map_err(|e| e.to_string())
}

fn write_component_file(
    crate_root: &Path,
    snake: &str,
    body: String,
) -> Result<ScaffoldResult, String> {
    write_module_file(crate_root, "src/components", snake, body)
}

fn write_module_file(
    crate_root: &Path,
    subdir: &str,
    snake: &str,
    body: String,
) -> Result<ScaffoldResult, String> {
    // src/state/ entries declare server-only store modules; without the
    // matching cfg gate on the `pub mod`/`pub use` lines, the wasm (web-only)
    // build fails with E0432 because the file is `#![cfg(feature = "server")]`
    // and effectively absent. ClientStore lives in the same dir but is NOT
    // server-only; it uses `write_module_file_with_cfg(... None)` directly.
    let cfg_attr = if subdir == "src/state" {
        Some("#[cfg(feature = \"server\")]")
    } else {
        None
    };
    write_module_file_with_cfg(crate_root, subdir, snake, body, cfg_attr)
}

fn write_module_file_with_cfg(
    crate_root: &Path,
    subdir: &str,
    snake: &str,
    body: String,
    cfg_attr: Option<&str>,
) -> Result<ScaffoldResult, String> {
    let dir = crate_root.join(subdir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let target = dir.join(format!("{snake}.rs"));
    if target.exists() {
        return Err(format!("{} already exists", target.display()));
    }
    std::fs::write(&target, body).map_err(|e| e.to_string())?;
    let mod_rs = dir.join("mod.rs");
    let upsert = upsert_mod_entry(&mod_rs, snake, cfg_attr)?;
    let (created, modified) = match upsert {
        ModUpsert::Created => (vec![target, mod_rs], vec![]),
        ModUpsert::Modified => (vec![target], vec![mod_rs]),
        ModUpsert::Unchanged => (vec![target], vec![]),
    };
    Ok(ScaffoldResult {
        files_created: created,
        files_modified: modified,
        ..Default::default()
    })
}

fn field_initial(ty: &str) -> &'static str {
    match ty {
        "checkbox" => "false",
        "number" => "0i64",
        _ => "String::new()",
    }
}

fn generate_form(crate_root: &Path, f: &DslForm) -> Result<ScaffoldResult, String> {
    let pascal = f.name.to_pascal_case();
    let snake = f.name.to_snake_case();

    let snake_field_names: Vec<String> =
        f.fields.iter().map(|fd| fd.name.to_snake_case()).collect();
    let snapshots = snake_field_names
        .iter()
        .map(|n| format!("                let {n}_v = {n}();"))
        .collect::<Vec<_>>()
        .join("\n");
    let arg_call = snake_field_names
        .iter()
        .map(|n| format!("{n}_v"))
        .collect::<Vec<_>>()
        .join(", ");
    let resets = f
        .fields
        .iter()
        .map(|fd| {
            let n = fd.name.to_snake_case();
            let init = field_initial(&fd.ty);
            format!("                        {n}.set({init});")
        })
        .collect::<Vec<_>>()
        .join("\n");

    let on_submit_body = match (&f.on_submit, &f.feeds_into) {
        (Some(h), Some(_)) => {
            let h = h.to_snake_case();
            let mut out = String::new();
            if !snapshots.is_empty() {
                out.push_str(&snapshots);
                out.push('\n');
            }
            out.push_str(&format!(
                "                spawn(async move {{\n                    if {h}({arg_call}).await.is_ok() {{\n"
            ));
            if !resets.is_empty() {
                out.push_str(&resets);
                out.push('\n');
            }
            out.push_str(
                "                        *version.write() += 1;\n                    }\n                });",
            );
            out
        }
        (Some(h), None) => {
            let h = h.to_snake_case();
            let mut out = String::new();
            if !snapshots.is_empty() {
                out.push_str(&snapshots);
                out.push('\n');
            }
            out.push_str(&format!(
                "                spawn(async move {{\n                    let _ = {h}({arg_call}).await;\n                }});"
            ));
            out
        }
        (None, Some(_)) => {
            "                // TODO submit handler\n                *version.write() += 1;"
                .to_string()
        }
        (None, None) => "                // TODO submit handler".to_string(),
    };

    let fields_ctx: Vec<_> = f
        .fields
        .iter()
        .map(|fd| {
            let initial = field_initial(&fd.ty);
            let input_type = match fd.ty.as_str() {
                "email" => "email",
                "password" => "password",
                "number" => "number",
                "checkbox" => "checkbox",
                "textarea" => "text",
                _ => "text",
            };
            let tag = if fd.ty == "textarea" {
                "textarea"
            } else {
                "input"
            };
            let validation = fd.validation.clone().unwrap_or_default();
            context! {
                name => fd.name.to_snake_case(),
                label => humanize(&fd.name),
                input_type => input_type,
                tag => tag,
                initial => initial,
                validation => validation,
            }
        })
        .collect();
    let feeds_into_snake = f.feeds_into.as_ref().map(|s| s.to_snake_case());
    let handler = f.on_submit.as_ref().map(|s| s.to_snake_case());
    let needs_handler_import = handler.is_some();
    let body = render(
        "form",
        FORM_TPL,
        context! {
            pascal => pascal.clone(),
            fields => fields_ctx,
            on_submit_body => on_submit_body,
            handler => handler,
            needs_handler_import => needs_handler_import,
            feeds_into_snake => feeds_into_snake,
        },
    )?;
    let mut r = write_component_file(crate_root, &snake, body)?;
    r.next_steps.push(format!(
        "import the form: `use crate::components::{pascal};`"
    ));
    if let Some(target) = &f.feeds_into {
        let t = target.to_snake_case();
        r.next_steps.push(format!(
            "render `{pascal}` inside the same parent that calls `provide_{t}_version()` so both share the version signal"
        ));
    }
    Ok(r)
}

fn generate_list(
    crate_root: &Path,
    l: &DslList,
    versioned: bool,
) -> Result<ScaffoldResult, String> {
    let pascal = l.name.to_pascal_case();
    let snake = l.name.to_snake_case();
    let endpoint = l.endpoint.to_snake_case();
    let body = render(
        "list",
        LIST_TPL,
        context! {
            pascal => pascal.clone(),
            snake => snake.clone(),
            endpoint => endpoint,
            item_type => l.item_type.clone(),
            versioned => versioned,
        },
    )?;
    let mut r = write_component_file(crate_root, &snake, body)?;
    if versioned {
        r.next_steps.push(format!(
            "call `crate::components::{snake}::provide_{snake}_version()` in the screen that hosts this list (and any forms feeding into it) before rendering them"
        ));
    }
    Ok(r)
}

fn generate_table(crate_root: &Path, t: &DslTable) -> Result<ScaffoldResult, String> {
    let pascal = t.name.to_pascal_case();
    let snake = t.name.to_snake_case();
    let endpoint = t.endpoint.to_snake_case();
    let cols: Vec<_> = t
        .columns
        .iter()
        .map(|c| {
            context! { name => c.name.clone(), label => c.label.clone() }
        })
        .collect();
    let body = render(
        "table",
        TABLE_TPL,
        context! {
            pascal => pascal.clone(),
            snake => snake.clone(),
            endpoint => endpoint,
            item_type => t.item_type.clone(),
            columns => cols,
        },
    )?;
    write_component_file(crate_root, &snake, body)
}

fn generate_signal(crate_root: &Path, s: &DslSignal) -> Result<ScaffoldResult, String> {
    let snake = s.name.to_snake_case();
    let body = render(
        "signal",
        SIGNAL_TPL,
        context! {
            snake => snake.clone(),
            ty => s.ty.clone(),
            initial => s.initial.clone(),
        },
    )?;
    write_module_file(crate_root, "src/signals", &snake, body)
}

fn generate_socket(crate_root: &Path, s: &DslSocket) -> Result<ScaffoldResult, String> {
    let snake = s.name.to_snake_case();
    let pascal = s.name.to_pascal_case();
    let upper = snake.to_uppercase();
    let body = render(
        "socket",
        SOCKET_TPL,
        context! {
            pascal => pascal,
            snake => snake.clone(),
            upper => upper,
            url => s.url.clone(),
        },
    )?;
    write_module_file(crate_root, "src/sockets", &snake, body)
}

fn generate_feed(crate_root: &Path, f: &DslFeed) -> Result<ScaffoldResult, String> {
    let pascal = f.name.to_pascal_case();
    let snake = f.name.to_snake_case();
    let socket_snake = f.socket.to_snake_case();
    let socket_pascal = f.socket.to_pascal_case();
    let body = render(
        "feed",
        FEED_TPL,
        context! {
            pascal => pascal,
            snake => snake.clone(),
            socket => socket_snake,
            socket_pascal => socket_pascal,
            item_type => f.item_type.clone(),
        },
    )?;
    write_component_file(crate_root, &snake, body)
}

fn generate_model(crate_root: &Path, m: &DslModel) -> Result<ScaffoldResult, String> {
    let pascal = m.name.to_pascal_case();
    let snake = m.name.to_snake_case();

    let defaults = ["Debug", "Clone", "PartialEq", "Serialize", "Deserialize"];
    let mut derives: Vec<String> = defaults.iter().map(|s| (*s).to_string()).collect();
    for extra in &m.derives {
        let t = extra.trim();
        if !t.is_empty() && !derives.iter().any(|d| d == t) {
            derives.push(t.to_string());
        }
    }
    let derives_str = derives.join(", ");

    let fields_ctx: Vec<_> = m
        .fields
        .iter()
        .map(|f| {
            context! {
                name => f.name.to_snake_case(),
                ty => f.ty.clone(),
                optional => f.optional,
            }
        })
        .collect();

    let body = render(
        "model",
        MODEL_TPL,
        context! {
            pascal => pascal,
            derives => derives_str,
            fields => fields_ctx,
        },
    )?;
    write_module_file(crate_root, "src/model", &snake, body)
}

fn generate_session(crate_root: &Path, s: &DslSessionState) -> Result<ScaffoldResult, String> {
    let snake = s.name.to_snake_case();
    let body = render(
        "session",
        SESSION_TPL,
        context! {
            snake => snake.clone(),
            user_type => s.user_type.clone(),
        },
    )?;
    write_module_file(crate_root, "src/auth", &snake, body)
}

async fn generate_login_screen(
    state: &Arc<State>,
    crate_root: &Path,
    ls: &DslLoginScreen,
    project_root: Option<&str>,
) -> Result<ScaffoldResult, String> {
    let pascal = ls.name.to_pascal_case();
    let snake = ls.name.to_snake_case();
    let body = render(
        "login",
        LOGIN_TPL,
        context! {
            pascal => pascal.clone(),
            redirect => ls.redirect_on_success.clone(),
        },
    )?;
    let mut r = write_component_file(crate_root, &snake, body)?;
    let route = scaffold::create_route(
        state,
        CreateRouteParams {
            path: ls.route.clone(),
            component: pascal.clone(),
            router_file: None,
            project_root: project_root.map(str::to_owned),
            params: Vec::new(),
        },
    )
    .await?;
    merge(&mut r, route);
    Ok(r)
}

fn generate_protected_route(
    crate_root: &Path,
    pr: &DslProtectedRoute,
    session_names: &BTreeSet<String>,
) -> Result<ScaffoldResult, String> {
    let pascal = pr.name.to_pascal_case();
    let snake = pr.name.to_snake_case();
    let session_snake = match &pr.requires {
        Some(s) => Some(s.to_snake_case()),
        None => session_names.iter().next().cloned(),
    };
    let body = render(
        "protected",
        PROTECTED_TPL,
        context! {
            pascal => pascal,
            redirect_to => pr.redirect_to.clone(),
            session_snake => session_snake.clone(),
        },
    )?;
    let mut r = write_component_file(crate_root, &snake, body)?;
    if session_snake.is_some() {
        r.next_steps.push(
            "make sure the SessionState's `provide_*` is called above any route wrapped by this guard".into(),
        );
    } else {
        r.next_steps.push(
            "no SessionState in the doc — wire your own session signal where the guard reads it"
                .into(),
        );
    }
    Ok(r)
}

async fn generate_screen(
    state: &Arc<State>,
    crate_root: &Path,
    sc: &DslScreen,
    client_stores: &[DslClientStore],
    project_root: Option<&str>,
) -> Result<ScaffoldResult, String> {
    let pascal = sc.name.to_pascal_case();
    let snake = sc.name.to_snake_case();
    let wrap_pascal = sc.wrap_with.as_ref().map(|w| w.to_pascal_case());

    let body = match &sc.template {
        None => render(
            "screen",
            SCREEN_TPL,
            context! {
                pascal => pascal.clone(),
                snake => snake.clone(),
                wrap_pascal => wrap_pascal.clone(),
            },
        )?,
        Some(t) => render_screen_template(
            crate_root,
            &pascal,
            &snake,
            wrap_pascal.as_deref(),
            client_stores,
            t,
        )?,
    };
    let mut r = write_component_file(crate_root, &snake, body)?;
    if let Some(w) = &wrap_pascal {
        r.next_steps.push(format!(
            "ensure `{w}` is exported from `crate::components` (e.g. emitted by a `protected_routes` entry or a hand-written component)"
        ));
    }
    let route = scaffold::create_route(
        state,
        CreateRouteParams {
            path: sc.route.clone(),
            component: pascal,
            router_file: None,
            project_root: project_root.map(str::to_owned),
            params: sc.route_params.clone(),
        },
    )
    .await?;
    merge(&mut r, route);
    Ok(r)
}

fn render_screen_template(
    crate_root: &Path,
    pascal: &str,
    snake: &str,
    wrap_pascal: Option<&str>,
    client_stores: &[DslClientStore],
    t: &DslScreenTemplate,
) -> Result<String, String> {
    match t.kind.as_str() {
        "empty" => render(
            "screen",
            SCREEN_TPL,
            context! {
                pascal => pascal,
                snake => snake,
                wrap_pascal => wrap_pascal,
            },
        ),
        "resource_list" => {
            // When CRUD ctx is attached (resource-synthesized), emit the rich
            // table with edit/delete actions. Otherwise fall back to the
            // simple list ladder for user-authored cases.
            if let Some(crud) = &t.crud {
                return render_resource_crud_list(crate_root, pascal, snake, wrap_pascal, crud);
            }
            let endpoint = t
                .endpoint
                .as_ref()
                .ok_or_else(|| {
                    format!("screen {pascal:?} template kind=resource_list requires `endpoint`")
                })?
                .to_snake_case();
            render(
                "screen_resource_list",
                SCREEN_RESOURCE_LIST_TPL,
                context! {
                    pascal => pascal,
                    snake => snake,
                    wrap_pascal => wrap_pascal,
                    endpoint => endpoint,
                },
            )
        }
        "resource_edit_form" => {
            let crud = t.crud.as_ref().ok_or_else(|| {
                format!(
                    "screen {pascal:?} kind=resource_edit_form is an internal template kind \
                     emitted by `resources:`; it cannot be used directly from a user-authored screen"
                )
            })?;
            render_resource_edit_form(pascal, snake, wrap_pascal, t, crud)
        }
        "resource_form" => {
            let submit = t
                .on_submit
                .as_ref()
                .or(t.endpoint.as_ref())
                .ok_or_else(|| {
                    format!(
                        "screen {pascal:?} template kind=resource_form requires `on_submit` or `endpoint`"
                    )
                })?
                .to_snake_case();
            let fields_ctx: Vec<_> = t
                .fields
                .iter()
                .map(|fd| {
                    let is_bool = fd.ty == "checkbox" || fd.rust_type.as_deref() == Some("bool");
                    let initial = if is_bool {
                        "false".to_string()
                    } else {
                        "String::new()".to_string()
                    };
                    let input_type = match fd.ty.as_str() {
                        "email" => "email",
                        "password" => "password",
                        "number" => "number",
                        "checkbox" => "checkbox",
                        "textarea" => "text",
                        _ => "text",
                    };
                    let tag = if fd.ty == "textarea" {
                        "textarea"
                    } else {
                        "input"
                    };
                    context! {
                        name => fd.name.to_snake_case(),
                        label => humanize(&fd.name),
                        input_type => input_type,
                        tag => tag,
                        initial => initial,
                        is_bool => is_bool,
                    }
                })
                .collect();
            let submit_body = resource_form_submit_body(t, &submit);
            render(
                "screen_resource_form",
                SCREEN_RESOURCE_FORM_TPL,
                context! {
                    pascal => pascal,
                    snake => snake,
                    wrap_pascal => wrap_pascal,
                    submit => submit,
                    item_type => t.item_type.clone(),
                    fields => fields_ctx,
                    submit_body => submit_body,
                    redirect_to => t.redirect_to.clone(),
                },
            )
        }
        "client_crud" => render_client_crud_screen(pascal, snake, wrap_pascal, client_stores, t),
        other => Err(format!(
            "unknown screen template kind {other:?} (expected: empty, resource_list, resource_form, client_crud)"
        )),
    }
}

fn render_client_crud_screen(
    pascal: &str,
    snake: &str,
    wrap_pascal: Option<&str>,
    client_stores: &[DslClientStore],
    t: &DslScreenTemplate,
) -> Result<String, String> {
    let store_ref = t.store.as_deref().ok_or_else(|| {
        format!("screen {pascal:?} kind=client_crud requires `store:` (a client_stores entry name)")
    })?;
    let store_snake = store_ref.to_snake_case();
    let store_cfg = client_stores
        .iter()
        .find(|cs| cs.name.to_snake_case() == store_snake)
        .ok_or_else(|| {
            format!(
                "screen {pascal:?} references unknown client_store {store_ref:?}; declare it under client_stores"
            )
        })?;
    let item_type = t
        .item_type
        .clone()
        .or_else(|| Some(store_cfg.item_type.clone()))
        .ok_or_else(|| format!("screen {pascal:?} kind=client_crud requires `item_type`"))?;
    let label_field = t
        .label_field
        .as_deref()
        .ok_or_else(|| format!("screen {pascal:?} kind=client_crud requires `label_field`"))?
        .to_snake_case();
    let checkbox_field = t.checkbox_field.as_deref().map(|s| s.to_snake_case());
    let id_field = store_cfg
        .id_field
        .as_deref()
        .ok_or_else(|| {
            format!(
                "screen {pascal:?} kind=client_crud requires the referenced client_store {store_ref:?} to declare `id_field` (delete/checkbox actions key off it)"
            )
        })?
        .to_snake_case();
    let id_type = store_cfg.id_type.clone().unwrap_or_else(|| "i64".into());
    // For integer ids we emit `1i64` etc. so the type of `next_id` is fixed
    // even before the first push. Non-integer id types fall back to bare `1`.
    let id_type_suffix = match id_type.as_str() {
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128"
        | "usize" => id_type.to_string(),
        _ => String::new(),
    };
    let has_id = !id_type_suffix.is_empty();
    let needs_model_import = store_cfg.item_type.to_snake_case() == item_type.to_snake_case();
    let humanized = humanize(&item_type);

    // Render the inner rsx body programmatically — the surrounding wrapper
    // (h1 / wrap_with / div) is filled in by CLIENT_CRUD_SCREEN_TPL.
    let mut body = String::new();
    let ind = if wrap_pascal.is_some() {
        "                "
    } else {
        "            "
    };
    body.push_str(&format!("{ind}h1 {{ \"{pascal}\" }}\n"));
    // "Add" form
    body.push_str(&format!("{ind}form {{ class: \"add\",\n"));
    body.push_str(&format!("{ind}    onsubmit: move |evt: FormEvent| {{\n"));
    body.push_str(&format!("{ind}        evt.prevent_default();\n"));
    body.push_str(&format!("{ind}        let value = draft();\n"));
    body.push_str(&format!("{ind}        if value.is_empty() {{ return; }}\n"));
    if has_id {
        body.push_str(&format!("{ind}        let id = next_id();\n"));
        body.push_str(&format!("{ind}        *next_id.write() += 1;\n"));
    }
    body.push_str(&format!("{ind}        store.push({item_type} {{\n"));
    if has_id {
        body.push_str(&format!("{ind}            {id_field}: id,\n"));
    }
    body.push_str(&format!("{ind}            {label_field}: value,\n"));
    body.push_str(&format!("{ind}            ..Default::default()\n"));
    body.push_str(&format!("{ind}        }});\n"));
    body.push_str(&format!("{ind}        draft.set(String::new());\n"));
    body.push_str(&format!("{ind}    }},\n"));
    body.push_str(&format!("{ind}    input {{\n"));
    body.push_str(&format!("{ind}        r#type: \"text\",\n"));
    body.push_str(&format!("{ind}        value: \"{{draft()}}\",\n"));
    body.push_str(&format!("{ind}        placeholder: \"New {humanized}\",\n"));
    body.push_str(&format!(
        "{ind}        oninput: move |e| draft.set(e.value()),\n"
    ));
    body.push_str(&format!("{ind}    }}\n"));
    body.push_str(&format!(
        "{ind}    button {{ r#type: \"submit\", \"Add\" }}\n"
    ));
    body.push_str(&format!("{ind}}}\n"));
    // List
    body.push_str(&format!("{ind}ul {{ class: \"{snake}-items\",\n"));
    body.push_str(&format!(
        "{ind}    for item in store.items.read().iter() {{\n"
    ));
    body.push_str(&format!(
        "{ind}        li {{ key: \"{{item.{id_field}}}\",\n"
    ));
    if let Some(cb) = &checkbox_field {
        body.push_str(&format!("{ind}            input {{\n"));
        body.push_str(&format!("{ind}                r#type: \"checkbox\",\n"));
        body.push_str(&format!(
            "{ind}                checked: \"{{item.{cb}}}\",\n"
        ));
        body.push_str(&format!("{ind}                oninput: {{\n"));
        body.push_str(&format!(
            "{ind}                    let id = item.{id_field}.clone();\n"
        ));
        body.push_str(&format!("{ind}                    move |_| {{\n"));
        body.push_str(&format!(
            "{ind}                        let id = id.clone();\n"
        ));
        body.push_str(&format!(
            "{ind}                        store.update_by_id(id, |t| t.{cb} = !t.{cb});\n"
        ));
        body.push_str(&format!("{ind}                    }}\n"));
        body.push_str(&format!("{ind}                }},\n"));
        body.push_str(&format!("{ind}            }}\n"));
    }
    body.push_str(&format!(
        "{ind}            span {{ \"{{item.{label_field}}}\" }}\n"
    ));
    body.push_str(&format!("{ind}            button {{ class: \"delete\",\n"));
    body.push_str(&format!("{ind}                onclick: {{\n"));
    body.push_str(&format!(
        "{ind}                    let id = item.{id_field}.clone();\n"
    ));
    body.push_str(&format!("{ind}                    move |_| {{\n"));
    body.push_str(&format!(
        "{ind}                        let id = id.clone();\n"
    ));
    body.push_str(&format!(
        "{ind}                        store.remove_by_id(id);\n"
    ));
    body.push_str(&format!("{ind}                    }}\n"));
    body.push_str(&format!("{ind}                }},\n"));
    body.push_str(&format!("{ind}                \"Delete\"\n"));
    body.push_str(&format!("{ind}            }}\n"));
    body.push_str(&format!("{ind}        }}\n"));
    body.push_str(&format!("{ind}    }}\n"));
    body.push_str(&format!("{ind}}}"));

    render(
        "client_crud_screen",
        CLIENT_CRUD_SCREEN_TPL,
        context! {
            pascal => pascal,
            snake => snake,
            wrap_pascal => wrap_pascal,
            store_snake => store_snake,
            item_type => item_type,
            needs_model_import => needs_model_import,
            has_id => has_id,
            id_type_suffix => id_type_suffix,
            body => body,
        },
    )
}

/// Locate the Routable enum on disk and return the import path callers can use
/// from a sibling component file (e.g. "crate::Route" when the enum is in
/// main.rs / lib.rs; "crate::router::Route" when in src/router.rs). Returns
/// None when no Routable enum is found, in which case the list template falls
/// back to plain `<a href>` links to avoid emitting un-compilable code.
fn detect_route_import(crate_root: &Path) -> Option<(String, String)> {
    let path = scaffold::find_routable(crate_root)?;
    let src_rel = path.strip_prefix(crate_root.join("src")).ok()?;
    let src = std::fs::read_to_string(&path).ok()?;
    let file = syn::parse_file(&src).ok()?;
    let enum_name = file.items.iter().find_map(|it| match it {
        syn::Item::Enum(e) => {
            let has_routable = e.attrs.iter().any(|a| {
                if !a.path().is_ident("derive") {
                    return false;
                }
                let mut found = false;
                let _ = a.parse_nested_meta(|m| {
                    if m.path.is_ident("Routable") {
                        found = true;
                    }
                    Ok(())
                });
                found
            });
            if has_routable {
                Some(e.ident.to_string())
            } else {
                None
            }
        }
        _ => None,
    })?;
    // Module path from crate root: drop the trailing `.rs`, treat `main` /
    // `lib` as the crate root (no module prefix), otherwise build
    // `crate::a::b::Enum` from the parent dirs + filename stem.
    let stem = src_rel.file_stem()?.to_str()?;
    let parent_components: Vec<String> = src_rel
        .parent()
        .into_iter()
        .flat_map(|p| p.components())
        .filter_map(|c| match c {
            std::path::Component::Normal(n) => n.to_str().map(String::from),
            _ => None,
        })
        .collect();
    let import = if (stem == "main" || stem == "lib") && parent_components.is_empty() {
        format!("crate::{enum_name}")
    } else {
        let mut segs = parent_components;
        segs.push(stem.to_string());
        format!("crate::{}::{}", segs.join("::"), enum_name)
    };
    Some((import, enum_name))
}

fn render_resource_crud_list(
    crate_root: &Path,
    pascal: &str,
    snake: &str,
    wrap_pascal: Option<&str>,
    crud: &CrudCtx,
) -> Result<String, String> {
    let columns: Vec<_> = crud
        .model_fields
        .iter()
        .map(|f| {
            let inner = strip_option(&f.ty).unwrap_or(&f.ty);
            let optional = f.optional || strip_option(&f.ty).is_some();
            // Non-Display fallback: custom types may not impl Display, so use
            // Debug. Users can post-edit if they want a different format.
            let is_primitive = matches!(
                inner,
                "String"
                    | "bool"
                    | "i8"
                    | "i16"
                    | "i32"
                    | "i64"
                    | "i128"
                    | "isize"
                    | "u8"
                    | "u16"
                    | "u32"
                    | "u64"
                    | "u128"
                    | "usize"
                    | "f32"
                    | "f64"
                    | "char"
            );
            let name = f.name.to_snake_case();
            // For Option<T> we want a *value* in the cell, not `Some(...)` /
            // `None` (Debug formatting); reach into the Option and render the
            // inner via Display (or empty string for None).
            let cell = if optional {
                if is_primitive {
                    format!("{{row.{name}.as_ref().map(|v| v.to_string()).unwrap_or_default()}}")
                } else {
                    // Non-Display inner — fall back to Debug of the inner value,
                    // still avoiding the Some(..)/None wrapper.
                    format!("{{row.{name}.as_ref().map(|v| format!(\"{{:?}}\", v)).unwrap_or_default()}}")
                }
            } else if is_primitive {
                format!("{{row.{name}}}")
            } else {
                format!("{{row.{name}:?}}")
            };
            context! {
                name => name,
                label => humanize(&f.name),
                cell => cell,
            }
        })
        .collect();
    // Build SPA-friendly Link expressions when we can resolve the Route enum
    // import path. Fall back to plain `a { href: ... }` when no Routable enum
    // is on disk (no router file yet) — that's at least correct.
    let route_link = detect_route_import(crate_root).map(|(import_path, enum_name)| {
        let new_variant = format!("{}NewScreen", crud.model_pascal);
        let edit_variant = format!("{}EditScreen", crud.model_pascal);
        context! {
            import_path => import_path,
            enum_name => enum_name,
            new_variant => new_variant,
            edit_variant => edit_variant,
            id_field => crud.id_field.clone(),
        }
    });

    render(
        "screen_resource_crud_list",
        SCREEN_RESOURCE_CRUD_LIST_TPL,
        context! {
            pascal => pascal,
            snake => snake,
            wrap_pascal => wrap_pascal,
            list_endpoint => crud.list_endpoint.clone(),
            delete_endpoint => crud.delete_endpoint.clone(),
            new_route => crud.new_route.clone(),
            list_route => crud.list_route.clone(),
            id_field => crud.id_field.clone(),
            humanized => humanize(&crud.model_pascal),
            columns => columns,
            route_link => route_link,
        },
    )
}

fn render_resource_edit_form(
    pascal: &str,
    snake: &str,
    wrap_pascal: Option<&str>,
    t: &DslScreenTemplate,
    crud: &CrudCtx,
) -> Result<String, String> {
    let fields_ctx: Vec<_> = t
        .fields
        .iter()
        .map(|fd| {
            let is_bool = fd.ty == "checkbox" || fd.rust_type.as_deref() == Some("bool");
            let input_type = match fd.ty.as_str() {
                "email" => "email",
                "password" => "password",
                "number" => "number",
                "checkbox" => "checkbox",
                "textarea" => "text",
                _ => "text",
            };
            let tag = if fd.ty == "textarea" {
                "textarea"
            } else {
                "input"
            };
            let signal_init_from_item = signal_init_from_item(fd);
            context! {
                name => fd.name.to_snake_case(),
                label => humanize(&fd.name),
                input_type => input_type,
                tag => tag,
                is_bool => is_bool,
                signal_init_from_item => signal_init_from_item,
            }
        })
        .collect();

    let submit_body = resource_edit_form_submit_body(t, crud);

    render(
        "screen_resource_edit_form",
        SCREEN_RESOURCE_EDIT_FORM_TPL,
        context! {
            pascal => pascal,
            snake => snake,
            wrap_pascal => wrap_pascal,
            model_pascal => crud.model_pascal.clone(),
            id_field => crud.id_field.clone(),
            id_type => crud.id_type.clone(),
            get_endpoint => crud.get_endpoint.clone(),
            update_endpoint => crud.update_endpoint.clone(),
            fields => fields_ctx,
            submit_body => submit_body,
        },
    )
}

/// Build the `use_signal(|| ...)` initializer expression for an edit-form
/// signal pre-populated from a loaded `item: Model`. Branches on the field's
/// rust_type + optional metadata.
fn signal_init_from_item(f: &DslFieldDef) -> String {
    let rust_ty = f.rust_type.as_deref().unwrap_or("String");
    let inner = strip_option(rust_ty).unwrap_or(rust_ty);
    let optional = f.optional || strip_option(rust_ty).is_some();
    let field_name = f.name.to_snake_case();
    let is_bool = inner == "bool";
    let is_string = inner == "String";

    if is_bool {
        return if optional {
            format!("item.{field_name}.unwrap_or(false)")
        } else {
            format!("item.{field_name}")
        };
    }
    if is_string {
        return if optional {
            format!("item.{field_name}.clone().unwrap_or_default()")
        } else {
            format!("item.{field_name}.clone()")
        };
    }
    // Numeric (or unknown): store as String so the input is editable.
    if optional {
        format!("item.{field_name}.map(|v| v.to_string()).unwrap_or_default()")
    } else {
        format!("item.{field_name}.to_string()")
    }
}

/// Build the submit body for the edit form. Preserves the original id and
/// calls the update_* server fn. Navigates to the list route on success.
fn resource_edit_form_submit_body(t: &DslScreenTemplate, crud: &CrudCtx) -> String {
    let indent = "                ";
    let mut out = String::new();
    for f in &t.fields {
        let n = f.name.to_snake_case();
        out.push_str(&format!("{indent}let {n}_v = {n}();\n"));
    }
    out.push_str(&format!("{indent}let id_v = original_id.clone();\n"));
    out.push_str(&format!("{indent}let item = {} {{\n", crud.model_pascal));
    out.push_str(&format!("{indent}    {}: id_v,\n", crud.id_field));
    for f in &t.fields {
        let n = f.name.to_snake_case();
        let val = field_submit_expr(f, &format!("{n}_v"));
        out.push_str(&format!("{indent}    {n}: {val},\n"));
    }
    out.push_str(&format!("{indent}    ..Default::default()\n"));
    out.push_str(&format!("{indent}}};\n"));
    let nav_line = format!("{indent}        nav.push(\"{}\");\n", crud.list_route);
    out.push_str(&format!(
        "{indent}spawn(async move {{\n{indent}    if {}(item).await.is_ok() {{\n{nav_line}{indent}    }}\n{indent}}});",
        crud.update_endpoint
    ));
    out
}

/// "stock_movement" or "StockMovement" → "Stock movement". Used for h1 / link
/// text on the synthesized CRUD screens.
fn humanize(s: &str) -> String {
    let snake = s.to_snake_case();
    let mut out = String::with_capacity(snake.len());
    for (i, ch) in snake.chars().enumerate() {
        if ch == '_' {
            out.push(' ');
        } else if i == 0 {
            for u in ch.to_uppercase() {
                out.push(u);
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Build the rust body that runs inside the form's onsubmit handler.
/// When `item_type` is set we attempt to construct it from the field signals
/// and call the submit fn with it. Otherwise we emit a TODO body.
///
/// Each field's submit-side expression is computed from its
/// `rust_type` + `optional` metadata (populated by `expand_resources` from the
/// source model). This produces compiling code for `String`, `Option<String>`,
/// integer/float (parsed from the String-backed signal), their Option variants,
/// and `bool`.
fn resource_form_submit_body(t: &DslScreenTemplate, submit: &str) -> String {
    let indent = "                ";
    let mut out = String::new();
    let has_item = t.item_type.is_some() && !t.fields.is_empty();

    if !t.fields.is_empty() {
        for f in &t.fields {
            let n = f.name.to_snake_case();
            out.push_str(&format!("{indent}let {n}_v = {n}();\n"));
        }
    }

    if has_item {
        let item_type = t.item_type.as_deref().unwrap();
        out.push_str(&format!("{indent}let item = {item_type} {{\n"));
        // Field assignment driven by the original Rust type when known.
        for f in &t.fields {
            let n = f.name.to_snake_case();
            let val = field_submit_expr(f, &format!("{n}_v"));
            out.push_str(&format!("{indent}    {n}: {val},\n"));
        }
        out.push_str(&format!("{indent}    ..Default::default()\n"));
        out.push_str(&format!("{indent}}};\n"));
        let nav_line = match &t.redirect_to {
            Some(r) => format!("{indent}        nav.push(\"{r}\");\n"),
            None => String::new(),
        };
        out.push_str(&format!(
            "{indent}spawn(async move {{\n{indent}    if {submit}(item).await.is_ok() {{\n{nav_line}{indent}    }}\n{indent}}});"
        ));
    } else if !t.fields.is_empty() {
        let arg_call = t
            .fields
            .iter()
            .map(|f| format!("{}_v", f.name.to_snake_case()))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "{indent}spawn(async move {{\n{indent}    let _ = {submit}({arg_call}).await;\n{indent}}});"
        ));
    } else {
        out.push_str(&format!(
            "{indent}// TODO call {submit}(...). Add `fields:` to the template to scaffold signals + inputs."
        ));
    }

    out
}

/// Build the Rust expression that converts a String-backed (or bool-backed)
/// signal snapshot into the model field's actual type. `signal_var` is the
/// local that already holds the snapshot (e.g. `"name_v"`).
fn field_submit_expr(f: &DslFieldDef, signal_var: &str) -> String {
    let rust_ty = f.rust_type.as_deref().unwrap_or("String");
    let inner = strip_option(rust_ty).unwrap_or(rust_ty);
    let is_numeric = matches!(
        inner,
        "i8" | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f32"
            | "f64"
    );
    let is_bool = inner == "bool";
    let is_string = inner == "String";

    let optional = f.optional || strip_option(rust_ty).is_some();

    if is_bool {
        // bool-backed signal already holds a bool — no parsing needed.
        return if optional {
            format!("Some({signal_var})")
        } else {
            signal_var.to_string()
        };
    }

    if is_numeric {
        let parse_expr = format!("{signal_var}.parse::<{inner}>().unwrap_or_default()");
        return if optional {
            format!(
                "if {signal_var}.is_empty() {{ None }} else {{ {signal_var}.parse::<{inner}>().ok() }}"
            )
        } else {
            parse_expr
        };
    }

    if is_string {
        return if optional {
            format!("if {signal_var}.is_empty() {{ None }} else {{ Some({signal_var}) }}")
        } else {
            signal_var.to_string()
        };
    }

    // Unknown type — fall back to a parse attempt for non-optional, or a TODO
    // wrapper for optional. The generated file is meant to be edited if the
    // model uses a custom type.
    if optional {
        format!(
            "if {signal_var}.is_empty() {{ None }} else {{ {signal_var}.parse::<{inner}>().ok() }}"
        )
    } else {
        format!("{signal_var}.parse::<{inner}>().unwrap_or_default()")
    }
}

/// If `ty` is an `Option<T>` (textually, with optional whitespace) returns `Some("T")`;
/// otherwise returns `None`. Naive, but adequate for the type strings we emit
/// from models (e.g. `Option<String>`, `Option<i64>`).
fn strip_option(ty: &str) -> Option<&str> {
    let t = ty.trim();
    let inner = t.strip_prefix("Option<")?.strip_suffix('>')?;
    Some(inner.trim())
}

// ---------- store + resource ----------

fn generate_store(crate_root: &Path, store: &DslStore) -> Result<ScaffoldResult, String> {
    let kind = store.kind.as_deref().unwrap_or("in_memory");
    if kind != "in_memory" {
        return Err(format!(
            "store {:?}: kind {kind:?} not implemented yet (only `in_memory`)",
            store.name
        ));
    }
    let store_pascal = store.name.to_pascal_case();
    let store_snake = store.name.to_snake_case();
    let res_pascal = store.resource.to_pascal_case();
    let id_field = store.id_field.as_deref().unwrap_or("id").to_snake_case();
    let id_type = store.id_type.as_deref().unwrap_or("i64").to_string();
    let emit_tests = store.emit_tests.unwrap_or(false);
    let body = render(
        "store",
        STORE_TPL,
        context! {
            store_pascal => store_pascal.clone(),
            res_pascal => res_pascal,
            id_field => id_field,
            id_type => id_type,
            emit_tests => emit_tests,
        },
    )?;
    let mut r = write_module_file(crate_root, "src/state", &store_snake, body)?;
    if emit_tests {
        r.next_steps.push(format!(
            "run `cargo test --features server -p <crate>` to execute the generated CRUD tests for {store_pascal}"
        ));
    }
    Ok(r)
}

fn generate_client_store(
    crate_root: &Path,
    cs: &DslClientStore,
    model_names: &BTreeSet<String>,
) -> Result<ScaffoldResult, String> {
    let pascal = cs.name.to_pascal_case();
    let snake = cs.name.to_snake_case();
    let item_type = cs.item_type.trim().to_string();
    let id_field = cs.id_field.as_ref().map(|s| s.to_snake_case());
    let id_type = cs.id_type.clone().unwrap_or_else(|| "i64".into());
    let initial = cs.initial.clone().unwrap_or_else(|| "Vec::new()".into());
    // Emit `use crate::model::ItemType;` when the type matches an in-doc model.
    let needs_model_import = model_names.contains(&item_type.to_snake_case());

    let body = render(
        "client_store",
        CLIENT_STORE_TPL,
        context! {
            pascal => pascal,
            snake => snake.clone(),
            item_type => item_type,
            needs_model_import => needs_model_import,
            id_field => id_field,
            id_type => id_type,
            initial => initial,
        },
    )?;
    // No server cfg gate — ClientStore runs in both wasm and server builds.
    let mut r = write_module_file_with_cfg(crate_root, "src/state", &snake, body, None)?;
    r.next_steps.push(format!(
        "call `crate::state::{snake}::provide_{snake}()` in the root component (or any ancestor of the screens that read it) before `use_{snake}()` is called"
    ));
    Ok(r)
}

#[derive(Debug, Clone)]
struct SynthServerFn {
    name: String,
    args: Vec<(String, String)>,
    return_type: String,
    method: &'static str,
    path: String,
    body: String,
}

/// Expand each `resources:` entry into the equivalent model + store + 5 server
/// fns + 2 screens. Synth server fns are returned separately because they
/// carry custom bodies that the standard server-fn generator can't emit.
fn expand_resources(doc: &mut DslDoc) -> Result<Vec<SynthServerFn>, String> {
    let resources = std::mem::take(&mut doc.resources);
    let mut synth = Vec::new();
    let mut existing_models: BTreeSet<String> =
        doc.models.iter().map(|m| m.name.to_snake_case()).collect();
    let mut existing_stores: BTreeSet<String> =
        doc.stores.iter().map(|s| s.name.to_snake_case()).collect();

    for r in &resources {
        let res_pascal = r.name.to_pascal_case();
        let res_snake = r.name.to_snake_case();
        let id_field = r.id_field.as_deref().unwrap_or("id").to_snake_case();
        if !r.fields.iter().any(|f| f.name.to_snake_case() == id_field) {
            return Err(format!(
                "resource {:?} must declare its id field {id_field:?} in `fields`",
                r.name
            ));
        }
        let id_type = r
            .fields
            .iter()
            .find(|f| f.name.to_snake_case() == id_field)
            .map(|f| f.ty.clone())
            .unwrap_or_else(|| "i64".into());
        // Explicit override wins; otherwise fall back to the built-in
        // pluralizer. Snake-case the override too so irregular forms still
        // produce valid URL slugs / fn names.
        let plural = r
            .plural
            .as_deref()
            .map(|p| p.to_snake_case())
            .unwrap_or_else(|| pluralize(&res_snake));
        // Default URL slugs are kebab-case (web convention): a model named
        // `StockMovement` lands at `/stock-movements`, not `/stock_movements`.
        // User-supplied `route_base` is taken verbatim.
        let route_base = r
            .route_base
            .clone()
            .unwrap_or_else(|| format!("/{}", plural.replace('_', "-")));
        let store_pascal = format!("{res_pascal}Store");
        let store_snake = format!("{res_snake}_store");

        // 1. Model — synthesize unless already declared. Default is forced
        // (here AND when patching an in-doc pre-declared model below) because
        // resource expansion turns on emit_tests for the store, and the
        // synthesized CRUD tests call `Model::default()`. Without this, tests
        // wouldn't compile.
        if existing_models.insert(res_snake.clone()) {
            let mut derives = r.derives.clone();
            if !derives.iter().any(|d| d == "Default") {
                derives.push("Default".into());
            }
            doc.models.push(DslModel {
                name: res_pascal.clone(),
                fields: r.fields.clone(),
                derives,
            });
        } else if let Some(m) = doc
            .models
            .iter_mut()
            .find(|m| m.name.to_snake_case() == res_snake)
            && !m.derives.iter().any(|d| d == "Default")
        {
            m.derives.push("Default".into());
        }

        // 2. Store — synthesize unless already declared.
        if existing_stores.insert(store_snake.clone()) {
            doc.stores.push(DslStore {
                name: store_pascal.clone(),
                resource: res_pascal.clone(),
                kind: Some("in_memory".into()),
                id_field: Some(id_field.clone()),
                id_type: Some(id_type.clone()),
                // Resource expansion forces Default on the synthesized model,
                // so the auto-generated CRUD tests will compile.
                emit_tests: Some(true),
            });
        }

        // 3. Server fns
        let store_path = format!("crate::state::{store_snake}::{store_pascal}");
        let list_name = format!("list_{plural}");
        let get_name = format!("get_{res_snake}");
        let create_name = format!("create_{res_snake}");
        let update_name = format!("update_{res_snake}");
        let delete_name = format!("delete_{res_snake}");

        let mk_body = |call: &str| {
            format!(
                "    #[cfg(feature = \"server\")]\n    {{\n        return Ok({call});\n    }}\n    #[cfg(not(feature = \"server\"))]\n    {{\n        unreachable!()\n    }}"
            )
        };

        synth.push(SynthServerFn {
            name: list_name.clone(),
            args: vec![],
            return_type: format!("Vec<crate::model::{res_pascal}>"),
            method: "get",
            path: format!("/api{route_base}"),
            body: mk_body(&format!("{store_path}::global().list()")),
        });
        synth.push(SynthServerFn {
            name: get_name.clone(),
            args: vec![("id".into(), id_type.clone())],
            return_type: format!("Option<crate::model::{res_pascal}>"),
            method: "post",
            path: format!("/api{route_base}/get"),
            body: mk_body(&format!("{store_path}::global().get(id)")),
        });
        synth.push(SynthServerFn {
            name: create_name.clone(),
            args: vec![("item".into(), format!("crate::model::{res_pascal}"))],
            return_type: format!("crate::model::{res_pascal}"),
            method: "post",
            path: format!("/api{route_base}"),
            body: mk_body(&format!("{store_path}::global().create(item)")),
        });
        synth.push(SynthServerFn {
            name: update_name.clone(),
            args: vec![("item".into(), format!("crate::model::{res_pascal}"))],
            return_type: format!("Option<crate::model::{res_pascal}>"),
            method: "post",
            path: format!("/api{route_base}/update"),
            body: mk_body(&format!("{store_path}::global().update(item)")),
        });
        synth.push(SynthServerFn {
            name: delete_name.clone(),
            args: vec![("id".into(), id_type.clone())],
            return_type: "bool".into(),
            method: "post",
            path: format!("/api{route_base}/delete"),
            body: mk_body(&format!("{store_path}::global().delete(id)")),
        });

        // 4. Screens: list + new + edit. The edit screen takes an `id`
        //    path-param so the Routable variant has `{ id: <id_type> }`.
        let list_screen = format!("{res_pascal}ListScreen");
        let new_screen = format!("{res_pascal}NewScreen");
        let edit_screen = format!("{res_pascal}EditScreen");
        let new_route = format!("{route_base}/new");
        let non_id_fields: Vec<DslFieldDef> = r
            .fields
            .iter()
            .filter(|f| f.name.to_snake_case() != id_field)
            .map(|f| DslFieldDef {
                name: f.name.clone(),
                ty: field_type_for_model_field(&f.ty),
                validation: None,
                rust_type: Some(f.ty.clone()),
                optional: f.optional,
            })
            .collect();

        let crud = CrudCtx {
            model_pascal: res_pascal.clone(),
            model_fields: r.fields.clone(),
            id_field: id_field.clone(),
            id_type: id_type.clone(),
            list_endpoint: list_name.clone(),
            get_endpoint: get_name.clone(),
            update_endpoint: update_name.clone(),
            delete_endpoint: delete_name.clone(),
            list_route: route_base.clone(),
            new_route: new_route.clone(),
        };

        doc.screens.push(DslScreen {
            name: list_screen,
            route: route_base.clone(),
            wrap_with: None,
            template: Some(DslScreenTemplate {
                kind: "resource_list".into(),
                endpoint: Some(list_name.clone()),
                item_type: Some(res_pascal.clone()),
                on_submit: None,
                redirect_to: None,
                fields: vec![],
                store: None,
                label_field: None,
                checkbox_field: None,
                crud: Some(crud.clone()),
            }),
            route_params: Vec::new(),
        });
        doc.screens.push(DslScreen {
            name: new_screen,
            route: new_route.clone(),
            wrap_with: None,
            template: Some(DslScreenTemplate {
                kind: "resource_form".into(),
                endpoint: Some(create_name.clone()),
                // Bare model name — the screen template emits the
                // `use crate::model::{item_type};` import itself.
                item_type: Some(res_pascal.clone()),
                on_submit: Some(create_name.clone()),
                redirect_to: Some(route_base.clone()),
                fields: non_id_fields.clone(),
                store: None,
                label_field: None,
                checkbox_field: None,
                crud: Some(crud.clone()),
            }),
            route_params: Vec::new(),
        });
        doc.screens.push(DslScreen {
            name: edit_screen,
            route: format!("{route_base}/:id/edit"),
            wrap_with: None,
            template: Some(DslScreenTemplate {
                kind: "resource_edit_form".into(),
                endpoint: Some(get_name.clone()),
                item_type: Some(res_pascal.clone()),
                on_submit: Some(update_name.clone()),
                redirect_to: Some(route_base.clone()),
                fields: non_id_fields,
                store: None,
                label_field: None,
                checkbox_field: None,
                crud: Some(crud),
            }),
            route_params: vec![("id".to_string(), id_type.clone())],
        });
    }
    Ok(synth)
}

/// Map a model field type onto the form-input kind used by the form template.
/// Anything non-trivial defaults to "text" — the user can post-edit.
fn field_type_for_model_field(ty: &str) -> String {
    match ty {
        "bool" => "checkbox".into(),
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize" | "isize" | "f32"
        | "f64" => "number".into(),
        _ => "text".into(),
    }
}

fn pluralize(snake: &str) -> String {
    if snake.ends_with('s')
        || snake.ends_with("sh")
        || snake.ends_with("ch")
        || snake.ends_with('x')
        || snake.ends_with('z')
    {
        format!("{snake}es")
    } else if snake.ends_with('y') {
        let chars: Vec<char> = snake.chars().collect();
        if chars.len() >= 2 && !"aeiou".contains(chars[chars.len() - 2]) {
            let mut s = snake.to_string();
            s.pop();
            s.push_str("ies");
            return s;
        }
        format!("{snake}s")
    } else {
        format!("{snake}s")
    }
}

async fn generate_synth_server_fn(
    state: &Arc<State>,
    crate_root: &Path,
    sf: &SynthServerFn,
    project_root: Option<&str>,
) -> Result<ScaffoldResult, String> {
    // Reuse the fullstack-capable check by detecting through ProjectInfo.
    let project = match project_root {
        Some(root) => crate::project::ProjectInfo::detect(std::path::Path::new(root)),
        None => state.project.lock().await.clone(),
    };
    let active = &project.dioxus_features;
    let fullstack_capable = active.iter().any(|f| f == "fullstack")
        || (active.iter().any(|f| f == "server") && active.iter().any(|f| f == "web"));
    if !fullstack_capable {
        return Err(
            "this project does not have `fullstack` (or `web`+`server`) enabled on the dioxus dep; \
             resource: server fns require a fullstack setup. Run audit_feature_flags for guidance."
                .into(),
        );
    }

    let snake = sf.name.to_snake_case();
    let server_dir = crate_root.join("src/server");
    std::fs::create_dir_all(&server_dir).map_err(|e| e.to_string())?;
    let target = server_dir.join(format!("{snake}.rs"));
    if target.exists() {
        return Err(format!("{} already exists", target.display()));
    }
    let body = render(
        "server_fn_body",
        SERVER_FN_WITH_BODY_TPL,
        context! {
            snake => snake.clone(),
            ret => sf.return_type.clone(),
            method => sf.method,
            path => sf.path.clone(),
            args => sf.args.iter().map(|(n, t)| context!{ name => n.clone(), ty => t.clone() }).collect::<Vec<_>>(),
            body => sf.body.clone(),
            extra_uses => Vec::<String>::new(),
        },
    )?;
    std::fs::write(&target, body).map_err(|e| e.to_string())?;
    let mod_rs = server_dir.join("mod.rs");
    let upsert = upsert_mod_entry(&mod_rs, &snake, None)?;
    let (files_created, files_modified) = match upsert {
        ModUpsert::Created => (vec![target, mod_rs], vec![]),
        ModUpsert::Modified => (vec![target], vec![mod_rs]),
        ModUpsert::Unchanged => (vec![target], vec![]),
    };
    Ok(ScaffoldResult {
        files_created,
        files_modified,
        ..Default::default()
    })
}

// ===========================================================================
// modify: in-place edits
// ===========================================================================

fn apply_modify(
    crate_root: &Path,
    m: &DslModify,
    if_missing: bool,
    result: &mut ScaffoldResult,
) -> Result<(), String> {
    match m {
        DslModify::AddModelField { model, fields } => {
            let path = leaf_for(crate_root, "src/model", model);
            let struct_name = model.to_pascal_case();
            modify_struct_fields(&path, &struct_name, fields, if_missing, result, "model")
        }
        DslModify::AddComponentProp { component, props } => {
            let path = leaf_for(crate_root, "src/components", component);
            let props_name = format!("{}Props", component.to_pascal_case());
            modify_props_struct(&path, &props_name, props, if_missing, result)
        }
        DslModify::AddServerFnArg { server_fn, args } => {
            let path = leaf_for(crate_root, "src/server", server_fn);
            let snake = server_fn.to_snake_case();
            modify_fn_args(&path, &snake, args, if_missing, result)
        }
    }
}

fn missing_target(
    path: &Path,
    kind: &str,
    if_missing: bool,
    result: &mut ScaffoldResult,
) -> Result<bool, String> {
    if path.exists() {
        return Ok(false);
    }
    if if_missing {
        result.collisions.push(path.to_path_buf());
        Ok(true)
    } else {
        Err(format!(
            "modify: target {} for {kind} does not exist on disk; create it first or pass `if_missing: true` to skip",
            path.display()
        ))
    }
}

fn modify_struct_fields(
    path: &Path,
    struct_name: &str,
    fields: &[DslModelField],
    if_missing: bool,
    result: &mut ScaffoldResult,
    kind_label: &str,
) -> Result<(), String> {
    if missing_target(path, kind_label, if_missing, result)? {
        return Ok(());
    }
    let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let parsed =
        syn::parse_file(&src).map_err(|e| format!("modify: parse {}: {e}", path.display()))?;
    let target = parsed
        .items
        .iter()
        .find_map(|it| match it {
            syn::Item::Struct(s) if s.ident == struct_name => Some(s),
            _ => None,
        })
        .ok_or_else(|| format!("modify: no struct {struct_name} in {}", path.display()))?;
    let existing: BTreeSet<String> = target
        .fields
        .iter()
        .filter_map(|f| f.ident.as_ref().map(|i| i.to_string()))
        .collect();
    let new_fields: Vec<&DslModelField> = fields
        .iter()
        .filter(|f| !existing.contains(&f.name.to_snake_case()))
        .collect();
    if new_fields.is_empty() {
        return Ok(());
    }
    let insert_at = find_close_delim(&src, &format!("struct {struct_name}"), '{', '}')?;
    let mut insertion = String::new();
    for f in &new_fields {
        let n = f.name.to_snake_case();
        if f.optional {
            insertion.push_str(&format!("    pub {n}: Option<{}>,\n", f.ty));
        } else {
            insertion.push_str(&format!("    pub {n}: {},\n", f.ty));
        }
    }
    let new_src = splice(&src, insert_at, &insertion);
    std::fs::write(path, new_src).map_err(|e| e.to_string())?;
    if !result.files_modified.iter().any(|p| p == path) {
        result.files_modified.push(path.to_path_buf());
    }
    Ok(())
}

fn modify_props_struct(
    path: &Path,
    struct_name: &str,
    props: &[DslPropDef],
    if_missing: bool,
    result: &mut ScaffoldResult,
) -> Result<(), String> {
    if missing_target(path, "component", if_missing, result)? {
        return Ok(());
    }
    let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let parsed =
        syn::parse_file(&src).map_err(|e| format!("modify: parse {}: {e}", path.display()))?;
    let target = parsed.items.iter().find_map(|it| match it {
        syn::Item::Struct(s) if s.ident == struct_name => Some(s),
        _ => None,
    });
    let Some(target) = target else {
        return Err(format!(
            "modify: no struct {struct_name} in {} — convert the component to take props first (re-create it with `props:` declared) before adding more",
            path.display()
        ));
    };
    let existing: BTreeSet<String> = target
        .fields
        .iter()
        .filter_map(|f| f.ident.as_ref().map(|i| i.to_string()))
        .collect();
    let new_props: Vec<&DslPropDef> = props
        .iter()
        .filter(|p| !existing.contains(&p.name.to_snake_case()))
        .collect();
    if new_props.is_empty() {
        return Ok(());
    }
    let insert_at = find_close_delim(&src, &format!("struct {struct_name}"), '{', '}')?;
    let mut insertion = String::new();
    for p in &new_props {
        let n = p.name.to_snake_case();
        if p.optional {
            insertion.push_str(&format!(
                "    #[props(default)]\n    pub {n}: Option<{}>,\n",
                p.ty
            ));
        } else {
            insertion.push_str(&format!("    pub {n}: {},\n", p.ty));
        }
    }
    let new_src = splice(&src, insert_at, &insertion);
    std::fs::write(path, new_src).map_err(|e| e.to_string())?;
    if !result.files_modified.iter().any(|p| p == path) {
        result.files_modified.push(path.to_path_buf());
    }
    Ok(())
}

fn modify_fn_args(
    path: &Path,
    snake_name: &str,
    args: &[DslArgDef],
    if_missing: bool,
    result: &mut ScaffoldResult,
) -> Result<(), String> {
    if missing_target(path, "server_fn", if_missing, result)? {
        return Ok(());
    }
    let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let parsed =
        syn::parse_file(&src).map_err(|e| format!("modify: parse {}: {e}", path.display()))?;
    let target_fn = parsed
        .items
        .iter()
        .find_map(|it| match it {
            syn::Item::Fn(f) if f.sig.ident == snake_name => Some(f),
            _ => None,
        })
        .ok_or_else(|| format!("modify: no fn {snake_name} in {}", path.display()))?;
    let existing: BTreeSet<String> = target_fn
        .sig
        .inputs
        .iter()
        .filter_map(|arg| match arg {
            syn::FnArg::Typed(pt) => match pt.pat.as_ref() {
                syn::Pat::Ident(pi) => Some(pi.ident.to_string()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let new_args: Vec<&DslArgDef> = args
        .iter()
        .filter(|a| !existing.contains(&a.name.to_snake_case()))
        .collect();
    if new_args.is_empty() {
        return Ok(());
    }
    let insert_at = find_close_delim(&src, &format!("fn {snake_name}"), '(', ')')?;
    // Preserve the parameter list's trailing-comma convention. If the existing
    // last non-whitespace before the closing `)` is `,`, we just append. If
    // it's `(` (no args), we still emit fields with leading newline + indent.
    // Either way the generated lines below carry their own trailing commas.
    let mut insertion = String::new();
    for a in &new_args {
        insertion.push_str(&format!("    {}: {},\n", a.name.to_snake_case(), a.ty));
    }
    let new_src = splice(&src, insert_at, &insertion);
    std::fs::write(path, new_src).map_err(|e| e.to_string())?;
    if !result.files_modified.iter().any(|p| p == path) {
        result.files_modified.push(path.to_path_buf());
    }
    Ok(())
}

/// Locate the byte position of the matching close delimiter for the opening
/// `open` that appears after `anchor` in `src`. Naive depth count — adequate
/// for the generated files we operate on (no string/char literals containing
/// raw braces or parens). The caller has already syn-parsed the source.
fn find_close_delim(src: &str, anchor: &str, open: char, close: char) -> Result<usize, String> {
    let start = src
        .find(anchor)
        .ok_or_else(|| format!("could not locate {anchor:?} in source"))?;
    let after_open = src[start..]
        .find(open)
        .map(|i| start + i + open.len_utf8())
        .ok_or_else(|| format!("malformed {anchor}: no {open:?}"))?;
    let mut depth: i32 = 1;
    for (i, ch) in src[after_open..].char_indices() {
        if ch == open {
            depth += 1;
        } else if ch == close {
            depth -= 1;
            if depth == 0 {
                return Ok(after_open + i);
            }
        }
    }
    Err(format!("malformed {anchor}: no {close:?}"))
}

fn splice(src: &str, at: usize, insertion: &str) -> String {
    let mut out = String::with_capacity(src.len() + insertion.len());
    out.push_str(&src[..at]);
    out.push_str(insertion);
    out.push_str(&src[at..]);
    out
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// For each colocated spec block, take its `example:` mapping (which is a
    /// DslDoc fragment under one or more primitive sections) and deserialize
    /// it as a DslDoc with version "1" injected. Catches drift between the
    /// hand-authored spec text and the Rust structs.
    #[test]
    fn spec_examples_round_trip() {
        let blocks: &[(&str, &str)] = &[
            ("CORE_MODEL", CORE_MODEL),
            ("CORE_STORE", CORE_STORE),
            ("CORE_CLIENT_STORE", CORE_CLIENT_STORE),
            ("CORE_RESOURCE", CORE_RESOURCE),
            ("CORE_COMPONENT", CORE_COMPONENT),
            ("CORE_SCREEN", CORE_SCREEN),
            ("CORE_SERVER_FN", CORE_SERVER_FN),
            ("CORE_MODIFY", CORE_MODIFY),
            ("CRUD_FORM", CRUD_FORM),
            ("CRUD_LIST", CRUD_LIST),
            ("CRUD_TABLE", CRUD_TABLE),
            ("REALTIME_SIGNAL", REALTIME_SIGNAL),
            ("REALTIME_SOCKET", REALTIME_SOCKET),
            ("REALTIME_FEED", REALTIME_FEED),
            ("AUTH_SESSION", AUTH_SESSION),
            ("AUTH_LOGIN", AUTH_LOGIN),
            ("AUTH_PROTECTED", AUTH_PROTECTED),
        ];
        for (name, block) in blocks {
            let v: serde_yml::Value = serde_yml::from_str(block)
                .unwrap_or_else(|e| panic!("{name}: spec block isn't YAML: {e}"));
            let map = v
                .as_mapping()
                .unwrap_or_else(|| panic!("{name}: top level not a map"));
            let primitive_value = map
                .iter()
                .next()
                .map(|(_, v)| v)
                .unwrap_or_else(|| panic!("{name}: empty"));
            let example = primitive_value
                .as_mapping()
                .and_then(|m| m.get("example"))
                .unwrap_or_else(|| panic!("{name}: no example: field"));
            let example_map = example
                .as_mapping()
                .unwrap_or_else(|| panic!("{name}: example is not a map"));
            let mut doc_yaml = String::from("version: \"1\"\n");
            for (k, v) in example_map.iter() {
                let mut snippet =
                    serde_yml::to_string(&serde_yml::mapping::Mapping::from_iter([(
                        k.clone(),
                        v.clone(),
                    )]))
                    .unwrap();
                if !snippet.ends_with('\n') {
                    snippet.push('\n');
                }
                doc_yaml.push_str(&snippet);
            }
            let doc: DslDoc = serde_yml::from_str(&doc_yaml)
                .unwrap_or_else(|e| panic!("{name}: deserialize failed: {e}\nyaml:\n{doc_yaml}"));
            assert_eq!(doc.version, "1");
        }
    }

    #[tokio::test]
    async fn rejects_unknown_extension() {
        let dummy = std::sync::Arc::new(State::new(std::env::temp_dir()).unwrap());
        let r = get_dsl_spec(
            &dummy,
            GetDslSpecParams {
                extensions: vec!["bogus".into()],
            },
        )
        .await;
        assert!(r.is_err());
    }

    #[test]
    fn screen_template_wraps_with_when_set() {
        let out = render(
            "screen",
            SCREEN_TPL,
            context! {
                pascal => "HomeScreen",
                snake => "home_screen",
                wrap_pascal => Some("Dashboard"),
            },
        )
        .unwrap();
        assert!(
            out.contains("use crate::components::Dashboard;"),
            "expected import for Dashboard, got:\n{out}"
        );
        assert!(
            out.contains("Dashboard {"),
            "expected Dashboard {{ ... }} wrapper, got:\n{out}"
        );
        let body_start = out.find("rsx!").unwrap();
        let body = &out[body_start..];
        let dash_pos = body.find("Dashboard {").unwrap();
        let div_pos = body.find("div {").unwrap();
        assert!(
            dash_pos < div_pos,
            "Dashboard wrapper must be outside the div, got:\n{out}"
        );
    }

    #[test]
    fn screen_template_omits_wrapper_when_unset() {
        let out = render(
            "screen",
            SCREEN_TPL,
            context! {
                pascal => "HomeScreen",
                snake => "home_screen",
                wrap_pascal => None::<String>,
            },
        )
        .unwrap();
        assert!(
            !out.contains("Dashboard"),
            "expected no wrapper, got:\n{out}"
        );
        assert!(!out.contains("use crate::components::"));
    }

    #[test]
    fn plan_dsl_classifies_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/components")).unwrap();
        std::fs::write(root.join("src/components/existing.rs"), "// existing\n").unwrap();
        std::fs::write(
            root.join("src/components/mod.rs"),
            "pub mod existing;\npub use existing::*;\n",
        )
        .unwrap();

        let doc: DslDoc = serde_yml::from_str(
            r#"version: "1"
components:
  - name: Existing
  - name: New
"#,
        )
        .unwrap();
        let plan = plan_dsl(&doc, &[], root);
        assert!(plan.dry_run);
        assert!(
            plan.collisions.iter().any(|p| p.ends_with("existing.rs")),
            "expected existing.rs in collisions, got {:?}",
            plan.collisions
        );
        assert!(
            plan.would_create.iter().any(|p| p.ends_with("new.rs")),
            "expected new.rs in would_create, got {:?}",
            plan.would_create
        );
        assert!(
            plan.would_modify.iter().any(|p| p.ends_with("mod.rs")),
            "expected mod.rs in would_modify, got {:?}",
            plan.would_modify
        );
    }

    #[test]
    fn skip_set_collects_existing_leaf_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/components")).unwrap();
        std::fs::write(root.join("src/components/existing.rs"), "").unwrap();

        let doc: DslDoc = serde_yml::from_str(
            r#"version: "1"
components:
  - name: Existing
  - name: New
"#,
        )
        .unwrap();
        let skip = skip_set(&doc, &[], root);
        assert_eq!(skip.len(), 1);
        assert!(skip.iter().any(|p| p.ends_with("existing.rs")));
    }

    #[tokio::test]
    async fn if_missing_skips_existing_and_reports_collisions() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "if_missing_test"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = { version = "0.7", features = ["fullstack"] }
"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src/components")).unwrap();
        std::fs::write(
            root.join("src/components/existing.rs"),
            "// hand-written; do not touch\n",
        )
        .unwrap();

        let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
        let result = execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
components:
  - name: Existing
  - name: NewOne
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: true,
                dry_run: false,
            },
        )
        .await
        .expect("execute_code should succeed in if_missing mode");

        assert!(
            result.collisions.iter().any(|p| p.ends_with("existing.rs")),
            "expected existing.rs in collisions, got {:?}",
            result.collisions
        );
        let existing_body =
            std::fs::read_to_string(root.join("src/components/existing.rs")).unwrap();
        assert_eq!(
            existing_body, "// hand-written; do not touch\n",
            "if_missing must not overwrite the existing file"
        );
        assert!(
            root.join("src/components/new_one.rs").exists(),
            "non-conflicting components should still be created"
        );
    }

    #[tokio::test]
    async fn if_missing_skips_existing_model_server_fn_signal_session() {
        // The skip-set machinery covers every primitive — confirm it applies
        // uniformly so iterative re-runs (add one field, re-run) don't force
        // the user to manually delete pre-existing files.
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            cargo_toml_with_fullstack("if_missing_all_primitives"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src/model")).unwrap();
        std::fs::create_dir_all(root.join("src/server")).unwrap();
        std::fs::create_dir_all(root.join("src/signals")).unwrap();
        std::fs::create_dir_all(root.join("src/auth")).unwrap();
        // Pre-seed each with hand-written content.
        std::fs::write(root.join("src/model/widget.rs"), "// hand model\n").unwrap();
        std::fs::write(
            root.join("src/server/fetch_widgets.rs"),
            "// hand server fn\n",
        )
        .unwrap();
        std::fs::write(root.join("src/signals/counter.rs"), "// hand signal\n").unwrap();
        std::fs::write(root.join("src/auth/session.rs"), "// hand session\n").unwrap();

        let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
        let result = execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
models:
  - name: Widget
    fields:
      - {name: id, type: i64}
server_fns:
  - name: fetch_widgets
    return_type: String
signals:
  - name: counter
    type: i32
    initial: "0"
session_states:
  - name: session
    user_type: String
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: true,
                dry_run: false,
            },
        )
        .await
        .expect("if_missing should skip pre-existing primitives, not error");

        for stub in [
            "src/model/widget.rs",
            "src/server/fetch_widgets.rs",
            "src/signals/counter.rs",
            "src/auth/session.rs",
        ] {
            assert!(
                result.collisions.iter().any(|p| p.ends_with(stub)),
                "expected {stub} in collisions, got {:?}",
                result.collisions
            );
            let body = std::fs::read_to_string(root.join(stub)).unwrap();
            assert!(
                body.starts_with("// hand"),
                "if_missing must not overwrite {stub}, got: {body}"
            );
        }
    }

    #[tokio::test]
    async fn dry_run_returns_plan_without_writing() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "dry_run_test"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = { version = "0.7", features = ["fullstack"] }
"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();

        let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
        let result = execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
components:
  - name: Widget
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: false,
                dry_run: true,
            },
        )
        .await
        .expect("dry_run should succeed");

        assert!(result.dry_run);
        assert!(
            result.would_create.iter().any(|p| p.ends_with("widget.rs")),
            "expected widget.rs in would_create, got {:?}",
            result.would_create
        );
        assert!(
            !root.join("src/components/widget.rs").exists(),
            "dry_run must not write the file"
        );
    }

    #[test]
    fn detects_multidoc_yaml() {
        assert!(has_extra_documents("a: 1\n---\nb: 2"));
        assert!(!has_extra_documents("---\na: 1\nb: 2"));
        assert!(!has_extra_documents("# comment\na: 1"));
    }

    #[test]
    fn model_template_emits_struct_with_derives_and_optional_fields() {
        let m = DslModel {
            name: "Product".into(),
            fields: vec![
                DslModelField {
                    name: "id".into(),
                    ty: "i64".into(),
                    optional: false,
                },
                DslModelField {
                    name: "name".into(),
                    ty: "String".into(),
                    optional: false,
                },
                DslModelField {
                    name: "description".into(),
                    ty: "String".into(),
                    optional: true,
                },
            ],
            derives: vec!["Eq".into(), "Clone".into()],
        };
        let dir = tempfile::TempDir::new().unwrap();
        let r = generate_model(dir.path(), &m).unwrap();
        let path = dir.path().join("src/model/product.rs");
        assert!(r.files_created.iter().any(|p| p == &path));
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("use serde::{Deserialize, Serialize};"));
        assert!(body.contains("pub struct Product {"));
        assert!(body.contains("pub id: i64,"));
        assert!(body.contains("pub name: String,"));
        assert!(body.contains("pub description: Option<String>,"));
        // Defaults + Eq, no duplicate Clone.
        assert!(body.contains("#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Eq)]"));
        // mod.rs should reference the new module.
        let mod_rs = std::fs::read_to_string(dir.path().join("src/model/mod.rs")).unwrap();
        assert!(mod_rs.contains("pub mod product;"));
    }

    #[tokio::test]
    async fn execute_code_creates_model_files_and_next_steps() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "models_test"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = { version = "0.7", features = ["fullstack"] }
"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        // A minimal main.rs so the crate-root `pub mod` auto-injection has a
        // file to patch — exercising the post-#2 behavior.
        std::fs::write(
            root.join("src/main.rs"),
            r#"use dioxus::prelude::*;

fn main() {}
"#,
        )
        .unwrap();

        let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
        let result = execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
models:
  - name: Product
    fields:
      - {name: id, type: i64}
      - {name: name, type: String}
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: false,
                dry_run: false,
            },
        )
        .await
        .expect("execute_code should succeed with models");
        assert!(root.join("src/model/product.rs").exists());
        assert!(root.join("src/model/mod.rs").exists());
        assert!(
            result
                .next_steps
                .iter()
                .any(|s| s.contains("serde") && s.contains("derive")),
            "expected a serde next_step, got {:?}",
            result.next_steps
        );
        // Cargo.toml should have been auto-patched with the serde dep line.
        let cargo = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            cargo.contains(r#"serde = { version = "1", features = ["derive"] }"#),
            "expected Cargo.toml to be patched with serde dep, got:\n{cargo}"
        );
        let cargo_path = root.join("Cargo.toml");
        assert!(
            result.files_modified.contains(&cargo_path),
            "Cargo.toml should appear in files_modified after auto-patch, got {:?}",
            result.files_modified
        );
        let main_rs = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
        assert!(
            main_rs.contains("pub mod model;"),
            "expected main.rs to be patched with `pub mod model;`, got:\n{main_rs}"
        );
        let main_path = root.join("src/main.rs");
        assert!(
            result.files_modified.contains(&main_path),
            "main.rs should appear in files_modified, got {:?}",
            result.files_modified
        );
    }

    #[test]
    fn ensure_serde_no_op_when_already_correct() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "ok"
version = "0.1.0"
edition = "2024"

[dependencies]
serde = { version = "1", features = ["derive"] }
"#,
        )
        .unwrap();
        match ensure_serde_in_cargo_toml(root).unwrap() {
            SerdePatch::AlreadyOk => {}
            _ => panic!("expected AlreadyOk for serde with derive feature"),
        }
    }

    #[test]
    fn ensure_serde_reports_missing_derive_feature() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "missing_derive"
version = "0.1.0"
edition = "2024"

[dependencies]
serde = "1"
"#,
        )
        .unwrap();
        match ensure_serde_in_cargo_toml(root).unwrap() {
            SerdePatch::PresentWithoutDeriveFeature => {}
            other => panic!(
                "expected PresentWithoutDeriveFeature, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn preflight_rejects_duplicate_model_name_and_duplicate_fields() {
        let dir = tempfile::TempDir::new().unwrap();
        let doc: DslDoc = serde_yml::from_str(
            r#"version: "1"
models:
  - name: Product
    fields:
      - {name: id, type: i64}
  - name: product
    fields:
      - {name: id, type: i64}
"#,
        )
        .unwrap();
        let err = preflight(&doc, &[], dir.path(), false).unwrap_err();
        assert!(err.contains("duplicate model"), "got {err}");

        let doc: DslDoc = serde_yml::from_str(
            r#"version: "1"
models:
  - name: Product
    fields:
      - {name: id, type: i64}
      - {name: ID, type: i64}
"#,
        )
        .unwrap();
        let err = preflight(&doc, &[], dir.path(), false).unwrap_err();
        assert!(err.contains("duplicate field"), "got {err}");
    }

    #[tokio::test]
    async fn execute_code_expands_resource_into_full_slice() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "resource_test"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = { version = "0.7", features = ["fullstack"] }
"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        // Minimal Routable enum so route inserts succeed.
        std::fs::write(
            root.join("src/router.rs"),
            r#"use dioxus::prelude::*;

#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[route("/")]
    Home {},
}
"#,
        )
        .unwrap();
        // main.rs so the crate-root `pub mod` auto-injection has a file to
        // patch. Without this, only a fallback next_steps hint is emitted.
        std::fs::write(
            root.join("src/main.rs"),
            r#"use dioxus::prelude::*;

pub mod router;

fn main() {}
"#,
        )
        .unwrap();

        let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
        let result = execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
resources:
  - name: Product
    fields:
      - {name: id, type: i64}
      - {name: name, type: String}
      - {name: description, type: String, optional: true}
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: false,
                dry_run: false,
            },
        )
        .await
        .expect("execute_code should succeed");

        // Model
        assert!(root.join("src/model/product.rs").exists());
        let model_body = std::fs::read_to_string(root.join("src/model/product.rs")).unwrap();
        assert!(
            model_body.contains("Default"),
            "synthesized resource model should derive Default, got:\n{model_body}"
        );

        // Store
        let store_path = root.join("src/state/product_store.rs");
        assert!(store_path.exists(), "store file should be emitted");
        let store_body = std::fs::read_to_string(&store_path).unwrap();
        assert!(store_body.contains(r#"#![cfg(feature = "server")]"#));
        assert!(store_body.contains("pub struct ProductStore"));
        assert!(store_body.contains("fn list("));
        assert!(store_body.contains("fn get("));
        assert!(store_body.contains("fn create("));
        assert!(store_body.contains("fn update("));
        assert!(store_body.contains("fn delete("));
        assert!(store_body.contains("use crate::model::Product"));
        // Resource expansion forces emit_tests=true, so the CRUD test block
        // should land. The synthesized model derives Default, so the tests
        // compile against `Product::default()`.
        assert!(
            store_body.contains("#[cfg(test)]"),
            "expected test block in store, got:\n{store_body}"
        );
        assert!(
            store_body.contains("create_assigns_id_and_appends_to_list"),
            "expected create test, got:\n{store_body}"
        );
        assert!(
            store_body.contains("delete_removes_matching_item_and_is_idempotent"),
            "expected delete test, got:\n{store_body}"
        );
        assert!(
            store_body.contains("Product::default()"),
            "tests should construct via Default, got:\n{store_body}"
        );
        // Sanity: the rendered store must parse as valid Rust — catches
        // template typos that the unit-render tests can't see.
        syn::parse_file(&store_body).unwrap_or_else(|e| {
            panic!("generated store file should parse as Rust: {e}\n--- file ---\n{store_body}")
        });
        let state_mod = std::fs::read_to_string(root.join("src/state/mod.rs")).unwrap();
        assert!(
            state_mod.contains(r#"#[cfg(feature = "server")]"#)
                && state_mod.contains("pub mod product_store;"),
            "state/mod.rs must cfg-gate store entries (otherwise wasm build fails E0432), got:\n{state_mod}"
        );

        // 5 server fns
        for name in [
            "list_products",
            "get_product",
            "create_product",
            "update_product",
            "delete_product",
        ] {
            let p = root.join("src/server").join(format!("{name}.rs"));
            assert!(p.exists(), "missing {}", p.display());
            let body = std::fs::read_to_string(&p).unwrap();
            assert!(
                body.contains(r#"#[cfg(feature = "server")]"#)
                    && body.contains("ProductStore::global()"),
                "server fn {name} should call into store, got:\n{body}"
            );
        }

        // 2 screens, 2 route variants
        assert!(root.join("src/components/product_list_screen.rs").exists());
        assert!(root.join("src/components/product_new_screen.rs").exists());
        let router = std::fs::read_to_string(root.join("src/router.rs")).unwrap();
        assert!(
            router.contains("ProductListScreen"),
            "list screen should be in router, got:\n{router}"
        );
        assert!(
            router.contains("ProductNewScreen"),
            "new screen should be in router, got:\n{router}"
        );

        // main.rs should be auto-patched with `pub mod` declarations for
        // every emitted top-level subdir (model, state, server, components).
        let main_rs = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
        for needed in [
            "pub mod model;",
            "pub mod state;",
            "pub mod server;",
            "pub mod components;",
        ] {
            assert!(
                main_rs.contains(needed),
                "expected main.rs to contain `{needed}`, got:\n{main_rs}"
            );
        }

        // The list screen uses use_resource + match ladder bound to list_products.
        let list_body =
            std::fs::read_to_string(root.join("src/components/product_list_screen.rs")).unwrap();
        assert!(
            list_body.contains("use_resource(")
                && list_body.contains("list_products()")
                && list_body.contains("Loading..."),
            "list screen should be resource-bound, got:\n{list_body}"
        );

        // The new screen has one input per non-id field and a submit body that
        // constructs Product and navigates to /products.
        let new_body =
            std::fs::read_to_string(root.join("src/components/product_new_screen.rs")).unwrap();
        assert!(
            new_body.contains("use_signal") && new_body.contains("create_product"),
            "new screen should call create_product, got:\n{new_body}"
        );
        assert!(
            new_body.contains("nav.push(\"/products\")"),
            "new screen should redirect to /products, got:\n{new_body}"
        );

        // The new screen's `use` for the model type should be a single
        // segment — emitted as `use crate::model::Product;`, never the
        // earlier-bug duplicated `use crate::model::crate::model::Product;`.
        assert!(
            new_body.contains("use crate::model::Product;"),
            "new screen should use bare model path, got:\n{new_body}"
        );
        assert!(
            !new_body.contains("crate::model::crate::"),
            "new screen must not duplicate the crate::model:: prefix, got:\n{new_body}"
        );

        // The edit screen should also have been emitted with an id prop,
        // call get_/update_, and route under /products/:id/edit.
        let edit_path = root.join("src/components/product_edit_screen.rs");
        assert!(edit_path.exists(), "edit screen file should be emitted");
        let edit_body = std::fs::read_to_string(&edit_path).unwrap();
        assert!(
            edit_body.contains("pub fn ProductEditScreen(id: i64)"),
            "edit screen should take id prop, got:\n{edit_body}"
        );
        assert!(
            edit_body.contains("get_product(") && edit_body.contains("update_product"),
            "edit screen should fetch via get_product and submit via update_product, got:\n{edit_body}"
        );
        assert!(
            router.contains("ProductEditScreen { id: i64 }"),
            "edit route variant should carry id field, got:\n{router}"
        );
        assert!(
            router.contains("/products/:id/edit"),
            "edit route path should appear, got:\n{router}"
        );

        // Every emitted .rs file must at least parse as Rust. This catches
        // template typos that no behavioural assert covers.
        for rel in [
            "src/model/product.rs",
            "src/state/product_store.rs",
            "src/server/list_products.rs",
            "src/server/get_product.rs",
            "src/server/create_product.rs",
            "src/server/update_product.rs",
            "src/server/delete_product.rs",
            "src/components/product_list_screen.rs",
            "src/components/product_new_screen.rs",
            "src/components/product_edit_screen.rs",
        ] {
            let body = std::fs::read_to_string(root.join(rel)).unwrap();
            syn::parse_file(&body)
                .unwrap_or_else(|e| panic!("emitted {rel} does not parse: {e}\n---\n{body}"));
        }

        // files_modified should be deduplicated — without it, src/router.rs and
        // src/components/mod.rs each appear once per route/component inserted.
        let mut sorted = result.files_modified.clone();
        sorted.sort();
        let mut deduped = sorted.clone();
        deduped.dedup();
        assert_eq!(
            sorted.len(),
            deduped.len(),
            "files_modified must be deduped; saw {:?}",
            result.files_modified
        );
    }

    #[tokio::test]
    async fn resource_form_template_emits_typed_constructor_for_mixed_field_types() {
        // Mix String / Option<String> / i64 / Option<i64> / f64 / bool so the
        // new screen exercises every branch of the form-typing fix.
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            cargo_toml_with_fullstack("res_typing_test"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/router.rs"),
            r#"use dioxus::prelude::*;

#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[route("/")]
    Home {},
}
"#,
        )
        .unwrap();

        let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
        execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
resources:
  - name: Product
    fields:
      - {name: id, type: i64}
      - {name: name, type: String}
      - {name: description, type: String, optional: true}
      - {name: quantity, type: i64}
      - {name: reorder_at, type: i64, optional: true}
      - {name: price, type: f64}
      - {name: active, type: bool}
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: false,
                dry_run: false,
            },
        )
        .await
        .expect("execute_code should succeed");

        let new_body =
            std::fs::read_to_string(root.join("src/components/product_new_screen.rs")).unwrap();

        // Signal initializers must be String::new() for text-backed inputs and
        // `false` for the bool. Crucially: NO `0i64` or `0.0f64` literals —
        // numeric fields are String-backed and parsed at submit.
        assert!(
            new_body.contains("let mut name = use_signal(|| String::new())"),
            "name should be a String-backed signal, got:\n{new_body}"
        );
        assert!(
            new_body.contains("let mut description = use_signal(|| String::new())"),
            "description (Option<String>) should still be String-backed, got:\n{new_body}"
        );
        assert!(
            new_body.contains("let mut quantity = use_signal(|| String::new())"),
            "i64 signal should be String-backed, got:\n{new_body}"
        );
        assert!(
            new_body.contains("let mut price = use_signal(|| String::new())"),
            "f64 signal should be String-backed, got:\n{new_body}"
        );
        assert!(
            new_body.contains("let mut active = use_signal(|| false)"),
            "bool signal should be initialized to false, got:\n{new_body}"
        );
        assert!(
            !new_body.contains("0i64") && !new_body.contains("0.0f64"),
            "numeric signals must not be initialized with a typed literal, got:\n{new_body}"
        );

        // Submit-side constructor must wrap Option fields and parse numerics.
        assert!(
            new_body.contains("name: name_v,"),
            "String field assigns raw signal value, got:\n{new_body}"
        );
        assert!(
            new_body.contains("if description_v.is_empty() { None } else { Some(description_v) }"),
            "Option<String> must wrap with Some and treat empty as None, got:\n{new_body}"
        );
        assert!(
            new_body.contains("quantity_v.parse::<i64>().unwrap_or_default()"),
            "i64 field must be parsed from String, got:\n{new_body}"
        );
        assert!(
            new_body.contains("price_v.parse::<f64>().unwrap_or_default()"),
            "f64 field must be parsed from String, got:\n{new_body}"
        );
        assert!(
            new_body.contains(
                "if reorder_at_v.is_empty() { None } else { reorder_at_v.parse::<i64>().ok() }"
            ),
            "Option<i64> must parse-or-none on empty, got:\n{new_body}"
        );
        assert!(
            new_body.contains("active: active_v,"),
            "bool field reads signal directly, got:\n{new_body}"
        );

        // No duplicated crate::model:: prefix.
        assert!(
            !new_body.contains("crate::model::crate::"),
            "new screen must not duplicate the crate::model:: prefix, got:\n{new_body}"
        );

        // All synthesized screens must still parse as valid Rust.
        for rel in [
            "src/components/product_list_screen.rs",
            "src/components/product_new_screen.rs",
            "src/components/product_edit_screen.rs",
        ] {
            let body = std::fs::read_to_string(root.join(rel)).unwrap();
            syn::parse_file(&body)
                .unwrap_or_else(|e| panic!("emitted {rel} does not parse: {e}\n---\n{body}"));
        }

        // The list screen should be a real table with column headers, an
        // edit link, and a delete button — not the placeholder `li{item:?}`.
        let list_body =
            std::fs::read_to_string(root.join("src/components/product_list_screen.rs")).unwrap();
        assert!(
            list_body.contains("table {")
                && list_body.contains("thead {")
                && list_body.contains("tbody {"),
            "list screen should emit a real table, got:\n{list_body}"
        );
        assert!(
            list_body.contains("key: \"{row.id}\""),
            "rows should be keyed by id, got:\n{list_body}"
        );
        assert!(
            list_body.contains("delete_product("),
            "delete button should call delete_product, got:\n{list_body}"
        );
        // List uses typed Link to the route enum for SPA navigation rather than
        // `<a href>` (which would force a full page reload).
        assert!(
            list_body.contains("Link { to: Route::ProductEditScreen { id: row.id.clone() }"),
            "edit link should be a typed Link to the EditScreen route variant, got:\n{list_body}"
        );
        assert!(
            list_body.contains("Link { to: Route::ProductNewScreen {}"),
            "new link should be a typed Link to the NewScreen route variant, got:\n{list_body}"
        );
        assert!(
            list_body.contains("use crate::router::Route;"),
            "list screen should import the Route enum, got:\n{list_body}"
        );
        assert!(
            !list_body.contains("a { href: \"/products/new\""),
            "list should not retain the old `a {{ href: }}` form, got:\n{list_body}"
        );
        assert!(
            list_body.contains("*version.write() += 1"),
            "delete should bump a version signal to refetch, got:\n{list_body}"
        );
        // No `li { \"{item:?}\" }` placeholder.
        assert!(
            !list_body.contains("li { \"{item:?}\" }"),
            "list should not retain the placeholder li body, got:\n{list_body}"
        );
        // Option<T> columns must render the inner value, not Debug-format the
        // Option wrapper (which would produce literal "Some(...)" / "None" in
        // the cell).
        assert!(
            list_body
                .contains("row.description.as_ref().map(|v| v.to_string()).unwrap_or_default()"),
            "Option<String> column should be unwrapped, not Debug-formatted, got:\n{list_body}"
        );
        assert!(
            list_body
                .contains("row.reorder_at.as_ref().map(|v| v.to_string()).unwrap_or_default()"),
            "Option<i64> column should be unwrapped, not Debug-formatted, got:\n{list_body}"
        );
        assert!(
            !list_body.contains("{row.description:?}") && !list_body.contains("{row.reorder_at:?}"),
            "no Option column should be Debug-formatted, got:\n{list_body}"
        );

        // Form labels in the new/edit screens should be human-readable
        // (matching the list-screen <th> style), not raw PascalCase identifiers.
        let new_body =
            std::fs::read_to_string(root.join("src/components/product_new_screen.rs")).unwrap();
        assert!(
            new_body.contains("label { \"Reorder at\" }"),
            "form label should be de-PascalCased, got:\n{new_body}"
        );
        assert!(
            !new_body.contains("label { \"ReorderAt\" }"),
            "form label should not be PascalCase, got:\n{new_body}"
        );
        let edit_body =
            std::fs::read_to_string(root.join("src/components/product_edit_screen.rs")).unwrap();
        assert!(
            edit_body.contains("label { \"Reorder at\" }"),
            "edit form label should be de-PascalCased, got:\n{edit_body}"
        );

        // The edit screen should pre-populate signals from the loaded item,
        // preserve the original id, and call update_product.
        let edit_body =
            std::fs::read_to_string(root.join("src/components/product_edit_screen.rs")).unwrap();
        assert!(
            edit_body.contains("let mut name = use_signal(|| item.name.clone())"),
            "edit form should init name from item, got:\n{edit_body}"
        );
        assert!(
            edit_body.contains(
                "let mut description = use_signal(|| item.description.clone().unwrap_or_default())"
            ),
            "edit form should unwrap Option<String> from item, got:\n{edit_body}"
        );
        assert!(
            edit_body.contains("let mut quantity = use_signal(|| item.quantity.to_string())"),
            "edit form should convert numeric to String, got:\n{edit_body}"
        );
        assert!(
            edit_body.contains("id: id_v,")
                && edit_body.contains("let id_v = original_id.clone();"),
            "edit submit body should preserve the original id, got:\n{edit_body}"
        );
        assert!(
            edit_body.contains("update_product(item)"),
            "edit submit should call update_product, got:\n{edit_body}"
        );
    }

    #[tokio::test]
    async fn resource_dry_run_classifies_all_synth_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "resource_dry"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = { version = "0.7", features = ["fullstack"] }
"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();

        let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
        let result = execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
resources:
  - name: Order
    fields:
      - {name: id, type: i64}
      - {name: total, type: f64}
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: false,
                dry_run: true,
            },
        )
        .await
        .expect("dry_run should succeed");
        assert!(result.dry_run);
        let paths: Vec<String> = result
            .would_create
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert!(paths.iter().any(|p| p.ends_with("order_store.rs")));
        assert!(paths.iter().any(|p| p.ends_with("list_orders.rs")));
        assert!(paths.iter().any(|p| p.ends_with("get_order.rs")));
        assert!(paths.iter().any(|p| p.ends_with("create_order.rs")));
        assert!(paths.iter().any(|p| p.ends_with("order_list_screen.rs")));
        assert!(paths.iter().any(|p| p.ends_with("order_new_screen.rs")));
        assert!(
            paths.iter().any(|p| p.ends_with("order_edit_screen.rs")),
            "dry_run should classify the synthesized edit screen, got {paths:?}"
        );
        assert!(
            !root.join("src/state/order_store.rs").exists(),
            "dry_run must not write"
        );
    }

    #[tokio::test]
    async fn resource_plural_override_drives_route_and_server_fn_names() {
        // `Person → people` is the canonical irregular case; the built-in
        // pluralizer would emit `persons`, so this exercises the `plural`
        // override end-to-end (route slug + list_{plural} fn name).
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            cargo_toml_with_fullstack("plural_test"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/router.rs"),
            r#"use dioxus::prelude::*;

#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[route("/")]
    Home {},
}
"#,
        )
        .unwrap();

        let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
        execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
resources:
  - name: Person
    plural: people
    fields:
      - {name: id, type: i64}
      - {name: name, type: String}
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: false,
                dry_run: false,
            },
        )
        .await
        .expect("execute_code should succeed with plural override");

        // Route slug uses the override.
        let router = std::fs::read_to_string(root.join("src/router.rs")).unwrap();
        assert!(
            router.contains("/people") && !router.contains("/persons"),
            "default route slug should follow the `plural:` override, got router:\n{router}"
        );
        // list_{plural} server fn uses the override.
        assert!(
            root.join("src/server/list_people.rs").exists(),
            "list server fn should be named after the plural override"
        );
        assert!(
            !root.join("src/server/list_persons.rs").exists(),
            "auto-pluralized list_persons.rs must not be emitted when override is set"
        );
    }

    #[tokio::test]
    async fn resource_default_route_base_is_kebab_case() {
        // A `StockMovement` resource without an explicit `route_base` should
        // default to the kebab-case slug `/stock-movements`, not the
        // snake_case `/stock_movements` web convention violator.
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            cargo_toml_with_fullstack("kebab_route_test"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/router.rs"),
            r#"use dioxus::prelude::*;

#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[route("/")]
    Home {},
}
"#,
        )
        .unwrap();

        let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
        execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
resources:
  - name: StockMovement
    fields:
      - {name: id, type: i64}
      - {name: note, type: String}
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: false,
                dry_run: false,
            },
        )
        .await
        .expect("execute_code should succeed");

        let router = std::fs::read_to_string(root.join("src/router.rs")).unwrap();
        assert!(
            router.contains("/stock-movements") && !router.contains("/stock_movements"),
            "default route slug should be kebab-case, got router:\n{router}"
        );
    }

    #[test]
    fn pluralize_handles_common_cases() {
        assert_eq!(pluralize("product"), "products");
        assert_eq!(pluralize("order"), "orders");
        assert_eq!(pluralize("box"), "boxes");
        assert_eq!(pluralize("category"), "categories");
        assert_eq!(pluralize("toy"), "toys");
        assert_eq!(pluralize("bus"), "buses");
    }

    #[test]
    fn preflight_rejects_store_referencing_unknown_model() {
        let dir = tempfile::TempDir::new().unwrap();
        let doc: DslDoc = serde_yml::from_str(
            r#"version: "1"
stores:
  - name: WidgetStore
    resource: Widget
"#,
        )
        .unwrap();
        let err = preflight(&doc, &[], dir.path(), false).unwrap_err();
        assert!(err.contains("unknown model"), "got {err}");
    }

    #[tokio::test]
    async fn client_store_emits_signal_backed_store_without_server_gate() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            cargo_toml_with_fullstack("client_store_test"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();

        let state = std::sync::Arc::new(State::new(root.to_path_buf()).unwrap());
        let _ = execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
models:
  - name: Todo
    derives: [Default]
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
      - {name: done, type: bool}
client_stores:
  - name: TodoStore
    item_type: Todo
    id_field: id
    id_type: i64
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: false,
                dry_run: false,
            },
        )
        .await
        .expect("client_store should scaffold");

        let store_path = root.join("src/state/todo_store.rs");
        assert!(
            store_path.exists(),
            "expected todo_store.rs at {store_path:?}"
        );
        let body = std::fs::read_to_string(&store_path).unwrap();
        assert!(
            !body.contains("#![cfg(feature = \"server\")]"),
            "ClientStore must NOT carry the server cfg gate, got:\n{body}"
        );
        assert!(
            body.contains("use crate::model::Todo;"),
            "missing model import: {body}"
        );
        assert!(
            body.contains("pub fn provide_todo_store()"),
            "missing provide_ fn: {body}"
        );
        assert!(
            body.contains("pub fn use_todo_store()"),
            "missing use_ fn: {body}"
        );
        assert!(body.contains("pub fn push("), "missing push helper: {body}");
        assert!(
            body.contains("pub fn remove_by_id("),
            "missing remove_by_id helper: {body}"
        );
        assert!(
            body.contains("pub fn update_by_id("),
            "missing update_by_id helper: {body}"
        );
        // Regression: `remove_by_id` must bind the post-write length to a local
        // before returning. The naive `items.read().len() < before` form leaves a
        // GenerationalRef alive past the Signal it borrows from and fails E0597
        // on `cargo check`. Keep this assertion until we have a fixture project
        // that runs a real `cargo check` in CI.
        assert!(
            body.contains("let after = items.read().len();"),
            "remove_by_id must bind post-write length to a local (E0597 regression), got:\n{body}"
        );
        assert!(
            !body.contains("items.read().len() < before"),
            "remove_by_id is using the inline-borrow form that fails borrow-check (E0597), got:\n{body}"
        );
        // Syntactic sanity-check on the whole emitted file.
        syn::parse_file(&body)
            .unwrap_or_else(|e| panic!("generated client_store does not parse: {e}\n---\n{body}"));

        // mod.rs should NOT have a server cfg gate for the client store entry.
        let mod_rs = std::fs::read_to_string(root.join("src/state/mod.rs")).unwrap();
        let todo_lines: Vec<&str> = mod_rs
            .lines()
            .filter(|l| l.contains("todo_store"))
            .collect();
        assert!(
            !todo_lines.iter().any(|l| l.contains("cfg(feature")),
            "ClientStore entries must not be gated in mod.rs, got: {mod_rs}"
        );
    }

    #[tokio::test]
    async fn client_crud_screen_wires_add_input_and_delete_button() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            cargo_toml_with_fullstack("client_crud_screen_test"),
        )
        .unwrap();
        // Pre-create a Routable enum so the screen route insert succeeds.
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            r#"use dioxus::prelude::*;

#[derive(Routable, Clone, PartialEq)]
enum Route {
    #[route("/old")]
    Old {},
}

fn main() {}
"#,
        )
        .unwrap();

        let state = std::sync::Arc::new(State::new(root.to_path_buf()).unwrap());
        let r = execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
models:
  - name: Todo
    derives: [Default]
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
      - {name: done, type: bool}
client_stores:
  - name: TodoStore
    item_type: Todo
    id_field: id
    id_type: i64
screens:
  - name: TodoScreen
    route: /
    template:
      kind: client_crud
      store: TodoStore
      item_type: Todo
      label_field: title
      checkbox_field: done
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: false,
                dry_run: false,
            },
        )
        .await
        .expect("client_crud screen should scaffold");

        let screen = root.join("src/components/todo_screen.rs");
        let body = std::fs::read_to_string(&screen).unwrap();
        assert!(
            body.contains("use crate::state::todo_store::use_todo_store;"),
            "missing client_store import:\n{body}"
        );
        assert!(
            body.contains("use crate::model::Todo;"),
            "missing model import:\n{body}"
        );
        assert!(
            body.contains("store.push(Todo {"),
            "missing push call:\n{body}"
        );
        assert!(
            body.contains("title: value,"),
            "missing label_field assignment:\n{body}"
        );
        assert!(
            body.contains("store.remove_by_id(id);"),
            "missing delete handler:\n{body}"
        );
        assert!(
            body.contains("store.update_by_id(id, |t| t.done = !t.done);"),
            "missing checkbox toggle:\n{body}"
        );
        // Sanity: it must compile structurally — input/onsubmit/button all
        // emitted under the rsx! block.
        assert!(body.contains("rsx!"), "missing rsx block:\n{body}");
        assert!(
            body.contains("button { r#type: \"submit\""),
            "missing add button:\n{body}"
        );

        // route variant inserted in main.rs
        let routes = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
        assert!(
            routes.contains("TodoScreen"),
            "TodoScreen variant not added: {routes}"
        );

        // ensure no server feature gate snuck into the screen
        assert!(
            !body.contains("cfg(feature = \"server\")"),
            "client_crud screen must not carry server cfg:\n{body}"
        );
        // The store file under src/state must also be unguarded.
        let cs = std::fs::read_to_string(root.join("src/state/todo_store.rs")).unwrap();
        assert!(
            !cs.contains("#![cfg(feature = \"server\")]"),
            "todo store should be client-side:\n{cs}"
        );

        // next_steps should mention provide_*
        assert!(
            r.next_steps
                .iter()
                .any(|s| s.contains("provide_todo_store")),
            "expected next_steps to mention provide_todo_store, got {:?}",
            r.next_steps
        );
    }

    /// TODO5 §4: a fresh `dx new` project has no `#[derive(Routable)]` enum.
    /// `execute_code` must bootstrap one in preflight so the call doesn't fail
    /// halfway through with "could not find a Routable enum" after already
    /// writing the model/store/component files.
    #[tokio::test]
    async fn bootstrap_router_creates_router_file_on_fresh_dx_new_project() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            cargo_toml_with_fullstack("bootstrap_router_test"),
        )
        .unwrap();
        // Simulate what `dx new` gives you: a plain main.rs with no Route.
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            "use dioxus::prelude::*;\n\nfn main() {}\n",
        )
        .unwrap();

        let state = std::sync::Arc::new(State::new(root.to_path_buf()).unwrap());
        let r = execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
models:
  - name: Todo
    derives: [Default]
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
      - {name: done, type: bool}
client_stores:
  - name: TodoStore
    item_type: Todo
    id_field: id
    id_type: i64
screens:
  - name: TodoScreen
    route: /
    template:
      kind: client_crud
      store: TodoStore
      item_type: Todo
      label_field: title
      checkbox_field: done
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: false,
                dry_run: false,
            },
        )
        .await
        .expect(
            "a Model + ClientStore + Screen doc must run cleanly against a fresh `dx new` project",
        );

        let router = root.join("src/router.rs");
        assert!(router.exists(), "auto-bootstrap must create src/router.rs");
        let body = std::fs::read_to_string(&router).unwrap();
        assert!(
            body.contains("#[derive(Routable, Clone, PartialEq)]"),
            "bootstrapped router must derive Routable, got:\n{body}"
        );
        assert!(
            body.contains("pub enum Route {"),
            "bootstrapped router must declare `pub enum Route`, got:\n{body}"
        );
        assert!(
            body.contains("#[route(\"/\")]") && body.contains("TodoScreen {},"),
            "bootstrapped router must seed the declared screen variant, got:\n{body}"
        );
        // pub mod router; must be auto-declared so `crate::router::Route`
        // resolves from main.rs.
        let main_rs = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
        assert!(
            main_rs.contains("pub mod router;"),
            "auto-bootstrap must add `pub mod router;` to main.rs, got:\n{main_rs}"
        );
        // No re-emit of the screen body should clobber the router.
        assert!(
            body.matches("TodoScreen {},").count() == 1,
            "screen route insert must dedupe against the seeded variant, got:\n{body}"
        );
        // Status should reflect a clean apply.
        assert_eq!(
            r.status.as_deref(),
            Some("applied"),
            "fresh-project run should report status: applied"
        );
        // The next_steps should call out router wiring so the human knows what's left.
        assert!(
            r.next_steps
                .iter()
                .any(|s| s.contains("Router::<crate::router::Route>")),
            "expected a Router mounting next_step, got {:?}",
            r.next_steps
        );
    }

    /// TODO5 §5: a re-run after every primitive already lands on disk used to
    /// return `next_steps: []` and no status field, which looked like success
    /// when the route variant might never have been inserted. The status field
    /// and the idempotent route insert together fix that.
    #[tokio::test]
    async fn rerun_with_if_missing_reports_no_changes_and_finishes_route_insert() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            cargo_toml_with_fullstack("rerun_no_changes_test"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            "use dioxus::prelude::*;\n\nfn main() {}\n",
        )
        .unwrap();

        let state = std::sync::Arc::new(State::new(root.to_path_buf()).unwrap());
        let yaml = r#"version: "1"
models:
  - name: Todo
    derives: [Default]
    fields:
      - {name: id, type: i64}
      - {name: title, type: String}
client_stores:
  - name: TodoStore
    item_type: Todo
    id_field: id
    id_type: i64
screens:
  - name: TodoScreen
    route: /
    template:
      kind: client_crud
      store: TodoStore
      item_type: Todo
      label_field: title
"#;
        // Initial run lays the app down.
        let first = execute_code(
            &state,
            ExecuteCodeParams {
                code: yaml.into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: false,
                dry_run: false,
            },
        )
        .await
        .expect("initial run should succeed");
        assert_eq!(first.status.as_deref(), Some("applied"));

        // Re-run with if_missing: every primitive's leaf file is already on
        // disk, so the only legitimate response is `status: no_changes`.
        let second = execute_code(
            &state,
            ExecuteCodeParams {
                code: yaml.into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: true,
                dry_run: false,
            },
        )
        .await
        .expect("re-run should not error in if_missing mode");
        assert_eq!(
            second.status.as_deref(),
            Some("no_changes"),
            "fully-collided re-run must report no_changes, got status={:?} created={:?} modified={:?}",
            second.status,
            second.files_created,
            second.files_modified,
        );
        assert!(
            !second.collisions.is_empty(),
            "fully-collided re-run must populate collisions"
        );
    }

    #[tokio::test]
    async fn next_steps_surface_todo_markers_with_file_and_line() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            cargo_toml_with_fullstack("todo_marker_test"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();

        // A bare Form (no `on_submit`) emits `// TODO submit handler` in the
        // generated body, which the scanner should pick up.
        let state = std::sync::Arc::new(State::new(root.to_path_buf()).unwrap());
        let r = execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
forms:
  - name: ContactForm
    fields:
      - {name: email, type: email}
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: false,
                dry_run: false,
            },
        )
        .await
        .expect("Form with no on_submit should scaffold");

        let hotspot = r
            .next_steps
            .iter()
            .find(|s| s.contains("contact_form.rs:") && s.contains("TODO"));
        assert!(
            hotspot.is_some(),
            "expected a `path:line — TODO ...` next_steps entry, got {:?}",
            r.next_steps
        );
        // The header entry should also be present.
        assert!(
            r.next_steps.iter().any(|s| s.contains("hand-edit hotspot")),
            "expected a hotspot header, got {:?}",
            r.next_steps
        );
    }

    #[test]
    fn preflight_rejects_client_crud_screen_with_unknown_store() {
        let dir = tempfile::TempDir::new().unwrap();
        let doc: DslDoc = serde_yml::from_str(
            r#"version: "1"
screens:
  - name: TodoScreen
    route: /
    template:
      kind: client_crud
      store: NopeStore
      item_type: Todo
      label_field: title
"#,
        )
        .unwrap();
        let err = preflight(&doc, &[], dir.path(), false).unwrap_err();
        assert!(err.contains("unknown client_store"), "got: {err}");
    }

    fn cargo_toml_with_fullstack(name: &str) -> String {
        format!(
            r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = {{ version = "0.7", features = ["fullstack"] }}
"#
        )
    }

    #[tokio::test]
    async fn modify_add_model_field_appends_and_is_idempotent() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            cargo_toml_with_fullstack("modify_model_test"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();

        let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());

        // Create the model first.
        execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
models:
  - name: Product
    fields:
      - {name: id, type: i64}
      - {name: name, type: String}
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: false,
                dry_run: false,
            },
        )
        .await
        .unwrap();
        let path = root.join("src/model/product.rs");
        let before = std::fs::read_to_string(&path).unwrap();
        assert!(!before.contains("pub sku:"));

        // Modify: add sku.
        let result = execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
modify:
  - kind: add_model_field
    model: Product
    fields:
      - {name: sku, type: String}
      - {name: weight, type: f32, optional: true}
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: false,
                dry_run: false,
            },
        )
        .await
        .expect("modify should succeed");
        assert!(result.files_modified.iter().any(|p| p == &path));
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("pub sku: String,"), "got:\n{body}");
        assert!(body.contains("pub weight: Option<f32>,"), "got:\n{body}");
        // Existing fields still present.
        assert!(body.contains("pub id: i64,"));
        assert!(body.contains("pub name: String,"));
        // Resulting file must still parse.
        syn::parse_file(&body).expect("modified model should parse");

        // Re-run identical modify: idempotent — no files_modified, no duplicate.
        let result = execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
modify:
  - kind: add_model_field
    model: Product
    fields:
      - {name: sku, type: String}
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: false,
                dry_run: false,
            },
        )
        .await
        .expect("idempotent re-run should succeed");
        assert!(
            result.files_modified.is_empty(),
            "re-run should be a no-op, got {:?}",
            result.files_modified
        );
        let after = std::fs::read_to_string(&path).unwrap();
        // Only one sku declaration.
        assert_eq!(after.matches("pub sku:").count(), 1);
    }

    #[tokio::test]
    async fn modify_add_component_prop_appends_with_optional() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            cargo_toml_with_fullstack("modify_comp_test"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();

        let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
        execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
components:
  - name: UserCard
    props:
      - {name: id, type: i32}
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: false,
                dry_run: false,
            },
        )
        .await
        .unwrap();
        let path = root.join("src/components/user_card.rs");

        let _ = execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
modify:
  - kind: add_component_prop
    component: UserCard
    props:
      - {name: avatar_url, type: String, optional: true}
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: false,
                dry_run: false,
            },
        )
        .await
        .expect("modify should succeed");
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            body.contains("#[props(default)]") && body.contains("pub avatar_url: Option<String>,"),
            "got:\n{body}"
        );
        syn::parse_file(&body).expect("modified component should parse");
    }

    #[tokio::test]
    async fn modify_add_component_prop_errors_when_no_props_struct() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            cargo_toml_with_fullstack("modify_no_props_test"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();

        let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
        execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
components:
  - name: Bare
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: false,
                dry_run: false,
            },
        )
        .await
        .unwrap();

        let err = execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
modify:
  - kind: add_component_prop
    component: Bare
    props:
      - {name: id, type: i32}
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: false,
                dry_run: false,
            },
        )
        .await
        .expect_err("should error when no Props struct exists");
        assert!(
            err.contains("convert the component to take props first"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn modify_add_server_fn_arg_appends_to_zero_arg_fn() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            cargo_toml_with_fullstack("modify_sfn_test"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();

        let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
        execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
server_fns:
  - name: fetch_users
    return_type: "Vec<String>"
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: false,
                dry_run: false,
            },
        )
        .await
        .unwrap();
        let path = root.join("src/server/fetch_users.rs");
        let before = std::fs::read_to_string(&path).unwrap();
        assert!(!before.contains("page"));

        let _ = execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
modify:
  - kind: add_server_fn_arg
    server_fn: fetch_users
    args:
      - {name: page, type: u32}
      - {name: page_size, type: u32}
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: false,
                dry_run: false,
            },
        )
        .await
        .expect("modify should succeed");
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("page: u32,"), "got:\n{body}");
        assert!(body.contains("page_size: u32,"), "got:\n{body}");
        syn::parse_file(&body).expect("modified server_fn should parse");
    }

    #[tokio::test]
    async fn modify_errors_when_target_missing_and_skips_under_if_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            cargo_toml_with_fullstack("modify_missing_test"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();

        let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());

        let err = execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
modify:
  - kind: add_model_field
    model: Ghost
    fields:
      - {name: x, type: i32}
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: false,
                dry_run: false,
            },
        )
        .await
        .expect_err("should error when target missing");
        assert!(err.contains("does not exist on disk"), "got: {err}");

        let result = execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
modify:
  - kind: add_model_field
    model: Ghost
    fields:
      - {name: x, type: i32}
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: true,
                dry_run: false,
            },
        )
        .await
        .expect("if_missing=true should swallow missing target");
        assert!(
            result.collisions.iter().any(|p| p.ends_with("ghost.rs")),
            "expected ghost.rs in collisions, got {:?}",
            result.collisions
        );
    }

    #[tokio::test]
    async fn modify_dry_run_classifies_targets() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            cargo_toml_with_fullstack("modify_dry_test"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src/model")).unwrap();
        std::fs::write(
            root.join("src/model/product.rs"),
            "pub struct Product { pub id: i64, }\n",
        )
        .unwrap();

        let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
        let result = execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
modify:
  - kind: add_model_field
    model: Product
    fields:
      - {name: sku, type: String}
  - kind: add_model_field
    model: Ghost
    fields:
      - {name: x, type: i32}
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: false,
                dry_run: true,
            },
        )
        .await
        .expect("dry_run should succeed even with missing target");
        assert!(result.dry_run);
        assert!(
            result
                .would_modify
                .iter()
                .any(|p| p.ends_with("product.rs")),
            "expected product.rs in would_modify, got {:?}",
            result.would_modify
        );
        assert!(
            result.collisions.iter().any(|p| p.ends_with("ghost.rs")),
            "expected ghost.rs in collisions, got {:?}",
            result.collisions
        );
        // Source file must be untouched.
        let body = std::fs::read_to_string(root.join("src/model/product.rs")).unwrap();
        assert!(!body.contains("sku"));
    }

    #[test]
    fn preflight_rejects_empty_or_duplicate_modify_entry() {
        let dir = tempfile::TempDir::new().unwrap();
        let doc: DslDoc = serde_yml::from_str(
            r#"version: "1"
modify:
  - kind: add_model_field
    model: Product
    fields: []
"#,
        )
        .unwrap();
        let err = preflight(&doc, &[], dir.path(), false).unwrap_err();
        assert!(err.contains("is empty"), "got {err}");

        let doc: DslDoc = serde_yml::from_str(
            r#"version: "1"
modify:
  - kind: add_server_fn_arg
    server_fn: fetch
    args:
      - {name: page, type: u32}
      - {name: page, type: u64}
"#,
        )
        .unwrap();
        let err = preflight(&doc, &[], dir.path(), false).unwrap_err();
        assert!(err.contains("duplicate name"), "got {err}");
    }

    #[tokio::test]
    async fn dry_run_classifies_model_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "models_dry"
version = "0.1.0"
edition = "2024"

[dependencies]
dioxus = { version = "0.7", features = ["fullstack"] }
"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();

        let state = std::sync::Arc::new(crate::state::State::new(root.to_path_buf()).unwrap());
        let result = execute_code(
            &state,
            ExecuteCodeParams {
                code: r#"version: "1"
models:
  - name: Product
    fields:
      - {name: id, type: i64}
"#
                .into(),
                project_root: Some(root.to_string_lossy().into_owned()),
                if_missing: false,
                dry_run: true,
            },
        )
        .await
        .expect("dry_run should succeed");
        assert!(result.dry_run);
        assert!(
            result
                .would_create
                .iter()
                .any(|p| p.ends_with("product.rs")),
            "expected product.rs in would_create, got {:?}",
            result.would_create
        );
        assert!(
            !root.join("src/model/product.rs").exists(),
            "dry_run must not write the file"
        );
    }
}
