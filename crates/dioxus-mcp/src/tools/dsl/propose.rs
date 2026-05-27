//! Human-in-the-loop scaffold tools (M6): `propose_scaffold`, `list_proposals`,
//! `resolve_proposal`, `check_proposal`. See [`crate::proposal`] for the store.
//!
//! Flow: an agent calls `propose_scaffold` with a DSL doc → it's previewed
//! (dry-run) and parked → the call blocks (bounded) for a human decision. A
//! human in the cockpit `list_proposals` → edits → `resolve_proposal`. Approve
//! runs `execute_code(dry_run:false)` on the *edited* doc; that result is
//! delivered back to the blocked agent (or its later `check_proposal` poll). The
//! agent is always told, via `executed_code`/`edited`/`note`, exactly what ran.

use std::sync::Arc;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ExecuteCodeParams, execute_code};
use crate::proposal::{ProposalEntry, ProposalStatus, now_secs};
use crate::state::State;

const DEFAULT_WAIT_SECS: u64 = 300;
/// Cap the block under Claude Code's default 600s tool-call timeout.
const MAX_WAIT_SECS: u64 = 540;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ProposeScaffoldParams {
    /// YAML DSL doc (same vocabulary as execute_code; see get_dsl_spec).
    pub code: String,
    pub project_root: Option<String>,
    /// Max seconds to block for a human decision before returning a pending
    /// handle to poll. Default 300, capped at 540 (under the client timeout).
    #[serde(default)]
    pub wait_secs: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ListProposalsParams {
    /// Include already-resolved proposals (default: pending only).
    #[serde(default)]
    pub include_resolved: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ResolveProposalParams {
    pub proposal_id: String,
    /// "approve" or "reject".
    pub action: String,
    /// Round-trip edit: the DSL to run instead of the original (approve only).
    pub edited_code: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct CheckProposalParams {
    pub proposal_id: String,
}

/// Build the agent-facing JSON for a terminal (or pending) proposal, always
/// stating which code actually ran.
fn terminal_payload(id: &str, entry: &ProposalEntry) -> Value {
    match &entry.status {
        ProposalStatus::Applied {
            result,
            edited,
            code,
        } => json!({
            "status": "applied",
            "proposal_id": id,
            "human_action": "approved",
            "edited": edited,
            "executed_code": code,
            "result": result,
            "note": if *edited {
                "Human approved WITH EDITS. `executed_code` is the DSL actually written to disk and differs from your proposal — treat it as ground truth."
            } else {
                "Human approved as proposed. `executed_code` is what was written."
            },
        }),
        ProposalStatus::Failed {
            error,
            edited,
            code,
        } => json!({
            "status": "failed",
            "proposal_id": id,
            "human_action": "approved",
            "edited": edited,
            "executed_code": code,
            "error": error,
            "note": "Human approved but the DSL failed to apply; nothing was written. Re-propose a corrected doc.",
        }),
        ProposalStatus::Rejected { reason } => json!({
            "status": "rejected",
            "proposal_id": id,
            "human_action": "rejected",
            "reason": reason,
            "note": "Human rejected the proposal; nothing was written.",
        }),
        ProposalStatus::Pending => json!({ "status": "pending", "proposal_id": id }),
    }
}

pub async fn propose_scaffold(
    state: &Arc<State>,
    p: ProposeScaffoldParams,
) -> Result<Value, String> {
    state.proposals.gc().await;

    // Preview via dry-run. Unparseable YAML errors here — no proposal parked.
    let preview = execute_code(
        state,
        ExecuteCodeParams {
            code: p.code.clone(),
            project_root: p.project_root.clone(),
            if_missing: true,
            dry_run: true,
            cargo_check: false,
            format_after: false,
        },
    )
    .await?;
    let preview_json = serde_json::to_value(&preview).map_err(|e| e.to_string())?;

    let id = state.proposals.mint_id();
    state
        .proposals
        .insert(ProposalEntry {
            id: id.clone(),
            code: p.code.clone(),
            project_root: p.project_root.clone(),
            preview: preview_json.clone(),
            status: ProposalStatus::Pending,
            created_at: now_secs(),
            resolved_at: None,
        })
        .await;

    let wait = p.wait_secs.unwrap_or(DEFAULT_WAIT_SECS).min(MAX_WAIT_SECS);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(wait);

    loop {
        match state.proposals.get(&id).await {
            None => return Err("proposal vanished".into()),
            Some(e) if e.is_terminal() => return Ok(terminal_payload(&id, &e)),
            Some(_) => {}
        }
        // Register interest, then re-check, to close the lost-wakeup window
        // (Notify::notified registers at creation).
        let notified = state.proposals.notify().notified();
        if let Some(e) = state.proposals.get(&id).await
            && e.is_terminal()
        {
            return Ok(terminal_payload(&id, &e));
        }
        tokio::select! {
            _ = notified => continue,
            _ = tokio::time::sleep_until(deadline) => {
                return Ok(json!({
                    "status": "pending",
                    "proposal_id": id,
                    "preview": preview_json,
                    "note": "Awaiting human decision in the playground cockpit. Call check_proposal with this proposal_id to get the outcome.",
                }));
            }
        }
    }
}

pub async fn list_proposals(state: &Arc<State>, p: ListProposalsParams) -> Result<Value, String> {
    state.proposals.gc().await;
    let proposals = state.proposals.snapshot(p.include_resolved).await;
    Ok(json!({ "proposals": proposals }))
}

pub async fn resolve_proposal(
    state: &Arc<State>,
    p: ResolveProposalParams,
) -> Result<Value, String> {
    let entry = match state.proposals.get(&p.proposal_id).await {
        Some(e) => e,
        None => return Ok(json!({ "ok": false, "error": "unknown proposal_id" })),
    };
    if entry.is_terminal() {
        return Ok(json!({ "ok": false, "error": "already resolved", "status": entry.status }));
    }

    match p.action.as_str() {
        "reject" => {
            state
                .proposals
                .resolve(
                    &p.proposal_id,
                    ProposalStatus::Rejected {
                        reason: p.reason.clone(),
                    },
                )
                .await?;
            Ok(json!({ "ok": true, "proposal_id": p.proposal_id, "status": "rejected" }))
        }
        "approve" => {
            let final_code = p.edited_code.clone().unwrap_or_else(|| entry.code.clone());
            let edited = p.edited_code.as_deref().is_some_and(|c| c != entry.code);

            // Run the real apply WITHOUT holding the proposal lock.
            let outcome = execute_code(
                state,
                ExecuteCodeParams {
                    code: final_code.clone(),
                    project_root: entry.project_root.clone(),
                    if_missing: true,
                    dry_run: false,
                    cargo_check: false,
                    format_after: false,
                },
            )
            .await;

            let status = match &outcome {
                Ok(result) => ProposalStatus::Applied {
                    result: serde_json::to_value(result).map_err(|e| e.to_string())?,
                    edited,
                    code: final_code.clone(),
                },
                Err(e) => ProposalStatus::Failed {
                    error: e.clone(),
                    edited,
                    code: final_code.clone(),
                },
            };
            // Ignore an already-resolved race (a double-click); if_missing made
            // the apply itself idempotent.
            let _ = state.proposals.resolve(&p.proposal_id, status).await;

            Ok(match outcome {
                Ok(result) => json!({
                    "ok": true, "proposal_id": p.proposal_id, "status": "applied",
                    "edited": edited, "result": serde_json::to_value(&result).map_err(|e| e.to_string())?,
                }),
                Err(e) => json!({
                    "ok": true, "proposal_id": p.proposal_id, "status": "failed",
                    "edited": edited, "error": e,
                }),
            })
        }
        other => Err(format!(
            "unknown action {other:?}; expected \"approve\" or \"reject\""
        )),
    }
}

pub async fn check_proposal(state: &Arc<State>, p: CheckProposalParams) -> Result<Value, String> {
    match state.proposals.get(&p.proposal_id).await {
        None => Ok(json!({
            "status": "unknown",
            "proposal_id": p.proposal_id,
            "note": "No such proposal (it may have been garbage-collected after resolution).",
        })),
        Some(e) if e.is_terminal() => Ok(terminal_payload(&p.proposal_id, &e)),
        Some(e) => {
            Ok(json!({ "status": "pending", "proposal_id": p.proposal_id, "preview": e.preview }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(status: ProposalStatus) -> ProposalEntry {
        ProposalEntry {
            id: "p-1".into(),
            code: "orig".into(),
            project_root: None,
            preview: Value::Null,
            status,
            created_at: 0,
            resolved_at: Some(0),
        }
    }

    #[test]
    fn applied_payload_states_executed_code_is_truth() {
        let v = terminal_payload(
            "p-1",
            &entry(ProposalStatus::Applied {
                result: json!({ "files_created": [] }),
                edited: true,
                code: "edited".into(),
            }),
        );
        assert_eq!(v["status"], "applied");
        assert_eq!(v["edited"], true);
        assert_eq!(v["executed_code"], "edited");
        assert!(v["note"].as_str().unwrap().contains("ground truth"));
    }

    #[test]
    fn rejected_payload_has_no_writes() {
        let v = terminal_payload("p-1", &entry(ProposalStatus::Rejected { reason: None }));
        assert_eq!(v["status"], "rejected");
        assert!(v["note"].as_str().unwrap().contains("nothing was written"));
    }
}
