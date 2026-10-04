//! `POST /api/cards/{id}/planner/input` — one message into a harness card's queue.
//! Every send carries an `Idempotency-Key` (#2043). Its binding commits in the harness
//! transaction that stores the message, so a retry after a lost answer replays that answer
//! instead of queueing the message a second time.

use crate::actor::Actor;
use crate::db::sqlite::planner_input_binding_get;
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::harness::{SendKey, is_harness_snapshot_value};
use crate::ids::{ActorId, CardId};
use crate::per_card_lock::{PerCardLockGuard, lock_card, lock_key};
use crate::routes::cards::{card_runs_headless_harness, validate_planner_input};
use crate::routes::terminal_cards::{parse_idempotency_key_header, stable_payload_hash};
use crate::session_projection_repo::{WorkerSessionProjection, WorkerSessionState};
use crate::state::{CodexShellState, RouteState, WorkerState};

use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use calm_types::planner_attachment::AttachmentId;
use calm_types::worker::WorkerSessionId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;

#[derive(Debug, Deserialize, ToSchema)]
pub struct SendPlannerInputRequest {
    pub text: String,
    /// Ids returned by `POST /api/cards/{id}/planner/attachments`. Naming an attachment here is what BINDS it: the bytes move out of the sweepable staging area before this request writes anything to the queue.
    /// An id belonging to another card is a 400, as is naming the same one twice or naming more than eight.
    #[serde(default)]
    pub attachments: Vec<AttachmentId>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SendPlannerInputResponse {
    #[schema(value_type = String)]
    pub card_id: CardId,
    pub worker_session_id: String,
    /// Stable id of the queue entry this text landed in, so the client can match its optimistic echo. Null only when the text folded into a pre-id queue entry.
    pub entry_id: Option<String>,
}

fn planner_input_audit_actor(actor: &Actor, card_id: &CardId) -> ActorId {
    match actor.to_actor_id() {
        ActorId::AiCodex(c) if c.as_str().is_empty() => ActorId::AiCodex(card_id.clone()),
        // Middleware currently only admits `ai:codex`; the other branches are ready for more AI kinds.
        ActorId::AiClaude(c) if c.as_str().is_empty() => ActorId::AiClaude(card_id.clone()),
        ActorId::AiPlanner(c) if c.as_str().is_empty() => ActorId::AiPlanner(card_id.clone()),
        other => other,
    }
}

/// Queue one message for the card's harness, under a required `Idempotency-Key`.
///
/// The first request under a key that the server stores binds the key to that message, in the
/// transaction that stores it. A retry with the same key and the same body (text, attachments,
/// actor) answers 200 with the first request's body and queues nothing, whatever happened to the
/// message since. The same key with a different body is 409 `conflict`. A refusal stores and
/// binds nothing, so its key can be sent again. A binding lasts as long as its card.
#[utoipa::path(
    post,
    path = "/api/cards/{id}/planner/input",
    tag = "cards",
    params(
        ("id" = String, Path, description = "Planner card id"),
        ("Idempotency-Key" = String, Header, description = "**Required.** One key per message; a retry under it replays the first answer."),
    ),
    request_body = SendPlannerInputRequest,
    responses(
        (status = 200, description = "User text queued for next harness turn, or the answer of the earlier request under this Idempotency-Key", body = SendPlannerInputResponse),
        (status = 400, description = "Empty text, or a missing or blank Idempotency-Key", body = ErrorBody),
        (status = 403, description = "Card is not a planner codex card", body = ErrorBody),
        (status = 404, description = "Card or track not found", body = ErrorBody),
        (status = 409, description = "This Idempotency-Key was used for a different message (code `conflict`); runtime is shutting down (code `conflict`); the planner harness session is dormant and not recoverable — reset to start a session (code `planner_harness_dormant`); or the runtime is no longer this card's and the text was NOT stored, so re-sending it reaches the successor (code `planner_harness_runtime_superseded`)", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
        (status = 503, description = "Observation queue saturated, shared codex app-server not running, or a planner-harness start is still in flight — retry shortly", body = ErrorBody),
    ),
)]
#[allow(deprecated)]
pub(crate) async fn send_planner_input(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    State(cs): State<CodexShellState>,
    actor: Actor,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<SendPlannerInputRequest>,
) -> Result<Json<SendPlannerInputResponse>> {
    let idempotency_key = parse_idempotency_key_header(&headers)?.ok_or_else(|| {
        CalmError::BadRequest(
            "Idempotency-Key header is required so a retried send cannot queue the message twice"
                .into(),
        )
    })?;
    send_planner_input_keyed(&s, &w, &cs, actor, id, body, idempotency_key)
        .await
        .map(Json)
}

