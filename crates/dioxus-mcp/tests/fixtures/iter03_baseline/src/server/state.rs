#![cfg(feature = "server")]

use crate::model::card::Card;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

// presence_map: narrow-eviction shape — SESSIONS only sheds via
// `logout_user`. Triggers `presence_map_narrow_eviction`.
pub static SESSIONS: Lazy<Mutex<HashMap<String, String>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

// presence_map: no eviction at all on PRESENCE. Triggers
// `presence_map_unbounded`.
pub static PRESENCE: Lazy<Mutex<HashMap<String, (Instant, String)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub const PRESENCE_TTL: Duration = Duration::from_secs(8);

pub fn user_from_cookies(
    cookies: &axum_extra::TypedHeader<axum_extra::headers::Cookie>,
) -> Option<String> {
    let sid = cookies.get("sid")?;
    SESSIONS.lock().ok()?.get(sid).cloned()
}

// duplicate_helper_across_client_and_server: body shape identical to the
// `normalize_positions` in `components/board_screen.rs`. Mirrors the real
// iter03 where the param is named `board` on the server side and `list`
// on the client side — the matcher must rewrite the param to a positional
// placeholder before comparing, otherwise this case goes silent.
pub fn normalize_positions(board: &mut Vec<Card>) {
    for col in ["todo", "doing", "done"] {
        let mut idxs: Vec<usize> = board
            .iter()
            .enumerate()
            .filter(|(_, c)| c.column == col)
            .map(|(i, _)| i)
            .collect();
        idxs.sort_by_key(|i| board[*i].position);
        for (rank, i) in idxs.into_iter().enumerate() {
            board[i].position = rank as i32;
        }
    }
}
