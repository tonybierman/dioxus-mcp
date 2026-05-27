use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Default)]
pub struct ScaffoldResult {
    pub files_created: Vec<PathBuf>,
    pub files_modified: Vec<PathBuf>,
    pub next_steps: Vec<String>,
    /// Files that already existed at a target path (populated when running
    /// `execute_code` with `if_missing: true` and a re-run skipped a primitive).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collisions: Vec<PathBuf>,
    /// Files that would be created — populated only by `execute_code` in
    /// `dry_run: true` mode.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub would_create: Vec<PathBuf>,
    /// Files that would be modified — populated only by `execute_code` in
    /// `dry_run: true` mode.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub would_modify: Vec<PathBuf>,
    /// True when the result is a dry-run plan rather than an applied change.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dry_run: bool,
    /// High-level outcome of the call. `"no_changes"` when nothing was written
    /// (everything collided under if_missing); `"partial"` when at least one
    /// primitive was skipped but others applied; `"applied"` when the whole
    /// doc landed cleanly. Populated by `execute_code` at the end of the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// File containing the `#[derive(Routable)]` enum where new Screen /
    /// LoginScreen variants will be inserted. Populated by `execute_code` when
    /// the doc declares routes, both for dry_run plans and applied runs.
    /// Useful when the enum lives somewhere other than `src/router.rs` (e.g.
    /// inlined in `src/main.rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routable_file: Option<PathBuf>,
    /// Generated file contents keyed by would-be path. Populated by
    /// `execute_code` in `dry_run: true` mode so the agent can preview what
    /// a template emits without committing. Currently scoped to Screen bodies
    /// (the main case where agents bypass the primitive because they can't
    /// predict the output); other primitives stay path-only.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub previews: std::collections::BTreeMap<PathBuf, String>,
    /// Structured, already-resolved render models for screens a client can't
    /// reconstruct on its own — specifically the server-synthesized resource
    /// screens (a `resources:` block expands into list/new/edit screens that
    /// never appear in the user's `screens:`). Populated by `execute_code` in
    /// `dry_run: true` mode alongside `previews`. Additive: empty for docs
    /// without resource slices, so existing clients are unaffected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub render_models: Vec<RenderModel>,
}

/// A resolved, renderable description of one screen. Unlike `previews` (raw RSX
/// text), this is structured data a browser client can tree-walk directly.
#[derive(Debug, Clone, Serialize, Default)]
pub struct RenderModel {
    /// PascalCase screen name.
    pub screen: String,
    /// Resolved template kind: `resource_list` | `resource_form` |
    /// `resource_edit_form`.
    pub kind: String,
    /// The screen's route.
    pub route: String,
    /// PascalCase model/item type the screen is built around.
    pub item_type: String,
    /// Root `div` class the generated screen uses (e.g. `screen product_list`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_class: Option<String>,
    /// Table columns for `resource_list` (from the model's fields). `ty` is the
    /// Rust type, so the client can synthesize type-appropriate mock cells.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<RenderField>,
    /// Form inputs for `resource_form` / `resource_edit_form`. `ty` is the
    /// input kind (text/email/number/checkbox/textarea/…).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<RenderField>,
    /// `resource_list`: server fn that returns the rows (shown in the
    /// "mock data" note).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_endpoint: Option<String>,
    /// `resource_list`: route to the "new" screen, for the toolbar link.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_route: Option<String>,
}

