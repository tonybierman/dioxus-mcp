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
#   - By default execute_code REFUSES to overwrite an existing leaf file under
#     src/components — the call errors with the conflicting path. Pass
#     `if_missing: true` to silently skip already-present primitives instead;
#     the response lists each skipped path under `collisions`.
#   - Models, server fns, signals, sockets, session states still error on the
#     inner `<target> already exists` check when their target file exists and
#     `if_missing` is false.
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
    description: "A top-level routed view. Generates a component file and inserts a route variant in src/router.rs. The `wrap_with` field lets a guard like ProtectedRoute sit at the route layer. The `template` field selects the emitted body — omit it for an empty placeholder; kind=resource_list emits a use_resource + loading/error/data ladder bound to the named endpoint; kind=resource_form emits a controlled form that calls on_submit (or endpoint) and navigates to redirect_to."
    fields:
      - {name: name, type: string, required: true}
      - {name: route, type: string, required: true}
      - {name: wrap_with, type: "ComponentName (e.g. a ProtectedRoute guard)", required: false}
      - {name: template, type: "ScreenTemplate {kind, endpoint?, item_type?, on_submit?, redirect_to?, fields?}", required: false}
    template_kinds: [empty, resource_list, resource_form]
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

const CORE_RESOURCE: &str = r#"  Resource:
    description: "A meta-primitive that fans out into a Model + Store + 5 server fns (list/get/create/update/delete) + 2 screens (list at `{route_base}` and new at `{route_base}/new`). One entry yields a full CRUD slice. The model must declare an integer id field; defaults to id with type i64. URL params (e.g. an edit-by-id route) are not yet emitted — wire that manually."
    fields:
      - {name: name, type: "PascalCase resource name (Product, Order, …)", required: true}
      - {name: fields, type: "ModelField[] — must contain the id field", required: true}
      - {name: id_field, type: "string (default \"id\")", required: false}
      - {name: route_base, type: "string (default \"/{plural-snake}\")", required: false}
      - {name: derives, type: "string[] forwarded to the synthesized Model", required: false}
    example:
      resources:
        - name: Product
          fields:
            - {name: id, type: i64}
            - {name: name, type: String}
            - {name: description, type: String, optional: true}
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
                h1 { "{{ pascal }}" }
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
                        value: "{{ '{' }}{{ f.name }}(){{ '}' }}",
                        oninput: move |e| {{ f.name }}.set(e.value()),
                    }
{%- endfor %}
                    button { r#type: "submit", "Submit" }
                }
            }
        }
{%- else %}
        div { class: "screen {{ snake }}",
            h1 { "{{ pascal }}" }
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
                    value: "{{ '{' }}{{ f.name }}(){{ '}' }}",
                    oninput: move |e| {{ f.name }}.set(e.value()),
                }
{%- endfor %}
                button { r#type: "submit", "Submit" }
            }
        }
{%- endif %}
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

    // Order matters: models first (so server fn return types and stores can
    // resolve them), then server fns (fail-fast on fullstack gating), then
    // leaf primitives, then screens (which call create_route serially).
    let mut models_emitted = false;
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
        models_emitted = true;
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

    let mut store_emitted = false;
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
        store_emitted = true;
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
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/components", &ls.name),
        ) {
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
        if skip_or_record(
            &skip,
            &mut result,
            leaf_for(&crate_root, "src/components", &sc.name),
        ) {
            continue;
        }
        let r = generate_screen(state, &crate_root, sc, p.project_root.as_deref()).await?;
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

    if models_emitted {
        result.next_steps.push(
            "ensure `serde = { version = \"1\", features = [\"derive\"] }` is in your Cargo.toml for the generated model(s)".into(),
        );
        result.next_steps.push(
            "declare `pub mod model;` in your crate root (src/main.rs or src/lib.rs) so the generated types are reachable as `crate::model::*`".into(),
        );
    }

    if store_emitted {
        result.next_steps.push(
            "declare `pub mod state;` in your crate root so server fns can resolve `crate::state::*` (the store files are `#![cfg(feature = \"server\")]` and compile to nothing on the wasm side)".into(),
        );
    }

    Ok(result)
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
    }

    Ok(())
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
    let dir = crate_root.join(subdir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let target = dir.join(format!("{snake}.rs"));
    if target.exists() {
        return Err(format!("{} already exists", target.display()));
    }
    std::fs::write(&target, body).map_err(|e| e.to_string())?;
    let mod_rs = dir.join("mod.rs");
    let upsert = upsert_mod_entry(&mod_rs, snake)?;
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
                label => fd.name.to_pascal_case(),
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
        Some(t) => render_screen_template(&pascal, &snake, wrap_pascal.as_deref(), t)?,
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
        },
    )
    .await?;
    merge(&mut r, route);
    Ok(r)
}

