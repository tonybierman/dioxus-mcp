pub(super) const SCREEN_TPL: &str = r#"use dioxus::prelude::*;
{%- if wrap_pascal %}
use crate::components::{{ wrap_pascal }};
{%- endif %}
{%- if store_snake %}
// Glob-import brings `use_{{ store_snake }}` AND the `#[store(pub)]`-generated
// extension trait into scope so call sites can invoke the typed methods.
use crate::state::{{ store_snake }}::*;
{%- endif %}

#[component]
pub fn {{ pascal }}() -> Element {
{%- if store_snake %}
    let _store = use_{{ store_snake }}();
    // `_store` exposes the ClientStore context; rename and use as needed.
{%- endif %}
{%- if body_empty %}
    rsx! {}
{%- else %}
    rsx! {
{%- if wrap_pascal %}
        {{ wrap_pascal }} {
            div { class: "{{ root_class }}",
                h1 { "{{ pascal }}" }
            }
        }
{%- else %}
        div { class: "{{ root_class }}",
            h1 { "{{ pascal }}" }
        }
{%- endif %}
    }
{%- endif %}
}
"#;

pub(super) const FORM_TPL: &str = r#"use dioxus::prelude::*;
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

pub(super) const LIST_TPL: &str = r#"use dioxus::prelude::*;
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

pub(super) const TABLE_TPL: &str = r#"use dioxus::prelude::*;
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

pub(super) const SIGNAL_TPL: &str = r#"use dioxus::prelude::*;

pub fn provide_{{ snake }}() -> Signal<{{ ty }}> {
    use_context_provider(|| Signal::new({{ initial }}))
}

pub fn use_{{ snake }}() -> Signal<{{ ty }}> {
    use_context::<Signal<{{ ty }}>>()
}
"#;

pub(super) const SOCKET_TPL: &str = r#"// Generated WebSocket binding (web-sys).
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

pub(super) const FEED_TPL: &str = r#"use dioxus::prelude::*;
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

pub(super) const SESSION_TPL: &str = r#"use dioxus::prelude::*;

pub fn provide_{{ snake }}() -> Signal<Option<{{ user_type }}>> {
    use_context_provider(|| Signal::new(None::<{{ user_type }}>))
}

pub fn use_{{ snake }}() -> Signal<Option<{{ user_type }}>> {
    use_context::<Signal<Option<{{ user_type }}>>>()
}
"#;

pub(super) const LOGIN_TPL: &str = r#"use dioxus::prelude::*;

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

pub(super) const PROTECTED_TPL: &str = r#"use dioxus::prelude::*;
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

pub(super) const MODEL_TPL: &str = r#"use serde::{Deserialize, Serialize};

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

pub(super) const STORE_TPL: &str = r#"#![cfg(feature = "server")]
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

pub(super) const SCREEN_RESOURCE_LIST_TPL: &str = r#"use dioxus::prelude::*;
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

pub(super) const SCREEN_RESOURCE_FORM_TPL: &str = r#"use dioxus::prelude::*;
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

/// Client-side reactive store, exposed via context. NOT gated on the server
/// feature — runs anywhere Dioxus runs. Uses Dioxus 0.7's canonical
/// `#[derive(Store)]` + `#[store]` extension trait for path-isolated
/// reactivity. Helpers mirror the spec: `push`, `clear`, and (when
/// `id_field` is set) `remove_by_id` + `update_by_id`. With `auto_id` the
/// store owns a plain `next_id: {id_type}` field and exposes a
/// `push_new(item)` helper that sets `item.{id_field}` before pushing.
pub(super) const CLIENT_STORE_TPL: &str = r#"use dioxus::prelude::*;
{%- if needs_model_import %}
use crate::model::{{ item_type }};
{%- endif %}

#[derive(Store, Clone, Default)]
pub struct {{ pascal }} {
    pub items: Vec<{{ item_type }}>,
{%- if auto_id %}
    pub next_id: {{ id_type }},
{%- endif %}
}

// The `pub` argument makes the generated `{{ pascal }}StoreExt` extension
// trait public so consumers in other modules can call these methods after
// a `use crate::state::{{ snake }}::*;` import. Method visibility tracks
// the trait, so no `pub` qualifier on the individual fns.
#[store(pub)]
impl Store<{{ pascal }}> {
    fn push(&mut self, item: {{ item_type }}) {
        self.items().write().push(item);
    }