/// A column or input field within a [`RenderModel`].
#[derive(Debug, Clone, Serialize, Default)]
pub struct RenderField {
    /// snake_case field name.
    pub name: String,
    /// Human-readable label (Title Case).
    pub label: String,
    /// Rust type (columns) or HTML input kind (fields).
    pub ty: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PropSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct CreateComponentParams {
    /// Component name in any case; will be normalized to PascalCase / snake_case.
    pub name: String,
    #[serde(default)]
    pub props: Vec<PropSpec>,
    /// Optional override directory (relative to crate root). Defaults to `src/components`.
    pub path: Option<String>,
    /// Stub-body skeleton. One of: `empty` (default — single placeholder div),
    /// `form` (form with submit handler), `list` (ul with empty-state),
    /// `crud_table` (table with header + toolbar), `resource_view` (article
    /// with field list + edit/delete actions). Templates are structural only —
    /// they do not wire to any data source; pair with `props:` or hand-edit
    /// after generation.
    #[serde(default)]
    pub template: Option<String>,
    /// Absolute path to the Dioxus project root. Required when the MCP server was not
    /// started in the target project directory.
    pub project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct CreateRouteParams {
    /// Route path, e.g. "/users/:id".
    pub path: String,
    /// Component name to render.
    pub component: String,
    /// File containing the `#[derive(Routable)]` enum. Defaults to `src/router.rs` then `src/main.rs`.
    pub router_file: Option<String>,
    /// Absolute path to the Dioxus project root. Required when the MCP server was not
    /// started in the target project directory.
    pub project_root: Option<String>,
    /// Optional path-param fields for the variant. Each entry is `(name, type)`
    /// and lands as `Variant { name: type }` so Dioxus's Routable derive can
    /// extract the value from the URL. Omit for variants with no path params.
    #[serde(default)]
    pub params: Vec<(String, String)>,
    /// Optional module path that the component is exported from (e.g.
    /// `"crate::components"`). When set, `create_route` also ensures the
    /// router file has `use {import_path}::{Component};` (or a `::*` glob
    /// matching that prefix) so Dioxus's Routable derive can resolve the
    /// variant's component. No-op when the import is already in scope.
    #[serde(default)]
    pub import_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ArgSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct CreateServerFnParams {
    pub name: String,
    #[serde(default)]
    pub args: Vec<ArgSpec>,
    /// Defaults to `String`.
    pub return_type: Option<String>,
    /// HTTP method: "get" or "post". Defaults to "post" when args is non-empty,
    /// "get" otherwise.
    pub method: Option<String>,
    /// Route path under which the server fn is exposed. Defaults to
    /// "/api/{snake_name}".
    pub path: Option<String>,
    /// Axum-style request extractors declared on the route attribute. Each
    /// entry lands as `name: ty` inside the `#[get/post(...)]` attribute's
    /// argument list only — the Dioxus 0.7.9 verb-macro binds the name into
    /// the fn scope itself, and repeating it in the rust fn signature would
    /// break `FromRequest` for the body tuple. Example:
    /// `extractors: [{ name: cookies, type: "TypedHeader<Cookie>" }]`
    /// emits `#[get("/api/board", cookies: TypedHeader<Cookie>)]` and the
    /// body can reference `cookies` directly. The user must already have
    /// `axum_extra` / `axum::headers` / etc. in scope.
    #[serde(default)]
    pub extractors: Vec<ArgSpec>,
    /// When true, the scaffolder injects the canonical cookie-authed prologue:
    /// it ensures a `cookies: TypedHeader<Cookie>` extractor is present, pulls
    /// the session id out of `session_cookie` (default `"session_id"`), and
    /// maps the missing-cookie case to `ServerFnError::new("not logged in")`.
    /// A trailing `// TODO touch_session(...)` marker is left in place so
    /// callers can wire their session store.
    #[serde(default)]
    pub auth_required: bool,
    /// Cookie name read by the auth prologue. Only consulted when
    /// `auth_required: true`. Default: `"session_id"`.
    #[serde(default)]
    pub session_cookie: Option<String>,
    /// Absolute path to the Dioxus project root. Required when the MCP server was not
    /// started in the target project directory.
    pub project_root: Option<String>,
}

/// Result of upserting an entry into a `mod.rs` file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModUpsert {
    /// The file was created from scratch.
    Created,
    /// The file existed and we added the entry (or re-sorted).
    Modified,
    /// The file already declared this module — no write.
    Unchanged,
}
