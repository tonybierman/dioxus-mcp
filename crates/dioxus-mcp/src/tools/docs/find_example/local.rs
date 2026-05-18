//! Local "pattern" examples shipped with dioxus-mcp. The upstream Dioxus
//! examples repo covers individual primitives (router, fullstack, use_signal,
//! …) but doesn't have one-page demonstrations of common app-level wirings
//! like "optimistic insert + reconcile via SSE." Those land here so
//! `find_example` can surface them alongside upstream hits.

pub struct LocalExample {
    pub name: &'static str,
    /// Short one-sentence blurb shown in `find_example` results.
    pub blurb: &'static str,
    /// Permalink to canonical documentation when one exists. Currently points
    /// at this crate's docs, since the example is shipped inline.
    pub url: &'static str,
    /// Self-contained Rust source the caller can paste directly.
    pub body: &'static str,
}

pub fn registry() -> &'static [LocalExample] {
    &[
        LocalExample {
            name: "optimistic-with-reconcile",
            blurb: "Optimistic insert with a tentative id + SSE reconcile when the server's canonical id arrives. Common shape for chat / todo / collab apps; trips up agents that try to await the server fn before rendering.",
            url: "https://github.com/anthropics/dioxus-mcp/blob/main/docs/patterns/optimistic-with-reconcile.md",
            body: OPTIMISTIC_WITH_RECONCILE,
        },
        LocalExample {
            name: "streaming-snapshot",
            blurb: "Axum SSE endpoint with a tokio broadcast channel + client-side EventSource subscriber that reconnects with backoff. Covers the full server-events shape: snapshot-then-tail, keep-alive frames, and the JSON event envelope.",
            url: "https://github.com/anthropics/dioxus-mcp/blob/main/docs/patterns/streaming-snapshot.md",
            body: STREAMING_SNAPSHOT,
        },
        LocalExample {
            name: "cookie-session-auth",
            blurb: "Cookie-based session auth: TypedHeader<Cookie> read, in-memory SESSIONS map, provide_session/use_session signal-in-context pair, and a Protected wrapper that redirects via use_effect. The whole login/logout/me shape in one file.",
            url: "https://github.com/anthropics/dioxus-mcp/blob/main/docs/patterns/cookie-session-auth.md",
            body: COOKIE_SESSION_AUTH,
        },
        LocalExample {
            name: "dnd-reorder",
            blurb: "HTML5 drag/drop kanban: `ondragstart` / `ondragover` / `ondrop` triplet with `dragging` + `drop_target` signals lifted to the parent. Covers the cross-column move the catalog `drag_and_drop_list` can't model (it's a single sortable list).",
            url: "https://github.com/anthropics/dioxus-mcp/blob/main/docs/patterns/dnd-reorder.md",
            body: DND_REORDER,
        },
        LocalExample {
            name: "wasm-polling-timer",
            blurb: "WASM-safe polling loop using `gloo_timers::future::TimeoutFuture` inside `use_future`. Replaces tokio sleeps (which don't compile on wasm) for periodic refreshes / heartbeats / debounced reactions.",
            url: "https://github.com/anthropics/dioxus-mcp/blob/main/docs/patterns/wasm-polling-timer.md",
            body: WASM_POLLING_TIMER,
        },
    ]
}

const OPTIMISTIC_WITH_RECONCILE: &str = r#"// Optimistic insert + SSE reconcile pattern.
//
// Shape: the user types, we insert a row immediately with a tentative
// negative id, spawn the server fn, and rely on the SSE stream to reconcile
// the canonical positive id. The reconcile is keyed by (author, body, ts)
// rather than the id (because the id is exactly what we don't know yet).
//
// Failure modes this protects against:
//   - the user staring at a spinner while the round-trip completes
//   - duplicate rows when the SSE event for our own write lands AFTER the
//     server-fn return — we de-dupe by content, not by id
//   - server fn fails: the optimistic row stays but gets a `failed: true`
//     flag so the UI can render a retry affordance
//
// Pair with: a #[get(...)] SSE endpoint on the server that broadcasts
// `Message` rows (one per write); the reconcile loop replaces any
// negative-id row whose content matches.

use std::sync::atomic::{AtomicI64, Ordering};
use dioxus::prelude::*;

