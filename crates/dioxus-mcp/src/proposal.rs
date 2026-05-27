//! Human-in-the-loop scaffold proposals (M6).
//!
//! An agent calls `propose_scaffold` instead of writing files; the proposal is
//! parked here. A human (via the dx-playground cockpit) sees it, optionally
//! edits the DSL, and approves/rejects via `resolve_proposal`. The approved —
//! possibly edited — doc is what actually runs, and the result is delivered back
//! to the (blocked or polling) agent. The store lives in [`State`](crate::state::State)
//! and is shared across all clients of one server process.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, Notify};

/// Terminal proposals are dropped by lazy GC after this long.
pub const PROPOSAL_TTL_SECS: u64 = 3600;
/// Hard cap on stored proposals (oldest terminal evicted beyond this).
pub const MAX_PROPOSALS: usize = 256;

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Lifecycle of a proposal. Previews/results are stored as `serde_json::Value`
/// because `ScaffoldResult` is `Serialize`-only server-side (no `Clone`), and
/// the entry must be `Clone` to snapshot out from under the lock.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProposalStatus {
    Pending,
    Rejected {
        reason: Option<String>,
    },
    /// `execute_code(dry_run:false)` succeeded. `code` is what actually ran
    /// (original or human-edited); `edited` flags whether it differs.
    Applied {
        result: Value,
        edited: bool,
        code: String,
    },
    /// The (possibly edited) DSL failed to apply; nothing was written.
    Failed {
        error: String,
        edited: bool,
        code: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalEntry {
    pub id: String,
    pub code: String,
    pub project_root: Option<String>,
    /// The dry-run `ScaffoldResult` (as JSON) — the preview shown in the cockpit.
    pub preview: Value,
    pub status: ProposalStatus,
    pub created_at: u64,
    pub resolved_at: Option<u64>,
}

impl ProposalEntry {
    pub fn is_terminal(&self) -> bool {
        !matches!(self.status, ProposalStatus::Pending)
    }
}

/// Shared proposal store. One `Notify` wakes all blocked `propose_scaffold`
/// awaiters on any resolution; each re-checks its own id (volume is human-paced,
/// so spurious wakeups are negligible).
#[derive(Default)]
pub struct Proposals {
    entries: Mutex<HashMap<String, ProposalEntry>>,
    notify: Notify,
    seq: AtomicU64,
    /// When set, the store is persisted here on every mutation and loaded on
    /// construct, so proposals survive a server respawn (e.g. an embedded
    /// cockpit dying with its Claude Code session). `None` = in-memory only.
    path: Option<PathBuf>,
}

impl Proposals {
    /// Build a store persisted at `path`, loading any existing proposals.
    pub fn with_path(path: PathBuf) -> Self {
        let entries = Self::load(&path).unwrap_or_default();
        Proposals {
            entries: Mutex::new(entries),
            notify: Notify::new(),
            seq: AtomicU64::new(0),
            path: Some(path),
        }
    }

    fn load(path: &Path) -> Option<HashMap<String, ProposalEntry>> {
        let bytes = std::fs::read(path).ok()?;
        match serde_json::from_slice(&bytes) {
            Ok(map) => Some(map),
            Err(e) => {
                tracing::debug!(error=%e, path=%path.display(), "proposal persist: load failed");
                None
            }
        }
    }

    /// Best-effort write of the whole store. Called under the entries lock after
    /// a mutation; no-op when not persisting. Failures are logged at debug and
    /// never propagated — persistence must not break the gate.
    fn save(&self, entries: &HashMap<String, ProposalEntry>) {
        let Some(path) = &self.path else {
            return;
        };
        let json = match serde_json::to_string_pretty(entries) {
            Ok(j) => j,
            Err(e) => {
                tracing::debug!(error=%e, "proposal persist: serialize failed");
                return;
            }
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(path, json) {
            tracing::debug!(error=%e, path=%path.display(), "proposal persist: write failed");
        }
    }

    /// Monotonic, human-readable id (no uuid dependency).
    pub fn mint_id(&self) -> String {
        format!(
            "p-{}-{}",
            now_secs(),
            self.seq.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// The shared notifier — awaiters call `.notified()` on it.
    pub fn notify(&self) -> &Notify {
        &self.notify
    }

    /// Drop terminal entries older than the TTL and enforce the hard cap by
    /// evicting the oldest terminal entries. Run on the propose/list entry paths
    /// (no background task needed).
    pub async fn gc(&self) {
        let now = now_secs();
        let mut g = self.entries.lock().await;
        g.retain(|_, e| match e.resolved_at {
            Some(t) => now.saturating_sub(t) < PROPOSAL_TTL_SECS,
            None => true,
        });
        if g.len() > MAX_PROPOSALS {
            let mut terminal: Vec<(String, u64)> = g
                .iter()
                .filter(|(_, e)| e.is_terminal())
                .map(|(k, e)| (k.clone(), e.resolved_at.unwrap_or(e.created_at)))
                .collect();
            terminal.sort_by_key(|(_, t)| *t);
            let excess = g.len() - MAX_PROPOSALS;
            for (k, _) in terminal.into_iter().take(excess) {
                g.remove(&k);
            }
        }
    }

    pub async fn insert(&self, entry: ProposalEntry) {
        let mut g = self.entries.lock().await;
        g.insert(entry.id.clone(), entry);
        self.save(&g);
    }

    pub async fn get(&self, id: &str) -> Option<ProposalEntry> {
        self.entries.lock().await.get(id).cloned()
    }

    /// Snapshot of proposals sorted by creation time. Pending only unless
    /// `include_resolved`.
    pub async fn snapshot(&self, include_resolved: bool) -> Vec<ProposalEntry> {
        let g = self.entries.lock().await;
        let mut v: Vec<ProposalEntry> = g
            .values()
            .filter(|e| include_resolved || matches!(e.status, ProposalStatus::Pending))
            .cloned()
            .collect();
        v.sort_by_key(|e| e.created_at);
        v
    }

    /// Move a pending entry to a terminal status and wake awaiters. Errors if
    /// the id is unknown or already resolved (idempotent guard).
    pub async fn resolve(&self, id: &str, status: ProposalStatus) -> Result<ProposalEntry, String> {
        let updated = {
            let mut g = self.entries.lock().await;
            let entry = g.get_mut(id).ok_or("unknown proposal_id")?;
            if entry.is_terminal() {
                return Err("proposal already resolved".into());
            }
            entry.status = status;
            entry.resolved_at = Some(now_secs());
            let updated = entry.clone();
            self.save(&g);
            updated
        };
        self.notify.notify_waiters();
        Ok(updated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(id: &str) -> ProposalEntry {
        ProposalEntry {
            id: id.into(),
            code: "code".into(),
            project_root: None,
            preview: Value::Null,
            status: ProposalStatus::Pending,
            created_at: now_secs(),
            resolved_at: None,
        }
    }

    #[tokio::test]
    async fn resolve_is_idempotent_and_guards_unknown() {
        let p = Proposals::default();
        p.insert(pending("a")).await;

        let r = p
            .resolve(
                "a",
                ProposalStatus::Applied {
                    result: Value::Null,
                    edited: true,
                    code: "edited".into(),
                },
            )
            .await
            .expect("first resolve ok");
        assert!(matches!(
            r.status,
            ProposalStatus::Applied { edited: true, .. }
        ));
        assert!(r.resolved_at.is_some());

        // Re-resolving a terminal entry is rejected.
        assert!(
            p.resolve("a", ProposalStatus::Rejected { reason: None })
                .await
                .is_err()
        );
        // Unknown id is rejected.
        assert!(
            p.resolve("missing", ProposalStatus::Rejected { reason: None })
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn snapshot_filters_pending_by_default() {
        let p = Proposals::default();
        p.insert(pending("a")).await;
        p.insert(pending("b")).await;
        p.resolve("b", ProposalStatus::Rejected { reason: None })
            .await
            .unwrap();

        assert_eq!(p.snapshot(false).await.len(), 1, "pending only");
        assert_eq!(p.snapshot(true).await.len(), 2, "include resolved");
    }

    #[test]
    fn ids_are_unique() {
        let p = Proposals::default();
        assert_ne!(p.mint_id(), p.mint_id());
    }

    #[tokio::test]
    async fn proposals_survive_reload_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/proposals.json");

        {
            let p = Proposals::with_path(path.clone());
            p.insert(pending("keep")).await;
        }
        // A fresh store at the same path reloads the persisted proposal.
        let reloaded = Proposals::with_path(path);
        let entry = reloaded.get("keep").await.expect("entry restored");
        assert!(matches!(entry.status, ProposalStatus::Pending));
    }
}
