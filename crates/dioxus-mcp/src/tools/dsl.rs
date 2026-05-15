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
    self, ArgSpec, CreateRouteParams, CreateServerFnParams, PropSpec, ScaffoldResult,
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
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslScreen {
    pub name: String,
    pub route: String,
    #[serde(default)]
    pub layout: Option<String>,
    /// Optional component name (e.g. a ProtectedRoute guard) that wraps the
    /// screen body. Imported from src/components and rendered around the page.
    #[serde(default)]
    pub wrap_with: Option<String>,
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
"#;

const CORE_COMPONENT: &str = r#"  Component:
    description: A reusable UI element. Generates src/components/{snake}.rs.
    fields:
      - {name: name, type: string, required: true}
      - {name: props, type: "PropDef[]", required: false}
    example:
      components:
        - name: UserCard
          props:
            - {name: id, type: i32}
            - {name: label, type: String, optional: true}
"#;

const CORE_SCREEN: &str = r#"  Screen:
    description: A top-level routed view. Generates a component file and inserts a route variant in src/router.rs.
    fields:
      - {name: name, type: string, required: true}
      - {name: route, type: string, required: true}
      - {name: layout, type: "sidebar|topnav|blank", required: false}
      - {name: wrap_with, type: "ComponentName (e.g. a ProtectedRoute guard)", required: false}
    example:
      screens:
        - name: HomeScreen
          route: /
          layout: sidebar
          wrap_with: Dashboard
"#;

const CORE_SERVER_FN: &str = r#"  ServerFn:
    description: An Axum-backed server fn using Dioxus 0.7's #[get/post("/path")] attribute. Requires fullstack feature on the dioxus dep.
    fields:
      - {name: name, type: string, required: true}
      - {name: args, type: "ArgDef[]", required: false}
      - {name: return_type, type: string, required: false}
      - {name: method, type: "get|post (defaults: post if args else get)", required: false}
      - {name: path, type: "string (default: /api/{snake_name})", required: false}
    example:
      server_fns:
        - name: fetch_users
          args:
            - {name: limit, type: u32}
          return_type: "Vec<String>"
          method: post
          path: /api/users
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
            div { class: "screen {{ snake }}{% if layout %} layout-{{ layout }}{% endif %}",
                h1 { "{{ pascal }}" }
            }
        }
{%- else %}
        div { class: "screen {{ snake }}{% if layout %} layout-{{ layout }}{% endif %}",
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
    out.push_str(CORE_COMPONENT);
    out.push_str(CORE_SCREEN);
    out.push_str(CORE_SERVER_FN);

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
    let doc: DslDoc = serde_yml::from_str(&p.code).map_err(|e| format!("YAML parse: {e}"))?;
    if doc.version != SPEC_VERSION {
        return Err(format!(
            "execute_code: version must be {SPEC_VERSION:?}, got {:?}",
            doc.version
        ));
    }

    let crate_root = scaffold::crate_root(state, p.project_root.as_deref()).await?;

    preflight(&doc, &crate_root)?;

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

    let mut result = ScaffoldResult {
        files_created: vec![],
        files_modified: vec![],
        next_steps: vec![],
    };

    // Order matters: server fns first (fail-fast on fullstack gating),
    // then leaf primitives, then screens (which call create_route serially).
    for sf in &doc.server_fns {
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

    for sig in &doc.signals {
        let r = generate_signal(&crate_root, sig)?;
        merge(&mut result, r);
    }

    let mut needs_websys = false;
    for s in &doc.sockets {
        let r = generate_socket(&crate_root, s)?;
        merge(&mut result, r);
        needs_websys = true;
    }

    for f in &doc.feeds {
        let r = generate_feed(&crate_root, f)?;
        merge(&mut result, r);
    }

    for c in &doc.components {
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
                project_root: p.project_root.clone(),
            },
        )
        .await?;
        merge(&mut result, r);
    }

    for f in &doc.forms {
        let r = generate_form(&crate_root, f)?;
        merge(&mut result, r);
    }

    for l in &doc.lists {
        let v = versioned_lists.contains(&l.name.to_snake_case());
        let r = generate_list(&crate_root, l, v)?;
        merge(&mut result, r);
    }

    for t in &doc.tables {
        let r = generate_table(&crate_root, t)?;
        merge(&mut result, r);
    }

    for s in &doc.session_states {
        let r = generate_session(&crate_root, s)?;
        merge(&mut result, r);
    }

    for ls in &doc.login_screens {
        let r = generate_login_screen(state, &crate_root, ls, p.project_root.as_deref()).await?;
        merge(&mut result, r);
    }

    for pr in &doc.protected_routes {
        let r = generate_protected_route(&crate_root, pr, &session_names)?;
        merge(&mut result, r);
    }

    for sc in &doc.screens {
        let r = generate_screen(state, &crate_root, sc, p.project_root.as_deref()).await?;
        merge(&mut result, r);
    }

    if needs_websys {
        result.next_steps.push(
            "add `web-sys = { version = \"0.3\", features = [\"WebSocket\", \"MessageEvent\", \"BinaryType\", \"ErrorEvent\"] }` and `wasm-bindgen = \"0.2\"` to your Cargo.toml for the generated socket(s)".into(),
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
}

// ---------- pre-flight ----------

fn preflight(doc: &DslDoc, crate_root: &Path) -> Result<(), String> {
    // 1. Collect every snake_case name across every primitive and reject dups
    //    that would land in the same target directory.
    let mut comp_names: BTreeSet<String> = BTreeSet::new();
    let mut sig_names: BTreeSet<String> = BTreeSet::new();
    let mut sock_names: BTreeSet<String> = BTreeSet::new();
    let mut srv_names: BTreeSet<String> = BTreeSet::new();
    let mut sess_names: BTreeSet<String> = BTreeSet::new();

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
    for s in &doc.session_states {
        if !sess_names.insert(s.name.to_snake_case()) {
            return Err(format!("duplicate session_state name: {}", s.name));
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

    // 3. Pre-check files that would collide with what's already on disk for
    //    each component-target name. (server_fn / signal / socket dirs may not
    //    exist yet; existence isn't an error there.)
    let comp_dir = crate_root.join("src/components");
    for n in &comp_names {
        if comp_dir.join(format!("{n}.rs")).exists() {
            return Err(format!(
                "src/components/{n}.rs already exists; refusing to overwrite"
            ));
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
    let dir = crate_root.join("src/components");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let target = dir.join(format!("{snake}.rs"));
    if target.exists() {
        return Err(format!("{} already exists", target.display()));
    }
    std::fs::write(&target, body).map_err(|e| e.to_string())?;
    let mod_rs = dir.join("mod.rs");
    let line = format!("pub mod {snake};\npub use {snake}::*;\n");
    let mut modified = vec![];
    let mut created = vec![target.clone()];
    if mod_rs.exists() {
        let mut current = std::fs::read_to_string(&mod_rs).map_err(|e| e.to_string())?;
        if !current.contains(&format!("pub mod {snake};")) {
            if !current.ends_with('\n') {
                current.push('\n');
            }
            current.push_str(&line);
            std::fs::write(&mod_rs, current).map_err(|e| e.to_string())?;
            modified.push(mod_rs);
        }
    } else {
        std::fs::write(&mod_rs, line).map_err(|e| e.to_string())?;
        created.push(mod_rs);
    }
    Ok(ScaffoldResult {
        files_created: created,
        files_modified: modified,
        next_steps: vec![],
    })
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
    let line = format!("pub mod {snake};\npub use {snake}::*;\n");
    let mut modified = vec![];
    let mut created = vec![target.clone()];
    if mod_rs.exists() {
        let mut current = std::fs::read_to_string(&mod_rs).map_err(|e| e.to_string())?;
        if !current.contains(&format!("pub mod {snake};")) {
            if !current.ends_with('\n') {
                current.push('\n');
            }
            current.push_str(&line);
            std::fs::write(&mod_rs, current).map_err(|e| e.to_string())?;
            modified.push(mod_rs);
        }
    } else {
        std::fs::write(&mod_rs, line).map_err(|e| e.to_string())?;
        created.push(mod_rs);
    }
    Ok(ScaffoldResult {
        files_created: created,
        files_modified: modified,
        next_steps: vec![],
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
    let body = render(
        "screen",
        SCREEN_TPL,
        context! {
            pascal => pascal.clone(),
            snake => snake.clone(),
            layout => sc.layout.clone(),
            wrap_pascal => wrap_pascal.clone(),
        },
    )?;
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
            ("CORE_COMPONENT", CORE_COMPONENT),
            ("CORE_SCREEN", CORE_SCREEN),
            ("CORE_SERVER_FN", CORE_SERVER_FN),
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
    fn detects_multidoc_yaml() {
        assert!(has_extra_documents("a: 1\n---\nb: 2"));
        assert!(!has_extra_documents("---\na: 1\nb: 2"));
        assert!(!has_extra_documents("# comment\na: 1"));
    }
}