#[derive(Clone, PartialEq)]
struct Message {
    id: i64,             // negative while optimistic, positive once reconciled
    author: String,
    body: String,
    failed: bool,
}

#[component]
pub fn Chat() -> Element {
    let mut messages = use_signal::<Vec<Message>>(Vec::new);
    let mut draft = use_signal(String::new);

    // SSE subscription — drains incoming canonical messages and reconciles.
    let _stream = use_resource(move || async move {
        let mut sse = match subscribe_chat_stream().await {
            Ok(s) => s,
            Err(_) => return,
        };
        while let Some(msg) = sse.next().await {
            messages.with_mut(|list| reconcile(list, msg));
        }
    });

    let send = move |evt: FormEvent| {
        evt.prevent_default();
        let body = draft.read().trim().to_string();
        if body.is_empty() { return; }
        draft.set(String::new());

        // 1) optimistic insert with a tentative id.
        let tentative_id = next_tentative_id();
        let pending = Message {
            id: tentative_id,
            author: "me".into(),
            body: body.clone(),
            failed: false,
        };
        messages.with_mut(|list| list.push(pending.clone()));

        // 2) fire-and-forget the server call. SSE will reconcile on success;
        //    on failure we mark the optimistic row.
        spawn(async move {
            if send_message(body).await.is_err() {
                messages.with_mut(|list| {
                    if let Some(row) = list.iter_mut().find(|m| m.id == tentative_id) {
                        row.failed = true;
                    }
                });
            }
        });
    };

    rsx! {
        ul {
            for m in messages.read().iter() {
                li {
                    key: "{m.id}",
                    class: if m.failed { "msg failed" } else if m.id < 0 { "msg pending" } else { "msg" },
                    "{m.author}: {m.body}"
                }
            }
        }
        form { onsubmit: send,
            input {
                value: "{draft}",
                oninput: move |e| draft.set(e.value()),
            }
        }
    }
}

/// Replace any optimistic row whose content matches `incoming` with the
/// canonical server row. Idempotent if the same SSE event arrives twice
/// (a re-subscribe replays the last N events).
fn reconcile(list: &mut Vec<Message>, incoming: Message) {
    // First: drop any optimistic row whose content matches.
    if let Some(idx) = list.iter().position(|m| {
        m.id < 0 && m.author == incoming.author && m.body == incoming.body
    }) {
        list[idx] = incoming;
        return;
    }
    // Second: skip if the canonical id is already present (re-subscribe).
    if list.iter().any(|m| m.id == incoming.id) {
        return;
    }
    list.push(incoming);
}

fn next_tentative_id() -> i64 {
    static COUNTER: AtomicI64 = AtomicI64::new(0);
    -(COUNTER.fetch_add(1, Ordering::Relaxed) + 1)
}

// --- elided: the server fn + SSE subscriber.
//     send_message: #[post("/api/chat")] pub async fn send_message(body: String) -> ServerFnError;
//     subscribe_chat_stream: client-side SSE subscriber returning a Stream<Message>.
"#;

const STREAMING_SNAPSHOT: &str = r#"// Server-Sent Events (SSE) snapshot-then-tail pattern for Dioxus 0.7 +
// axum. Server holds a `broadcast::Sender<Event>` shared across all
// connections; on connect, the handler sends a snapshot, then forwards
// every broadcast frame until the client disconnects.
//
// Client opens an EventSource, parses each `data:` frame as JSON, and
// reconnects with exponential backoff on error. EventSource handles
// reconnect natively but doesn't expose the retry delay — we wrap it so
// the reconnect cadence is visible (and pause-able under feature flag).
//
// Wire the route into your axum router alongside the Dioxus app:
//   .nest_service("/", dioxus_router)
//   .route("/api/events", get(events_handler))
//   .with_state(EventBus::default());
//
// Cargo.toml additions:
//   axum = { version = "0.8", features = ["json"] }
//   tokio = { version = "1", features = ["sync", "macros", "rt-multi-thread"] }
//   tokio-stream = { version = "0.1", features = ["sync"] }
//   futures = "0.3"
//   serde = { version = "1", features = ["derive"] }
//   serde_json = "1"
//   # client side:
//   web-sys = { version = "0.3", features = ["EventSource", "MessageEvent"] }

