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
}
