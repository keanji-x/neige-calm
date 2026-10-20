//! #2774: a person starts a new, blank Planner conversation in the same open Track. The card
//! stays the Planner; a new session on a new thread becomes current in the restart's own start
//! transaction, which also writes the conversation floor. The previous conversation is read-only
//! history and no longer receives wakes.
use axum::{Extension, Router, extract::State, http::HeaderMap, routing::post};
use calm_types::planner_conversation::{PlannerConversation, PlannerConversationStarted};
use calm_types::worker::WorkerSessionId;
use serde_json::{Value, json};

use super::conversation_read::ReadScope;
use crate::actor::Actor;
use crate::auth::Principal;
use crate::error::{CalmError, ErrorBody, Result};
use crate::extract::{Json, Path};
use crate::harness::backend::factory::{PlannerFactory, ReadinessFor};
use crate::harness::profile::{HarnessProfile, PlannerBinding};
use crate::harness::{HarnessPhaseTag, HarnessSnapshot};
use crate::model::{Card, CardRole, new_id};
use crate::operation::OperationKey;
use crate::operation::planner_harness_shutdown_adapter::PlannerHarnessShutdownOperationPayload;
use crate::operation::planner_harness_start_adapter::{
    CONVERSATION_ORDINAL_KEY, NewConversation, PlannerHarnessStartOperationPayload, busy,
    hold_open, refuse_user_entries, unconfirmed,
};
use crate::operation::planner_start_fence::CardStartFence;
use crate::per_card_lock::lock_key;
use crate::routes::idempotency_key::{parse_idempotency_key_header, stable_payload_hash};
use crate::routes::planner_cards::{planner_card_operation_outcome, run_planner_card_operation};
use crate::session_projection_repo::{WorkerSessionProjection, WorkerSessionState};
use crate::state::{AppState, CodexShellState, RouteState};

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/cards/{id}/planner/conversations",
        post(start_planner_conversation).get(list_planner_conversations),
    )
}

/// The scope a person's key is stored under: one card's new-conversation requests.
fn scoped_key(card: &Card, idempotency_key: &str) -> String {
    format!("planner-new-conversation:{}:{idempotency_key}", card.id)
}