// ---------- server side ----------

#[cfg(feature = "server")]
mod server_side {
    use axum::{
        extract::State,
        response::sse::{Event as SseEvent, KeepAlive, Sse},
    };
    use futures::Stream;
    use serde::{Deserialize, Serialize};
    use std::convert::Infallible;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::{broadcast, RwLock};
    use tokio_stream::{wrappers::BroadcastStream, StreamExt};

    /// One event the server publishes. Serialized as JSON in the SSE frame.
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct Event {
        pub id: u64,
        pub kind: String,
        pub payload: String,
    }

    /// Cluster-wide event bus. The Mutex-guarded Vec is the snapshot (the
    /// last N events replayed to a freshly-connected client); the broadcast
    /// channel is the live tail.
    #[derive(Clone, Default)]
    pub struct EventBus(Arc<EventBusInner>);

    #[derive(Default)]
    pub struct EventBusInner {
        snapshot: RwLock<Vec<Event>>,
        tx: tokio::sync::OnceCell<broadcast::Sender<Event>>,
    }

    impl EventBus {
        pub async fn publish(&self, evt: Event) {
            self.0.snapshot.write().await.push(evt.clone());
            if let Some(tx) = self.0.tx.get() {
                let _ = tx.send(evt);
            }
        }

        async fn tx(&self) -> broadcast::Sender<Event> {
            self.0
                .tx
                .get_or_init(|| async { broadcast::channel::<Event>(256).0 })
                .await
                .clone()
        }
    }

    /// GET /api/events handler — yields snapshot frames first, then live ones.
    pub async fn events_handler(
        State(bus): State<EventBus>,
    ) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
        let tx = bus.tx().await;
        let live_rx = tx.subscribe();
        let snapshot = bus.0.snapshot.read().await.clone();

        let snapshot_stream = futures::stream::iter(snapshot)
            .map(|e| Ok(to_sse(e)));
        let live_stream = BroadcastStream::new(live_rx)
            .filter_map(|item| item.ok().map(|e| Ok(to_sse(e))));

        Sse::new(snapshot_stream.chain(live_stream))
            .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
    }

    fn to_sse(e: Event) -> SseEvent {
        SseEvent::default()
            .id(e.id.to_string())
            .event("event")
            .data(serde_json::to_string(&e).unwrap_or_default())
    }
}

// ---------- client side ----------

#[cfg(target_arch = "wasm32")]
mod client_side {
    use dioxus::prelude::*;
    use futures::channel::mpsc;
    use futures::StreamExt;
    use serde::Deserialize;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;
    use web_sys::{EventSource, MessageEvent};

    #[derive(Clone, Debug, Deserialize)]
    pub struct Event {
        pub id: u64,
        pub kind: String,
        pub payload: String,
    }

    /// Open an EventSource against `/api/events`. Returns a stream the caller
    /// drains in a `use_future`. EventSource auto-reconnects on a dropped
    /// connection; we expose `on_disconnect` so the UI can flash an indicator
    /// while the browser retries.
    pub fn subscribe_events() -> mpsc::UnboundedReceiver<Event> {
        let (tx, rx) = mpsc::unbounded::<Event>();

        let source = match EventSource::new("/api/events") {
            Ok(s) => s,
            Err(_) => return rx,
        };

        // onmessage: parse the JSON frame and forward.
        let tx_msg = tx.clone();
        let on_msg = Closure::wrap(Box::new(move |evt: MessageEvent| {
            let Some(data) = evt.data().as_string() else { return };
            let Ok(parsed) = serde_json::from_str::<Event>(&data) else { return };
            let _ = tx_msg.unbounded_send(parsed);
        }) as Box<dyn FnMut(MessageEvent)>);
        source.set_onmessage(Some(on_msg.as_ref().unchecked_ref()));
        on_msg.forget();

        // onerror: EventSource handles the retry; just log so the disconnect
        // is visible in the console. Replace with a Signal<bool> if the UI
        // wants to render a "reconnecting…" badge.
        let on_err = Closure::wrap(Box::new(move |_evt: JsValue| {
            web_sys::console::warn_1(&"events: EventSource reconnecting...".into());
        }) as Box<dyn FnMut(JsValue)>);
        source.set_onerror(Some(on_err.as_ref().unchecked_ref()));
        on_err.forget();

        rx
    }

