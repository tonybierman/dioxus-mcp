use dioxus::prelude::*;
use crate::model::card::Card;

// Shared-enum-validation shape: client-side const array of `(&str, &str)`
// pairs whose first-position strings match the server-side pattern set.
pub const COLUMNS: [(&str, &str); 3] =
    [("todo", "Todo"), ("doing", "Doing"), ("done", "Done")];

#[component]
pub fn BoardScreen() -> Element {
    rsx! {
        BoardBody {}
    }
}

#[component]
fn BoardBody() -> Element {
    let mut cards = use_signal(Vec::<Card>::new);
    // Optimistic-lock-gate signal: snapshot + bump + compare around a
    // reconciliation server fn. Also triggers `signal_used_as_fence`.
    let mut local_lock = use_signal(|| 0u32);
    let mut status = use_signal(|| None::<String>);
    // signal_drilled_2_levels: `dragging` is created here via `use_signal`
    // and forwarded BoardBody → Column → CardItem. BoardBody has no prop
    // named `dragging` so prop_drill can't see the first hop; the lint's
    // origin scanner has to synthesize it.
    let dragging = use_signal(|| None::<String>);

    use_future(move || async move {
        loop {
            let lock = local_lock();
            match crate::server::fetch_board().await {
                Ok(server_cards) => {
                    if local_lock() == lock {
                        cards.set(server_cards);
                        status.set(None);
                    }
                }
                Err(e) => {
                    status.set(Some(format!("Sync issue: {e}")));
                }
            }
            // polling_future_no_backoff: constant 2s interval, no jitter,
            // no error-path delay extension. iter03's exact shape.
            gloo_timers::future::TimeoutFuture::new(2000).await;
        }
    });
    // empty_async_error_arm: presence heartbeat that swallows errors —
    // iter03's `ping_presence` loop, board_screen.rs:55-57.
    use_future(move || async move {
        loop {
            match crate::server::ping_presence().await {
                Ok(_v) => {}
                Err(_) => {}
            }
            gloo_timers::future::TimeoutFuture::new(3000).await;
        }
    });

    let submit_card = move || {
        // Magic-id-prefix shape: `format!("tmp-{}", …)` forging an ID.
        let optimistic = Card {
            id: format!("tmp-{}", 1),
            title: "x".into(),
            column: "todo".into(),
            position: i32::MAX,
            author: "you".into(),
        };
        cards.with_mut(|list| list.push(optimistic.clone()));
        local_lock += 1;
        spawn(async move {
            let _ = crate::server::create_card("x".into(), "todo".into()).await;
            local_lock += 1;
        });
    };

    let delete_card_action = move |id: String| {
        cards.with_mut(|list| list.retain(|c| c.id != id));
        local_lock += 1;
        spawn(async move {
            let _ = crate::server::delete_card(id).await;
            local_lock += 1;
        });
    };

    let move_card = move |(id, column, position): (String, String, i32)| {
        cards.with_mut(|list| {
            if let Some(c) = list.iter_mut().find(|c| c.id == id) {
                c.column = column.clone();
                c.position = position;
            }
            normalize_positions(list);
        });
        local_lock += 1;
        spawn(async move {
            let _ = crate::server::move_card(id, column, position).await;
            local_lock += 1;
        });
    };

    rsx! {
        if let Some(msg) = status() {
            div { class: "banner", "{msg}" }
        }
        button { onclick: move |_| submit_card(), "Add" }
        for (col_id, col_label) in COLUMNS.iter() {
            Column {
                key: "{col_id}",
                id: (*col_id).to_string(),
                label: (*col_label).to_string(),
                cards: column_cards(&cards.read(), col_id),
                dragging: dragging,
                on_move: move_card,
                on_delete: delete_card_action,
            }
        }
    }
}

// vec_or_owned_prop_passthrough: `Vec<Card>` and `Card` owned props,
// callers (BoardBody, Column) have reactive writes.
#[component]
fn Column(
    id: String,
    label: String,
    cards: Vec<Card>,
    dragging: Signal<Option<String>>,
    on_move: EventHandler<(String, String, i32)>,
    on_delete: EventHandler<String>,
) -> Element {
    let label_for_render = label.clone();
    rsx! {
        section { class: "column",
            header { h2 { "{label_for_render}" } }
            for (idx, card) in cards.iter().cloned().enumerate() {
                CardItem {
                    key: "{card.id}",
                    card: card,
                    idx: idx as i32,
                    dragging: dragging,
                    on_move: on_move,
                    on_delete: on_delete,
                }
            }
        }
    }
}

#[component]
fn CardItem(
    card: Card,
    idx: i32,
    dragging: Signal<Option<String>>,
    on_move: EventHandler<(String, String, i32)>,
    on_delete: EventHandler<String>,
) -> Element {
    // Magic-id-prefix read site: `.id.starts_with("tmp-")`.
    let is_pending = card.id.starts_with("tmp-");
    let card_id = card.id.clone();
    let card_id_del = card_id.clone();
    let _ = (idx, on_move, is_pending, dragging);
    rsx! {
        div {
            class: if is_pending { "card pending" } else { "card" },
            div { class: "card-body", "{card.title}" }
            div { class: "card-foot",
                span { class: "author", "{card.author}" }
                button {
                    onclick: move |_| on_delete.call(card_id_del.clone()),
                    "x"
                }
            }
        }
    }
}

fn column_cards(all: &[Card], col: &str) -> Vec<Card> {
    let mut filtered: Vec<Card> =
        all.iter().filter(|c| c.column == col).cloned().collect();
    filtered.sort_by_key(|c| c.position);
    filtered
}

// duplicate_helper_across_client_and_server: this fn body is byte-
// identical to the one in `src/server/state.rs`.
fn normalize_positions(list: &mut Vec<Card>) {
    for col in ["todo", "doing", "done"] {
        let mut idxs: Vec<usize> = list
            .iter()
            .enumerate()
            .filter(|(_, c)| c.column == col)
            .map(|(i, _)| i)
            .collect();
        idxs.sort_by_key(|i| list[*i].position);
        for (rank, i) in idxs.into_iter().enumerate() {
            list[i].position = rank as i32;
        }
    }
}