#[utoipa::path(
    post,
    path = "/api/cards/{id}/planner/conversations",
    tag = "cards",
    params(
        ("id" = String, Path, description = "Planner card id"),
        ("Idempotency-Key" = String, Header, description = "Required. A replay under the same key returns the first request's answer and starts nothing."),
    ),
    responses(
        (status = 200, description = "A new, blank conversation is current; the earlier one is read-only history", body = PlannerConversationStarted),
        (status = 400, description = "Missing, blank, non-ASCII or over-long `Idempotency-Key`", body = ErrorBody),
        (status = 403, description = "Only the person starts a new conversation; or the card is not a Planner, or is on an area chat Track", body = ErrorBody),
        (status = 404, description = "Card or Track not found", body = ErrorBody),
        (status = 409, description = "`conflict`: busy, unconfirmed, closed or changed, and nothing changed; `idempotency_key_reused`: another request's key", body = ErrorBody),
        (status = 500, description = "The start failed after the cutover; the new conversation stays current and restart recovers it", body = ErrorBody),
        (status = 503, description = "The Planner is still settling; retry shortly", body = ErrorBody),
    ),
)]
pub(crate) async fn start_planner_conversation(
    State(s): State<RouteState>,
    State(cs): State<CodexShellState>,
    State(app): State<AppState>,
    actor: Actor,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<PlannerConversationStarted>> {
    super::track_report_blocks::require_rest_user_actor_for(
        &actor,
        "new planner conversation",
        "Only the person starts a new Planner conversation.",
    )?;
    let card = s
        .repo
        .card_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    crate::track_access::require_detail_access(s.repo.as_ref(), card.track_id.as_str(), &principal)
        .await?;
    let binding = s
        .write
        .verify_role(&card.id)
        .and_then(|role| PlannerBinding::from_card(&card, role))
        .filter(|binding| binding.profile == HarnessProfile::Planner)
        .ok_or_else(|| CalmError::Forbidden(format!("card {id} is not a Planner card")))?;
    let idempotency_key = parse_idempotency_key_header(&headers)?.ok_or_else(|| {
        CalmError::BadRequest(
            "Idempotency-Key header is required so a retried request cannot start a second conversation"
                .into(),
        )
    })?;
    let key = OperationKey {
        operation_key: new_id(),
        idempotency_key: Some(scoped_key(&card, &idempotency_key)),
        // The request, not the session it replaces: a replay after the cutover must hash the same.
        payload_hash: stable_payload_hash(&json!({
            "actor": actor.as_str(),
            "card_id": card.id,
            "request": "new_conversation",
        }))?,
    };
    // Replay before admission: admitting or quiescing again would act on the NEW conversation.
    if let Some(op_id) = CardStartFence::keyed_start(&s.operation_runtime, &key).await? {
        return started(&s, &card, &key, &op_id).await.map(Json);
    }
    // The session the person is replacing, read before waiting on the card: a request that
    // queued behind another cutover compares against the session it was made for.
    let expected = s
        .repo
        .session_projection_active_for_card(&card.id.to_string())
        .await?
        .ok_or_else(no_live_conversation)?;
    let fence = CardStartFence::lock(
        &s.planner_recovery_locks,
        &s.repo,
        &s.operation_runtime,
        &card.id,
    )
    .await;
    // A same-key request that held the card first has stored its answer.
    if let Some(op_id) = CardStartFence::keyed_start(&s.operation_runtime, &key).await? {
        return started(&s, &card, &key, &op_id).await.map(Json);
    }
    let track = s
        .repo
        .track_get(card.track_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {}", card.track_id)))?;
    if track.purpose.as_deref() == Some(crate::AREA_CHAT_PURPOSE) {
        return Err(CalmError::Forbidden(format!(
            "planner harness is disabled for area chat track {}",
            track.id
        )));
    }
    if track.closed_at.is_some() {
        return Err(CalmError::Conflict(
            "Reopen the Track before starting a new Planner conversation.".into(),
        ));
    }
    crate::conversation_archive::require_unarchived(s.repo.as_ref(), card.id.as_str()).await?;
    // #2843: a canonical conversation's accepted inputs, pending ones included, stay with its
    // current carrier, and the start only moves historical storage; refuse before any quiesce.
    if crate::conversation_inputs::has_canonical_storage(s.repo.as_ref(), card.id.as_str()).await? {
        return Err(canonical());
    }
    PlannerFactory::new(
        std::sync::Arc::downgrade(&cs.shared_codex_appserver),
        &s.claude_planner,
        &s.acp_planner,
    )
    .ready(&binding.provider, ReadinessFor::StartOperation)
    .await?;
    let current = admit(&s, &cs, &app, &fence, &card, &expected).await?;

    let payload = PlannerHarnessStartOperationPayload {
        actor: actor.to_actor_id(),
        track_id: track.id.to_string(),
        planner_card_id: card.id.clone(),
        report_card_id: None,
        sort: None,
        cwd: track.workspace.agent_cwd().to_string(),
        goal: None,
        reset_harness_items: false,
        force_new_thread: true,
        profile: HarnessProfile::Planner,
        create_card: None,
        first_message: None,
        create_request_sha256: None,
        opening_briefing: None,
        new_conversation: Some(NewConversation {
            expected_session_id: current.id.clone(),
        }),
    };
    let stored_key = key.clone();
    let result = fence
        .start_new_conversation(s.planner_start_adapter.clone(), principal, &payload, key)
        .await?;
    planner_card_operation_outcome(result.outcome)?;
    // The predecessor's provider resources, as a restart releases them.
    run_planner_card_operation(
        &s,
        "planner-harness-shutdown",
        serde_json::to_value(PlannerHarnessShutdownOperationPayload {
            worker_session_id: current.id,
        })?,
    )
    .await?;
    started(&s, &card, &stored_key, &result.op_id)
        .await
        .map(Json)
}

fn no_live_conversation() -> CalmError {
    CalmError::Conflict(
        "The Planner has no live conversation; restart it before starting a new one.".into(),
    )
}

fn canonical() -> CalmError {
    CalmError::Conflict(
        "A new conversation is not available yet: this Planner keeps its messages, queued ones included, here."
            .into(),
    )
}

/// Admission under the card's fence: the card is settled, its session is the one the person
/// meant, nothing refuses the cutover, and the live owner (if any) is quiesced on its own loop.
/// Every refusal leaves the data and the owner as they were. A request abandoned after the
/// quiesce leaves an ordinary sleeping Planner, which the shared wake path restores.
async fn admit(
    s: &RouteState,
    cs: &CodexShellState,
    app: &AppState,
    fence: &CardStartFence,
    card: &Card,
    expected: &WorkerSessionProjection,
) -> Result<WorkerSessionProjection> {
    // Lock order: card -> drive -> track, as archive and hibernation take them.
    let operations = s.operation_runtime.lock_for_track_delete().await;
    let track_lock = lock_key(&s.track_delete_locks, card.track_id.as_str()).await;
    fence.require_settled().await?;
    let current = s
        .repo
        .session_projection_active_for_card(&card.id.to_string())
        .await?
        .ok_or_else(no_live_conversation)?;
    if current.id != expected.id {
        return Err(CalmError::Conflict(
            "The Planner conversation changed while this request waited; review it and try again."
                .into(),
        ));
    }
    refuse_persisted_activity(app, card, &current).await?;
    let handle = s.harness.get(&current.id);
    match handle.as_ref() {
        Some(handle) => {
            if !handle.hibernate_if_idle().await? {
                return Err(busy());
            }
            s.harness.remove_if_same(&current.id, handle);
        }
        None => refuse_unowned(&current)?,
    }
    drop(track_lock);
    drop(operations);
    crate::planner_hibernation::release_quiesced(
        handle.as_ref(),
        &current,
        crate::harness::effective_runtime_thread_id(&current),
        &cs.shared_codex_appserver,
    )
    .await?;
    Ok(current)
}

/// Durable facts that refuse the cutover whether or not an owner is live: a `hold` ask the
/// provider waits on, and a provider submission whose outcome is unknown.
async fn refuse_persisted_activity(
    app: &AppState,
    card: &Card,
    current: &WorkerSessionProjection,
) -> Result<()> {
    let db = app
        .sqlite_db()
        .ok_or_else(|| CalmError::Internal("a new conversation requires durable storage".into()))?;
    if crate::ask::has_open_hold_ask(
        db.read_pool(),
        &card.track_id,
        &WorkerSessionId::from(current.id.as_str()),
    )
    .await?
    {
        return Err(hold_open());
    }
    if crate::db::sqlite::acp_submission_unresolved(db.read_pool(), &current.id).await? {
        return Err(unconfirmed());
    }
    Ok(())
}

/// Without a live owner the persisted snapshot is the only evidence: it must parse and be idle
/// with no person's sentence queued, and the row must not claim activity nothing owns.
fn refuse_unowned(current: &WorkerSessionProjection) -> Result<()> {
    if matches!(
        current.status,
        WorkerSessionState::Starting
            | WorkerSessionState::Running
            | WorkerSessionState::TurnPending
    ) {
        return Err(unconfirmed());
    }
    let snapshot = current
        .handle_state_json
        .clone()
        .and_then(HarnessSnapshot::parse_known)
        .ok_or_else(unconfirmed)?;
    if !matches!(
        snapshot.phase,
        HarnessPhaseTag::Idle | HarnessPhaseTag::TurnCompleted | HarnessPhaseTag::Resumed
    ) {
        return Err(busy());
    }
    refuse_user_entries(&snapshot.pending_entries())
}

/// The stored answer of the start a key binds, the same on its first attempt and on every replay.
async fn started(
    s: &RouteState,
    card: &Card,
    key: &OperationKey,
    op_id: &crate::operation::OperationId,
) -> Result<PlannerConversationStarted> {
    let result = s.operation_runtime.wait(op_id).await?;
    planner_card_operation_outcome(result.outcome)?;
    let op = CardStartFence::stored_start(&s.operation_runtime, key)
        .await?
        .filter(|op| &op.id == op_id)
        .ok_or_else(|| CalmError::Internal(format!("start {op_id} is not this key's")))?;
    let data = op
        .tx_output
        .map(|output| output.data)
        .ok_or_else(|| CalmError::Internal(format!("start {op_id} has no committed output")))?;
    let conversation = data
        .get(CONVERSATION_ORDINAL_KEY)
        .and_then(Value::as_i64)
        .ok_or_else(|| CalmError::Internal(format!("start {op_id} opened no conversation")))?;
    let new_thread_id = data
        .get("codex_thread_id")
        .and_then(Value::as_str)
        .ok_or_else(|| CalmError::Internal(format!("start {op_id} minted no thread")))?
        .to_string();
    Ok(PlannerConversationStarted {
        card_id: card.id.clone(),
        conversation,
        new_thread_id,
    })
}

#[utoipa::path(
    get,
    path = "/api/cards/{id}/planner/conversations",
    tag = "cards",
    params(("id" = String, Path, description = "Planner card id")),
    responses(
        (status = 200, description = "Earlier, read-only conversations that still hold transcript rows, oldest first", body = Vec<PlannerConversation>),
        (status = 403, description = "Current Area access refused, or the card is not a Planner card", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_planner_conversations(
    State(s): State<RouteState>,
    principal: Principal,
    Path(id): Path<String>,
) -> Result<Json<Vec<PlannerConversation>>> {
    let scope = ReadScope::check(s.repo.as_ref(), &id, &principal).await?;
    let card = super::cards::harness_card(&s, &id).await?;
    if card.track_id.as_str() != scope.track {
        return Err(CalmError::Forbidden("conversation moved".into()));
    }
    if s.write.verify_role(&card.id) != Some(CardRole::Planner) {
        return Err(CalmError::Forbidden(format!(
            "card {id} is not a Planner card"
        )));
    }
    let conversations = s
        .repo
        .planner_conversations_earlier(card.id.as_str())
        .await?;
    #[cfg(feature = "fixtures")]
    crate::track_access::after_assembly(&scope.track).await?;
    scope.publish(s.repo.as_ref(), &principal).await?;
    Ok(Json(conversations))
}