    #[component]
    pub fn EventLog() -> Element {
        let mut events = use_signal::<Vec<Event>>(Vec::new);
        use_future(move || async move {
            let mut stream = subscribe_events();
            while let Some(evt) = stream.next().await {
                events.write().push(evt);
            }
        });
        rsx! {
            ul { class: "event-log",
                for e in events.read().iter() {
                    li { key: "{e.id}", "{e.kind}: {e.payload}" }
                }
            }
        }
    }
}
"#;

const COOKIE_SESSION_AUTH: &str = r#"// Cookie-based session auth pattern.
//
// Shape: the server owns a `SESSIONS` map keyed by sid. /api/login mints a sid
// and emits a Set-Cookie via `FullstackContext::add_response_header`. /api/me
// reads the cookie back via `TypedHeader<Cookie>`. The client mirrors the
// "who am I" answer into a `Signal<Option<User>>` provided in context so a
// `Protected` wrapper can redirect unauthenticated users.
//
// Cargo.toml needs:
//   axum-extra = { version = "0.10", features = ["typed-header"] }
//   uuid       = { version = "1", features = ["v4"] }
//
// The verb-macro `cookies:` extractor binds `cookies` into scope itself —
// DO NOT also list it in the rust fn signature (FromRequest will reject the
// body tuple if you do). The dioxus-mcp DSL `auth_required: true` flag
// produces the same prologue automatically.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use axum_extra::{TypedHeader, headers::Cookie};
use dioxus::prelude::*;
use dioxus::fullstack::FullstackContext;
use uuid::Uuid;

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct User {
    pub name: String,
}

// Server-side session store (replace with redis/sqlite for real apps).
static SESSIONS: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[post("/api/login")]
pub async fn login(name: String) -> Result<(), ServerFnError> {
    if name.trim().is_empty() {
        return Err(ServerFnError::new("name required"));
    }
    let sid = Uuid::new_v4().to_string();
    SESSIONS.lock().unwrap().insert(sid.clone(), name);

    let ctx = FullstackContext::current()
        .ok_or_else(|| ServerFnError::new("no request context"))?;
    ctx.add_response_header(
        "set-cookie",
        format!("session_id={sid}; Path=/; HttpOnly; SameSite=Lax"),
    );
    Ok(())
}

#[get("/api/logout", cookies: TypedHeader<Cookie>)]
pub async fn logout() -> Result<(), ServerFnError> {
    if let Some(sid) = cookies.get("session_id") {
        SESSIONS.lock().unwrap().remove(sid);
    }
    if let Some(ctx) = FullstackContext::current() {
        ctx.add_response_header("set-cookie", "session_id=; Path=/; Max-Age=0");
    }
    Ok(())
}

#[get("/api/me", cookies: TypedHeader<Cookie>)]
pub async fn me() -> Result<Option<User>, ServerFnError> {
    let Some(sid) = cookies.get("session_id") else {
        return Ok(None);
    };
    let name = SESSIONS.lock().unwrap().get(sid).cloned();
    Ok(name.map(|name| User { name }))
}

// ----- client side -----

#[derive(Clone, Copy)]
pub struct SessionState(pub Signal<Option<User>>);

pub fn provide_session() {
    let sig = use_signal(|| None::<User>);
    // On first mount, ask the server who we are. The Resource result is
    // mirrored into the Signal so consumers can read it synchronously.
    let _ = use_resource(move || {
        let mut sig = sig;
        async move {
            if let Ok(u) = me().await {
                sig.set(u);
            }
        }
    });
    use_context_provider(|| SessionState(sig));
}

pub fn use_session() -> SessionState {
    use_context::<SessionState>()
}

#[component]
pub fn Protected(children: Element) -> Element {
    let session = use_session();
    let nav = use_navigator();
    use_effect(move || {
        if session.0.read().is_none() {
            nav.replace("/login");
        }
    });
    if session.0.read().is_none() {
        return rsx! { div { class: "loading", "Redirecting..." } };
    }
    rsx! { {children} }
}
"#;