    fn clear(&mut self) {
        self.items().write().clear();
    }
{%- if auto_id %}

    /// Assign the next id to `item.{{ id_field }}` then push. The id
    /// allocator lives inside the store, so call sites can drop the id
    /// field from the struct literal.
    fn push_new(&mut self, item: {{ item_type }}) -> {{ id_type }} {
        let mut item = item;
        let id = self.next_id().cloned();
        self.next_id().set(id + 1{{ id_type_suffix }});
        item.{{ id_field }} = id;
        self.items().write().push(item);
        id
    }
{%- endif %}
{%- if id_field %}

    fn remove_by_id(&mut self, id: {{ id_type }}) -> bool {
        let mut items = self.items();
        let before = items.read().len();
        items.write().retain(|x| x.{{ id_field }} != id);
        let after = items.read().len();
        after < before
    }

    fn update_by_id(&mut self, id: {{ id_type }}, f: impl FnOnce(&mut {{ item_type }})) {
        let mut items = self.items();
        let mut guard = items.write();
        if let Some(item) = guard.iter_mut().find(|x| x.{{ id_field }} == id) {
            f(item);
        }
    }
{%- endif %}
{%- if checkbox_field %}

    /// Drop every item whose `{{ checkbox_field }}` field is true. Mirrors the
    /// canonical "Clear completed" action so call sites can stay in the
    /// store's typed extension trait instead of reaching into
    /// `self.items().write().retain(...)`.
    fn clear_{{ checkbox_field }}(&mut self) {
        self.items().write().retain(|x| !x.{{ checkbox_field }});
    }

    /// Count of items whose `{{ checkbox_field }}` is false — the "remaining"
    /// readout in canonical TodoMVC-shaped UIs. Read inside `rsx!` as
    /// `store.remaining()` — Dioxus reactivity tracks the underlying
    /// `items()` signal.
    fn remaining(&self) -> usize {
        self.items().read().iter().filter(|x| !x.{{ checkbox_field }}).count()
    }

    /// True when at least one item has `{{ checkbox_field }}` set. Lets call
    /// sites gate a "Clear completed" button without re-running `iter().any`
    /// at every render site.
    fn any_{{ checkbox_field }}(&self) -> bool {
        self.items().read().iter().any(|x| x.{{ checkbox_field }})
    }
{%- endif %}
}

pub fn provide_{{ snake }}() -> Store<{{ pascal }}> {
    use_context_provider(|| Store::new({{ pascal }} {
        items: {{ initial }},
{%- if auto_id %}
        next_id: 1{{ id_type_suffix }},
{%- endif %}
    }))
}

pub fn use_{{ snake }}() -> Store<{{ pascal }}> {
    use_context::<Store<{{ pascal }}>>()
}
"#;

/// Screen template that wires an "add input + list with delete (and optional
/// checkbox)" UI to a ClientStore. No server fn round-trip — all state lives
/// in the `Store<T>`-backed context store.
///
/// When `checkbox_field` is set, a sibling `{{ pascal }}Row` component is
/// emitted below the screen so the per-row body (with its closure captures)
/// is one prop boundary away from the screen — easier to restyle, easier to
/// add per-row hooks (drag handles, editing-in-place, …) without rewriting
/// the parent.
pub(super) const CLIENT_CRUD_SCREEN_TPL: &str = r#"use dioxus::prelude::*;
{%- if wrap_pascal %}
use crate::components::{{ wrap_pascal }};
{%- endif %}
// Glob-import brings `use_{{ store_snake }}` AND the `#[store(pub)]`-generated
// extension trait into scope so call sites can invoke the typed methods.
use crate::state::{{ store_snake }}::*;
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
{%- if row_component %}

{{ row_component }}
{%- endif %}
"#;

/// Resource-synthesized list screen with a real table: column headers from the
/// model fields, keyed rows, per-row Edit link, Delete button (calls the
/// delete server-fn and bumps a local version signal to refetch), and an
/// empty-state CTA. Used when `crud_ctx` is set on a `resource_list` template.
pub(super) const SCREEN_RESOURCE_CRUD_LIST_TPL: &str = r#"use dioxus::prelude::*;
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
pub(super) const SCREEN_RESOURCE_EDIT_FORM_TPL: &str = r#"use dioxus::prelude::*;
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

pub(super) const SERVER_FN_WITH_BODY_TPL: &str = r#"use dioxus::prelude::*;
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
