//! `/api/waves/:wave_id/proposals`, `/api/proposals/:id/{accept,reject}` —
//! Issue #955 §5.5/§5.6 (PR-b): human adjudication of the ④ proposal
//! channel.
//!
//! **User-only.** A plugin proposes and may withdraw its own pending
//! proposal (`neige.proposal.withdraw`); accepting, rejecting and the
//! `stale` verdict are the human's alone. That is enforced twice, the
//! §3.2 defence-in-depth pattern: here at the route (`actor != "user"` ⇒
//! 403) and again inside the write transaction by `role_gate`'s hard
//! `ProposalResolved{accepted|rejected|stale}` User-only clause — so a
//! future non-REST caller cannot route around it.
//!
//! Shape mirrors `routes::cards::ratify_card` (the codebase's other
//! agent-proposes/human-decides gate): user check → row lookup → in-tx
//! pending re-check → 409 on a lost double-resolve race. The one
//! structural difference is `accept`, which per §5.6 must commit the
//! decision **and** the report change in ONE transaction, so no crash
//! window can leave a proposal "resolved but not written" or "written but
//! still pending".
//!
//! Three verdict outcomes come out of `accept`:
//!
//!   * every anchoring check passes ⇒ `accepted` + the report's one
//!     `CardUpdated`/`WaveReportEdited(author = plugin)` pair, all in the
//!     accept transaction;
//!   * an anchor moved under the proposal (rev mismatch, vanished block,
//!     moved `base_doc_heads`) ⇒ `stale`, appended by **that same accept
//!     transaction** with no report change and then committed (§5.6:
//!     `stale` is a decision, not a third event and not a persistent
//!     flag). Recording it in a second transaction would leave a window
//!     in which a concurrent reject/withdraw commits first and an
//!     already-authoritatively-stale accept comes back 409 — the verdict
//!     has to land where it was determined;
//!   * the proposal is structurally impossible (`400`) ⇒ nothing is
//!     recorded and it **stays pending**. Per §5.2.1's error split such a
//!     proposal can never succeed against any snapshot, so calling it
//!     `stale` would tell the plugin "re-read and retry" about something
//!     no retry can fix. Surfacing it keeps the bug visible; the user can
//!     still `reject`.

use crate::actor::Actor;
use crate::db::sqlite::{ProposalRow, proposal_get_tx};
use crate::db::write_with_actor_events_typed;
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use crate::model::Wave;
use crate::state::{AppState, RouteState};
use crate::wave_report::{
    AcceptFailure, ProposalAcceptCtx, ProposalAcceptOutcome, persist_report_proposal_accept,
    resolve_report_for_wave,
};

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{get, post},
};
use calm_types::proposal::{ProposalDecision, ProposalOp};
use serde::Serialize;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/waves/{wave_id}/proposals", get(list_wave_proposals))
        .route("/api/proposals/{id}/accept", post(accept_proposal))
        .route("/api/proposals/{id}/reject", post(reject_proposal))
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// One pending proposal, as the adjudication UI needs it.
///
/// `plugin_id` is rendered verbatim even when the plugin has been
/// uninstalled (§5.5: a pending proposal lives in the event log, not in a
/// plugin process — it stays adjudicable and the UI shows the raw id).
/// `ops` is the typed `ProposalOp` list so PR-c can render before/after
/// blocks without re-parsing JSON text.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PendingProposal {
    pub proposal_id: String,
    pub wave_id: String,
    /// Submitting plugin's manifest id — may name an uninstalled plugin.
    pub plugin_id: String,
    pub subject_kind: String,
    /// Opaque baseline token the proposal was written against.
    pub base_doc_heads: String,
    pub ops: Vec<ProposalOp>,
    /// Human-facing rationale the plugin supplied.
    pub note: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PendingProposalsResponse {
    pub proposals: Vec<PendingProposal>,
}

/// Adjudication result. `decision` is the verdict actually recorded —
/// notably `stale` when the user pressed accept but the anchors had moved,
/// which is a successful adjudication, not an error.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ResolveProposalResponse {
    pub proposal_id: String,
    pub wave_id: String,
    pub decision: ProposalDecision,
    /// Present for `stale`: which anchoring check failed, so the UI can
    /// explain why the change was not applied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------