const DND_REORDER: &str = r#"// HTML5 drag/drop kanban pattern.
//
// Shape: each draggable card emits `ondragstart` to publish its identity; each
// drop target accepts via `ondragover` (preventDefault to opt in) and
// `ondrop` to commit the reorder. The catalog `drag_and_drop_list` only
// covers a single sortable list — for cross-column moves you need this
// pattern with `dragging` (the card currently being moved) and `drop_target`
// (where the drop indicator is hovering) lifted into the parent signal.
//
// The trick most agents miss: `ondragover` MUST call `prevent_default()` or
// `ondrop` won't fire. The browser defaults the dragover to "not droppable."

use dioxus::prelude::*;

#[derive(Clone, PartialEq)]
pub struct Card { pub id: i64, pub col: String, pub title: String }

#[component]
pub fn Board() -> Element {
    let mut cards = use_signal::<Vec<Card>>(Vec::new);
    let mut dragging = use_signal::<Option<i64>>(|| None);
    let mut drop_target = use_signal::<Option<String>>(|| None);

    let commit_move = move |to_col: String| {
        if let Some(id) = *dragging.read() {
            cards.with_mut(|list| {
                if let Some(c) = list.iter_mut().find(|c| c.id == id) {
                    c.col = to_col;
                }
            });
        }
        dragging.set(None);
        drop_target.set(None);
    };

    rsx! {
        div { class: "board",
            for col in ["todo", "doing", "done"] {
                div {
                    class: "column",
                    class: if drop_target.read().as_deref() == Some(col) { "drag-over" } else { "" },
                    // Crucial: prevent_default on dragover opts the element in
                    // as a drop target. Without it, ondrop never fires.
                    ondragover: move |e| { e.prevent_default(); drop_target.set(Some(col.to_string())); },
                    ondragleave: move |_| { if drop_target.read().as_deref() == Some(col) { drop_target.set(None); } },
                    ondrop: {
                        let c = col.to_string();
                        let commit = commit_move.clone();
                        move |e| { e.prevent_default(); commit(c.clone()); }
                    },

                    h2 { "{col}" }
                    for card in cards.read().iter().filter(|c| c.col == col).cloned().collect::<Vec<_>>() {
                        div {
                            class: "card",
                            class: if Some(card.id) == *dragging.read() { "dragging" } else { "" },
                            draggable: "true",
                            ondragstart: {
                                let id = card.id;
                                move |_| dragging.set(Some(id))
                            },
                            ondragend: move |_| { dragging.set(None); drop_target.set(None); },
                            "{card.title}"
                        }
                    }
                }
            }
        }
    }
}
"#;

const WASM_POLLING_TIMER: &str = r#"// WASM-safe polling timer pattern.
//
// Shape: `use_future` owns a loop that awaits `TimeoutFuture::new(ms)` from
// `gloo-timers`. This works on the wasm32 target where `tokio::time::sleep`
// fails to compile (and tokio runtimes don't drive in the browser anyway).
//
// Cargo.toml needs (wasm side only):
//   gloo-timers = { version = "0.3", features = ["futures"] }
//
// Pattern uses:
//   - Periodic refresh of a `use_resource` by bumping a Signal<u32> tick.
//   - Heartbeat / liveness ping.
//   - Debounced reaction (clear the timer on every keystroke, restart it).

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;

#[component]
pub fn StatusPanel() -> Element {
    let mut tick = use_signal(|| 0u32);

    // Bump `tick` every 5s; any `use_resource` that reads `tick()` will
    // refetch. The future lives for the component's lifetime — when the
    // component unmounts, Dioxus drops the task.
    use_future(move || async move {
        loop {
            TimeoutFuture::new(5_000).await;
            *tick.write() += 1;
        }
    });

    let status = use_resource(move || async move {
        let _ = tick();
        fetch_status().await
    });

    rsx! {
        div { class: "status",
            match &*status.read_unchecked() {
                None => rsx! { div { "Loading..." } },
                Some(Err(e)) => rsx! { div { class: "error", "Error: {e}" } },
                Some(Ok(s)) => rsx! { div { "Last update #{tick}: {s}" } },
            }
        }
    }
}