/// The send behind the route, for callers inside the server that mint their own key.
#[allow(deprecated)]
pub(crate) async fn send_planner_input_keyed(
    s: &RouteState,
    w: &WorkerState,
    cs: &CodexShellState,
    actor: Actor,
    id: String,
    body: SendPlannerInputRequest,
    idempotency_key: String,
) -> Result<SendPlannerInputResponse> {
    let SendPlannerInputRequest { text, attachments } = body;
    let char_count = validate_planner_input(&text, !attachments.is_empty())?;

    let card = s
        .repo
        .card_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    let role = s
        .write
        .verify_role(&card.id)
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    if !card_runs_headless_harness(&card, role) {
        return Err(CalmError::Forbidden(format!(
            "card {id} is not a planner codex card",
        )));
    }

    // Decided before anything with an effect: a retry's attachments are already bound, and a lazy
    // restart would recover a runtime this request no longer needs.
    let key = SendKey {
        payload_hash: format!(
            "v1:{}",
            stable_payload_hash(&json!({
                "actor": actor.as_str(),
                "text": &text,
                "attachments": &attachments,
            }))?
        ),
        idempotency_key,
    };
    let _key_guard = lock_key(
        &s.planner_input_key_locks,
        &format!("{}\u{0}{}", card.id, key.idempotency_key),
    )
    .await;
    if let Some(answer) = replay(w, &card.id, &key).await? {
        return Ok(answer);
    }
    #[cfg(feature = "fixtures")]
    wait_at_replay_miss_hook_for_test(card.id.as_str()).await;

    // `_recovery_guard` holds the per-card recovery lock until end of scope, so a concurrent `/planner/reset` can't supersede the just-recovered runtime before the observe/audit below.
    let (runtime, harness, _recovery_guard) =
        ensure_live_planner_harness(s, w, cs, &card.id, actor.as_str() == "user").await?;
    let track = s
        .repo
        .track_get(card.track_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {} for card {id}", card.track_id)))?;
    let scope = EventScope::Card {
        card: card.id.clone(),
        track: track.id.clone(),
        area: track.area_id.clone(),
    };
    // Bind BEFORE the entry exists: the bytes leave the sweepable staging directory first, so a queued message never names a file the orphan sweep may remove (codex answers an unreadable image with placeholder text and no error). A bind followed by a failed enqueue leaks bytes in `bound/`, which is the better failure direction.
    // `attachment_root` refuses an attached workspace, so such a card gets a 400 here only when it actually names an attachment.
    let attachments = if attachments.is_empty() {
        Vec::new()
    } else {
        let root =
            crate::planner_attachments::attachment_root(&track.workspace, &s.workspace_root)?;
        crate::planner_attachments::bind::bind_attachments(
            &root,
            &card.id,
            &attachments,
            &s.planner_attachment_locks,
        )
        .await?
    };
    // Migrate ONLY the AI-header path (empty placeholder card) to the live planner session actor; the human path MUST stay unchanged so the audit log keeps distinguishing human input from agent actions.
    let audit_actor = match actor.to_actor_id() {
        ActorId::AiCodex(c) | ActorId::AiClaude(c) | ActorId::AiPlanner(c)
            if c.as_str().is_empty() && runtime.status.is_active_authority() =>
        {
            ActorId::AiPlannerSession(WorkerSessionId::from(runtime.id.clone()))
        }
        _ => planner_input_audit_actor(&actor, &card.id),
    };

    let attachment_count = attachments.len();
    let ack = harness
        .observe_user_message_durable(text, attachments, key)
        .await?;

    tracing::info!(
        actor = %actor.as_str(),
        card_id = %card.id,
        runtime_id = %runtime.id,
        char_count,
        attachment_count,
        "planner harness user message enqueued"
    );

    if let Err(error) = s
        .repo
        .log_pure_event(
            audit_actor,
            scope,
            None,
            &s.events,
            s.write.role_cache(),
            s.write.area_cache(),
            Event::HarnessUserMessageEnqueued {
                worker_session_id: runtime.id.clone(),
                card_id: card.id.clone(),
                track_id: card.track_id.clone(),
                char_count: char_count as u32,
            },
        )
        .await
    {
        // The user message is already durably accepted; a 500 here would invite a retry that executes the same intent twice.
        tracing::error!(
            card_id = %card.id,
            runtime_id = %runtime.id,
            error = %error,
            "planner input was accepted but its audit event failed"
        );
    }

    Ok(SendPlannerInputResponse {
        card_id: card.id,
        worker_session_id: runtime.id.clone(),
        entry_id: ack.entry_id.map(|id| id.as_str().to_string()),
    })
}

/// The answer an earlier request under this key was given, if one was stored. The answer is a
/// fact about that request and is replayed as it was, wherever its entry has gone since.
async fn replay(
    w: &WorkerState,
    card_id: &CardId,
    key: &SendKey,
) -> Result<Option<SendPlannerInputResponse>> {
    let pool = w.repo.sqlite_pool().ok_or_else(|| {
        CalmError::Internal("planner input keys require a sqlite-backed repo".into())
    })?;
    let Some(binding) =
        planner_input_binding_get(&pool, card_id.as_str(), &key.idempotency_key).await?
    else {
        return Ok(None);
    };
    if binding.payload_hash != key.payload_hash {
        return Err(CalmError::Conflict(
            "This Idempotency-Key was already used for a different message on this card; send \
             the new message under a new key."
                .into(),
        ));
    }
    Ok(Some(SendPlannerInputResponse {
        card_id: card_id.clone(),
        worker_session_id: binding.worker_session_id,
        entry_id: binding.entry_id,
    }))
}

/// Resolve a live [`PlannerHarness`] handle for a planner card. Fast path: active runtime row + registry hit. Registry miss with an active row: lazily re-spawn via `spawn_recovered_harness` (no Codex RPC). A human send can also recover a `failed` carrier through `planner_recovery`.
/// No eligible row, or an unrecoverable one (no thread anywhere, or a corrupt snapshot) → typed 409 `PlannerHarnessDormant` so the client steers the user to `/planner/reset`.
/// Takes the per-card lock and re-fetches under it so racing Sends can't double-spawn; `/planner/reset` takes the SAME lock, and the guard is RETURNED so the caller holds it through enqueue/audit. Row-intrinsic dormancy (409) is checked before daemon liveness (503).
#[allow(deprecated)]
async fn ensure_live_planner_harness(
    s: &RouteState,
    w: &WorkerState,
    cs: &CodexShellState,
    card_id: &CardId,
    human_send: bool,
) -> Result<(
    WorkerSessionProjection,
    crate::harness::PlannerHarness,
    Option<PerCardLockGuard>,
)> {
    let dormant = || {
        CalmError::PlannerHarnessDormant(format!(
            "no recoverable planner harness session for card {card_id}; reset to start a session",
        ))
    };
    // Unlocked fast path only: its reads can straddle a racing Send's recovery commit, so a miss
    // here is not dormancy (#1820); only the locked re-check below answers 409.
    if let Some(runtime) = super::planner_recovery::candidate(s, card_id, human_send).await?
        && runtime.status != WorkerSessionState::Failed
        && let Some(harness) = s.harness.get(&runtime.id)
    {
        return Ok((runtime, harness, None));
    }

    let guard = lock_card(&s.planner_recovery_locks, card_id.as_str()).await;
    // Re-fetch under the lock and use only this row: `/planner/reset` or a racing Send may have moved it.
    let runtime = super::planner_recovery::candidate(s, card_id, human_send)
        .await?
        .ok_or_else(dormant)?;
    let runtime = if runtime.status == WorkerSessionState::Failed {
        super::planner_recovery::recover(s, w, cs, runtime).await?
    } else {
        runtime
    };
    if let Some(harness) = s.harness.get(&runtime.id) {
        return Ok((runtime, harness, Some(guard)));
    }
    // A `starting` row means `planner-harness-start` is still in flight: the adapter writes the row BEFORE the harness is registered, so recovering here would spawn a harness the start op then shuts down, dropping any queued input. 503 so the client retries.
    if runtime.status == WorkerSessionState::Starting {
        return Err(CalmError::ServiceUnavailable(
            "planner harness is starting; retry shortly".into(),
        ));
    }
    // Row-intrinsic dormancy runs BEFORE the daemon liveness probe, so an unrecoverable row 409s (Reset) even when the daemon is down. Pre-validate the snapshot: the strict deserializer inside recovery panics on unknown shapes.
    let snapshot_value = match runtime.handle_state_json.as_ref() {
        Some(value) if is_harness_snapshot_value(value) => value,
        _ => return Err(dormant()),
    };
    // A half-failed start can leave an active row without a thread; mirror boot recovery's fallback to the snapshot's `last_thread_id`, and only when BOTH are absent is the row unrecoverable.
    let has_thread = |t: Option<&str>| t.map(str::trim).is_some_and(|trimmed| !trimmed.is_empty());
    if !has_thread(runtime.thread_id.as_deref())
        && !has_thread(snapshot_value.get("last_thread_id").and_then(Value::as_str))
    {
        return Err(dormant());
    }
    // A recovered harness can't issue turns without its backend; surface that instead of spawning a silently-wedged task.
    // A Claude Planner needs its config and its pinned binary, not the shared app-server (#1791 §4.1 row 11).
    if runtime.kind == crate::session_projection_repo::WorkerSessionKind::SharedPlanner
        && runtime.agent_provider == Some(crate::session_projection_repo::AgentProvider::Claude)
    {
        s.claude_planner.check_ready().await?;
    } else if !cs.shared_codex_appserver.is_running() {
        return Err(CalmError::ServiceUnavailable(
            cs.shared_codex_appserver.not_running_message(),
        ));
    }
    let runtime_id = runtime.id.clone();
    let harness = crate::harness::spawn_recovered_harness(
        w.repo.clone(),
        s.events.clone(),
        s.write.role_cache().clone(),
        s.write.area_cache().clone(),
        cs.shared_codex_appserver.clone(),
        s.thread_seals.clone(),
        &s.claude_planner_wiring(),
        &s.harness,
        &s.track_delete_locks,
        runtime.clone(),
        crate::harness::ClaimMode::Replace,
    )
    .await?
    .installed()
    .ok_or_else(dormant)?;
    tracing::info!(
        card_id = %card_id,
        runtime_id = %runtime_id,
        "planner harness lazily recovered on /planner/input registry miss"
    );
    // Return the guard so the caller keeps the per-card lock alive through `harness.observe` and the audit event.
    Ok((runtime, harness, Some(guard)))
}

/// Pause one send of `card_id` right after its replay check found no binding, so a test can
/// commit a concurrent send under the same key in between (#2043, the UNIQUE backstop).
#[cfg(feature = "fixtures")]
#[derive(Clone)]
pub struct ReplayMissHook {
    pub entered: std::sync::Arc<tokio::sync::Notify>,
    pub release: std::sync::Arc<tokio::sync::Notify>,
}

#[cfg(feature = "fixtures")]
fn replay_miss_hooks()
-> &'static std::sync::Mutex<std::collections::HashMap<String, ReplayMissHook>> {
    static HOOKS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, ReplayMissHook>>,
    > = std::sync::OnceLock::new();
    HOOKS.get_or_init(Default::default)
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn install_replay_miss_hook_for_test(card_id: &str, hook: ReplayMissHook) {
    replay_miss_hooks()
        .lock()
        .expect("replay miss hook mutex")
        .insert(card_id.to_owned(), hook);
}

#[cfg(feature = "fixtures")]
async fn wait_at_replay_miss_hook_for_test(card_id: &str) {
    let hook = replay_miss_hooks()
        .lock()
        .expect("replay miss hook mutex")
        .remove(card_id);
    if let Some(hook) = hook {
        hook.entered.notify_one();
        hook.release.notified().await;
    }
}