// GET /api/waves/:wave_id/proposals
// ---------------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/waves/{wave_id}/proposals",
    tag = "proposals",
    params(("wave_id" = String, Path, description = "Wave id")),
    responses(
        (status = 200, description = "Pending proposals for the wave, submission order", body = PendingProposalsResponse),
        (status = 403, description = "Actor is not the authenticated user", body = ErrorBody),
        (status = 404, description = "Wave not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_wave_proposals(
    State(s): State<RouteState>,
    actor: Actor,
    Path(wave_id): Path<String>,
) -> Result<Json<PendingProposalsResponse>> {
    require_user(&actor, "list proposals")?;
    // 404 on an unknown wave rather than an empty list: an empty list is a
    // meaningful answer the UI renders, so it must not double as "no such
    // wave".
    s.repo
        .wave_get(&wave_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {wave_id}")))?;
    let rows = s.repo.proposals_pending_by_wave(&wave_id).await?;
    let proposals = rows
        .into_iter()
        .map(pending_from_row)
        .collect::<Result<Vec<_>>>()?;
    Ok(Json(PendingProposalsResponse { proposals }))
}

/// Decode one projection row into its wire shape. The projection stores
/// `ops` as the event's JSON text; a row that will not decode is corrupt
/// Tier-A data, so it fails loudly instead of silently vanishing from the
/// list (a proposal the user cannot see is a proposal they cannot reject).
fn pending_from_row(row: ProposalRow) -> Result<PendingProposal> {
    let ops: Vec<ProposalOp> = serde_json::from_str(&row.ops).map_err(|e| {
        CalmError::Internal(format!(
            "proposals: malformed ops on proposal {}: {e}",
            row.proposal_id
        ))
    })?;
    Ok(PendingProposal {
        proposal_id: row.proposal_id,
        wave_id: row.wave_id,
        plugin_id: row.plugin_id,
        subject_kind: row.subject_kind,
        base_doc_heads: row.base_doc_heads,
        ops,
        note: row.note,
        created_at: row.created_at,
    })
}

// ---------------------------------------------------------------------------
// POST /api/proposals/:id/accept
// ---------------------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/api/proposals/{id}/accept",
    tag = "proposals",
    params(("id" = String, Path, description = "Proposal id")),
    responses(
        (status = 200, description = "Proposal resolved — `accepted` (report changed) or `stale` (anchors moved, report untouched)", body = ResolveProposalResponse),
        (status = 400, description = "Proposal is structurally invalid; it stays pending", body = ErrorBody),
        (status = 403, description = "Actor is not the authenticated user", body = ErrorBody),
        (status = 404, description = "Proposal or its wave not found", body = ErrorBody),
        (status = 409, description = "Proposal is no longer pending (already resolved)", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn accept_proposal(
    State(s): State<RouteState>,
    actor: Actor,
    Path(id): Path<String>,
) -> Result<Json<ResolveProposalResponse>> {
    require_user(&actor, "accept a proposal")?;
    let (row, wave) = load_pending(&s, &id).await?;
    let accept = ProposalAcceptCtx {
        proposal_id: row.proposal_id.clone(),
        plugin_id: row.plugin_id.clone(),
    };
    // §5.5 — adjudication checks FACTS, not the plugin's permissions: a
    // terminal wave can no longer take a report change, so an accept
    // attempt lands `stale`. (PR-a's projection already drops a terminal
    // wave's pending rows in the terminalizing transaction, so in practice
    // `load_pending` 404s first; this arm covers the pre-projection /
    // replayed-row case and keeps the route faithful to §5.5 rather than
    // depending on the projection's cleanup for correctness.)
    if wave.lifecycle.is_terminal() {
        return record_decision(
            &s,
            &row,
            &wave,
            ProposalDecision::Stale,
            Some(format!(
                "wave {} is terminal (`{}`) — its report can no longer be changed",
                wave.id,
                wave.lifecycle.as_db_str()
            )),
        )
        .await;
    }
    let ops: Vec<ProposalOp> = serde_json::from_str(&row.ops).map_err(|e| {
        CalmError::Internal(format!(
            "proposals: malformed ops on proposal {}: {e}",
            row.proposal_id
        ))
    })?;
    let (wave_row, report_card, payload) =
        resolve_report_for_wave(s.repo.as_ref(), wave.id.as_str()).await?;
    match persist_report_proposal_accept(
        s.repo.as_ref(),
        &s.events,
        &s.write,
        wave_row,
        report_card,
        payload,
        row.base_doc_heads.clone(),
        ops,
        accept,
    )
    .await
    {
        Ok(ProposalAcceptOutcome::Accepted(_card)) => Ok(Json(ResolveProposalResponse {
            proposal_id: row.proposal_id,
            wave_id: row.wave_id,
            decision: ProposalDecision::Accepted,
            reason: None,
        })),
        // Anchors had moved. The accept transaction ITSELF recorded the
        // `stale` verdict and committed with the report untouched — there
        // is nothing left for this route to write, which is the whole
        // point: no window exists in which a concurrent reject/withdraw
        // could overtake an already-determined stale verdict.
        Ok(ProposalAcceptOutcome::Stale(reason)) => Ok(Json(ResolveProposalResponse {
            proposal_id: row.proposal_id,
            wave_id: row.wave_id,
            decision: ProposalDecision::Stale,
            reason: Some(reason),
        })),
        // Someone else resolved it between the pre-flight and the tx.
        Err(AcceptFailure::NotPending) => Err(CalmError::Conflict(format!(
            "proposal {} is no longer pending",
            row.proposal_id
        ))),
        Err(AcceptFailure::Other(e)) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// POST /api/proposals/:id/reject
// ---------------------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/api/proposals/{id}/reject",
    tag = "proposals",
    params(("id" = String, Path, description = "Proposal id")),
    responses(
        (status = 200, description = "Proposal rejected; report untouched", body = ResolveProposalResponse),
        (status = 403, description = "Actor is not the authenticated user", body = ErrorBody),
        (status = 404, description = "Proposal or its wave not found", body = ErrorBody),
        (status = 409, description = "Proposal is no longer pending (already resolved)", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn reject_proposal(
    State(s): State<RouteState>,
    actor: Actor,
    Path(id): Path<String>,
) -> Result<Json<ResolveProposalResponse>> {
    require_user(&actor, "reject a proposal")?;
    let (row, wave) = load_pending(&s, &id).await?;
    record_decision(&s, &row, &wave, ProposalDecision::Rejected, None).await
}

// ---------------------------------------------------------------------------
// Shared
// ---------------------------------------------------------------------------

/// Adjudication is the human's alone (§5.1/§5.4). The in-tx `role_gate`
/// clause is the authority; this is the friendly 403.
fn require_user(actor: &Actor, what: &str) -> Result<()> {
    if actor.as_str() != "user" {
        return Err(CalmError::Forbidden(format!(
            "only the authenticated user may {what}"
        )));
    }
    Ok(())
}

/// Pre-flight: the proposal exists, is still pending, and its wave still
/// exists. Shapes 404/409 for the client; every handler re-checks
/// "pending" inside its own write transaction, which is the authority.
async fn load_pending(s: &RouteState, id: &str) -> Result<(ProposalRow, Wave)> {
    let row = s
        .repo
        .proposal_get(id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("proposal {id}")))?;
    if row.status != "pending" {
        return Err(CalmError::Conflict(format!(
            "proposal {id} is already resolved (`{}`)",
            row.status
        )));
    }
    // §5.5 — a deleted wave is a 404: there is nothing left to change.
    let wave =
        s.repo.wave_get(&row.wave_id).await?.ok_or_else(|| {
            CalmError::NotFound(format!("wave {} for proposal {id}", row.wave_id))
        })?;
    Ok((row, wave))
}

/// Record a side-effect-free decision (`rejected` / `stale`) in one write
/// transaction, with the in-tx pending re-check that makes a double
/// resolve a 409.
async fn record_decision(
    s: &RouteState,
    row: &ProposalRow,
    wave: &Wave,
    decision: ProposalDecision,
    reason: Option<String>,
) -> Result<Json<ResolveProposalResponse>> {
    let scope = EventScope::Wave {
        wave: wave.id.clone(),
        cove: wave.cove_id.clone(),
    };
    let proposal_id = row.proposal_id.clone();
    let wave_id = wave.id.clone();
    write_with_actor_events_typed::<(), _>(s.repo.as_ref(), None, &s.events, &s.write, move |tx| {
        let scope = scope.clone();
        let proposal_id = proposal_id.clone();
        let wave_id = wave_id.clone();
        Box::pin(async move {
            let current = proposal_get_tx(tx, &proposal_id)
                .await?
                .ok_or_else(|| CalmError::NotFound(format!("proposal {proposal_id}")))?;
            if current.status != "pending" {
                return Err(CalmError::Conflict(format!(
                    "proposal {proposal_id} is no longer pending (`{}`)",
                    current.status
                )));
            }
            // The payload's `plugin_id` is the SUBMITTER (role_gate reads
            // it for the withdrawn-ownership clause), so it must come from
            // the stored row, never from the request.
            let event = Event::ProposalResolved {
                wave_id,
                proposal_id,
                plugin_id: current.plugin_id,
                decision,
            };
            Ok(((), vec![(ActorId::User, scope, event)]))
        })
    })
    .await?;
    Ok(Json(ResolveProposalResponse {
        proposal_id: row.proposal_id.clone(),
        wave_id: row.wave_id.clone(),
        decision,
        reason,
    }))
}