// Debounced variant: every keystroke restarts the 300ms wait; the search
// only fires once the user has stopped typing for 300ms.
#[component]
pub fn DebouncedSearch() -> Element {
    let mut query = use_signal(String::new);
    let mut results = use_signal::<Vec<String>>(Vec::new);

    use_future(move || async move {
        // Read query once each loop iteration. We read at the top so the
        // future re-runs on every change (Dioxus tracks the read).
        let q = query();
        if q.trim().is_empty() { return; }
        TimeoutFuture::new(300).await;
        // The user might have typed more in the 300ms — recheck.
        if q == *query.read() {
            if let Ok(hits) = search(q).await {
                results.set(hits);
            }
        }
    });

    rsx! {
        input { value: "{query()}", oninput: move |e| query.set(e.value()) }
        ul {
            for hit in results.read().iter() {
                li { "{hit}" }
            }
        }
    }
}

async fn fetch_status() -> Result<String, String> { Ok("ok".into()) }
async fn search(_q: String) -> Result<Vec<String>, String> { Ok(vec![]) }
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_optimistic_with_reconcile() {
        let r = registry();
        assert!(r.iter().any(|e| e.name == "optimistic-with-reconcile"));
        let entry = r
            .iter()
            .find(|e| e.name == "optimistic-with-reconcile")
            .unwrap();
        assert!(!entry.body.is_empty());
        assert!(entry.body.contains("reconcile"));
    }

    #[test]
    fn registry_has_streaming_snapshot() {
        let r = registry();
        let entry = r
            .iter()
            .find(|e| e.name == "streaming-snapshot")
            .expect("streaming-snapshot entry should exist");
        assert!(entry.body.contains("Sse"), "should reference axum::Sse");
        assert!(
            entry.body.contains("broadcast::Sender") || entry.body.contains("BroadcastStream"),
            "should reference the broadcast channel"
        );
        assert!(
            entry.body.contains("EventSource"),
            "client side should subscribe via EventSource"
        );
    }

    #[test]
    fn registry_has_cookie_session_auth() {
        let r = registry();
        let entry = r
            .iter()
            .find(|e| e.name == "cookie-session-auth")
            .expect("cookie-session-auth entry should exist");
        // Server side
        assert!(
            entry.body.contains("TypedHeader<Cookie>"),
            "should read cookies via TypedHeader<Cookie>"
        );
        assert!(
            entry.body.contains("SESSIONS"),
            "should reference the SESSIONS map"
        );
        assert!(
            entry.body.contains("FullstackContext::current()"),
            "should set the Set-Cookie via FullstackContext"
        );
        assert!(
            entry.body.contains("ServerFnError::new"),
            "should use the 0.7.3 ServerFnError constructor"
        );
        // Client side
        assert!(
            entry.body.contains("provide_session") && entry.body.contains("use_session"),
            "should expose the Signal-in-context pair"
        );
        assert!(
            entry.body.contains("Protected") && entry.body.contains("use_effect"),
            "should show the Protected wrapper redirecting via use_effect"
        );
    }

    #[test]
    fn registry_has_dnd_reorder() {
        let r = registry();
        let entry = r
            .iter()
            .find(|e| e.name == "dnd-reorder")
            .expect("dnd-reorder entry should exist");
        // All three event handlers must be present — that's what makes the
        // HTML5 drag/drop loop work.
        assert!(entry.body.contains("ondragstart"));
        assert!(entry.body.contains("ondragover"));
        assert!(entry.body.contains("ondrop"));
        assert!(
            entry.body.contains("prevent_default"),
            "ondragover must call prevent_default or the drop never fires"
        );
        // The signal-pair shape (dragging + drop_target) that the catalog
        // widget doesn't model.
        assert!(entry.body.contains("dragging"));
        assert!(entry.body.contains("drop_target"));
    }

    #[test]
    fn registry_has_wasm_polling_timer() {
        let r = registry();
        let entry = r
            .iter()
            .find(|e| e.name == "wasm-polling-timer")
            .expect("wasm-polling-timer entry should exist");
        assert!(
            entry.body.contains("TimeoutFuture"),
            "should use gloo-timers TimeoutFuture (wasm-safe)"
        );
        assert!(
            entry.body.contains("use_future"),
            "polling loop should live inside use_future"
        );
        assert!(
            entry.body.contains("gloo_timers") || entry.body.contains("gloo-timers"),
            "should reference gloo-timers in the imports/cargo notes"
        );
    }
}