fn render_screen_template(
    pascal: &str,
    snake: &str,
    wrap_pascal: Option<&str>,
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
                    context! {
                        name => fd.name.to_snake_case(),
                        label => fd.name.to_pascal_case(),
                        input_type => input_type,
                        tag => tag,
                        initial => initial,
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
        other => Err(format!(
            "unknown screen template kind {other:?} (expected: empty, resource_list, resource_form)"
        )),
    }
}

/// Build the rust body that runs inside the form's onsubmit handler.
/// When `item_type` is set we attempt to construct it from the field signals
/// and call the submit fn with it. Otherwise we emit a TODO body.
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
        // Best-effort field assignment. Caller must include all required
        // fields in `fields:` or hand-edit the produced file.
        for f in &t.fields {
            let n = f.name.to_snake_case();
            let val = if matches!(f.ty.as_str(), "number") {
                format!("{n}_v.parse().unwrap_or_default()")
            } else {
                format!("{n}_v")
            };
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
    r.next_steps.push(format!(
        "declare `pub mod state;` in your crate root so server fns can reach `crate::state::{store_snake}::{store_pascal}`"
    ));
    if emit_tests {
        r.next_steps.push(format!(
            "run `cargo test --features server -p <crate>` to execute the generated CRUD tests for {store_pascal}"
        ));
    }
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
        let plural = pluralize(&res_snake);
        let route_base = r.route_base.clone().unwrap_or_else(|| format!("/{plural}"));
        let store_pascal = format!("{res_pascal}Store");
        let store_snake = format!("{res_snake}_store");

        // 1. Model — synthesize unless already declared.
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
            name: get_name,
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
            name: update_name,
            args: vec![("item".into(), format!("crate::model::{res_pascal}"))],
            return_type: format!("Option<crate::model::{res_pascal}>"),
            method: "post",
            path: format!("/api{route_base}/update"),
            body: mk_body(&format!("{store_path}::global().update(item)")),
        });
        synth.push(SynthServerFn {
            name: delete_name,
            args: vec![("id".into(), id_type.clone())],
            return_type: "bool".into(),
            method: "post",
            path: format!("/api{route_base}/delete"),
            body: mk_body(&format!("{store_path}::global().delete(id)")),
        });

        // 4. Screens: list + new. Edit/show require URL params which the
        //    DSL doesn't yet emit.
        let list_screen = format!("{res_pascal}ListScreen");
        let new_screen = format!("{res_pascal}NewScreen");
        let non_id_fields: Vec<DslFieldDef> = r
            .fields
            .iter()
            .filter(|f| f.name.to_snake_case() != id_field)
            .map(|f| DslFieldDef {
                name: f.name.clone(),
                ty: field_type_for_model_field(&f.ty),
                validation: None,
            })
            .collect();

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
            }),
        });
        doc.screens.push(DslScreen {
            name: new_screen,
            route: format!("{route_base}/new"),
            wrap_with: None,
            template: Some(DslScreenTemplate {
                kind: "resource_form".into(),
                endpoint: Some(create_name.clone()),
                item_type: Some(format!("crate::model::{res_pascal}")),
                on_submit: Some(create_name),
                redirect_to: Some(route_base.clone()),
                fields: non_id_fields,
            }),
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
    let upsert = upsert_mod_entry(&mod_rs, &snake)?;
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
        assert!(
            result
                .next_steps
                .iter()
                .any(|s| s.contains("pub mod model;")),
            "expected a `pub mod model;` next_step, got {:?}",
            result.next_steps
        );
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
        assert!(root.join("src/state/mod.rs").exists());

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

        // Helpful next_steps
        assert!(
            result
                .next_steps
                .iter()
                .any(|s| s.contains("pub mod state;")),
            "expected a `pub mod state;` next_step, got {:?}",
            result.next_steps
        );

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
        ] {
            let body = std::fs::read_to_string(root.join(rel)).unwrap();
            syn::parse_file(&body)
                .unwrap_or_else(|e| panic!("emitted {rel} does not parse: {e}\n---\n{body}"));
        }
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
            !root.join("src/state/order_store.rs").exists(),
            "dry_run must not write"
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
