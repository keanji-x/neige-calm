//! The one place a message send decides what a harness card's session needs before its text is
//! queued: reuse a live one, rebuild a registry miss, recover a failed one, or start a card with
//! no thread to preserve (#2184). Everything else is 409 `planner_harness_dormant` or 503.

use crate::actor::Actor;
use crate::error::{CalmError, Result};
use crate::harness::profile::PlannerBinding;
use crate::harness::{PlannerHarness, is_harness_snapshot_value};
use crate::ids::CardId;
use crate::per_card_lock::{PerCardLockGuard, lock_card};
use crate::routes::cards::{HarnessCardStart, start_harness_card};
use crate::session_projection_repo::{
    AgentProvider, CardConversation, WorkerSessionKind, WorkerSessionProjection, WorkerSessionState,
};
use crate::state::{CodexShellState, RouteState, WorkerState};
use serde_json::Value;

use super::planner_recovery;

fn dormant(card_id: &CardId) -> CalmError {
    CalmError::PlannerHarnessDormant(format!(
        "no recoverable planner harness session for card {card_id}; reset to start a session",
    ))
}

/// The backend a harness of `provider` turns on, as a 503 when it is not ready. A Claude Planner
/// needs its config and its pinned binary, not the shared app-server (#1791 §4.1 row 11).
async fn require_backend(
    s: &RouteState,
    cs: &CodexShellState,
    provider: AgentProvider,
) -> Result<()> {
    match provider {
        AgentProvider::Claude => s.claude_planner.check_ready().await,
        AgentProvider::Codex if cs.shared_codex_appserver.is_running() => Ok(()),
        AgentProvider::Codex => Err(CalmError::ServiceUnavailable(
            cs.shared_codex_appserver.not_running_message(),
        )),
    }
}

fn starting() -> CalmError {
    CalmError::ServiceUnavailable("planner harness is starting; retry shortly".into())
}

/// A live [`PlannerHarness`] for the card, by the card's state:
/// * an active row with a registered harness → reused (unlocked fast path);
/// * an active row with a registry miss → re-spawned from its snapshot (no Codex RPC);
/// * a person's send to a `failed` carrier with a recoverable snapshot → `planner_recovery`;
/// * a person's send to a card with no thread to preserve (never started, or only failed starts
///   that never got a thread) → one `planner-harness-start`;
/// * a `starting` row, or a start in flight on a card with nothing to preserve → 503, retry;
/// * anything else (a retired, superseded or unrecoverable carrier holding a thread, or a
///   transcript) → 409 `planner_harness_dormant`. Row-intrinsic dormancy is checked before daemon
///   liveness, so such a row is 409 even with the daemon down.
///
/// Everything past the fast path runs under the per-card `planner_recovery_locks` guard, which
/// `/planner/reset` takes too, and re-reads the card under it, so racing sends neither double-spawn
/// nor double-start. The guard is RETURNED so the caller holds it through enqueue and audit.
#[allow(deprecated)]
pub(crate) async fn ensure_planner_session(
    s: &RouteState,
    w: &WorkerState,
    cs: &CodexShellState,
    card_id: &CardId,
    actor: &Actor,
) -> Result<(
    WorkerSessionProjection,
    PlannerHarness,
    Option<PerCardLockGuard>,
)> {
    let human_send = actor.as_str() == "user";
    // Unlocked fast path only: its reads can straddle a racing Send's recovery commit, so a miss
    // here is not dormancy (#1820); only the locked re-check below answers 409.
    if let Some(runtime) = planner_recovery::candidate(s, card_id, human_send).await?
        && runtime.status != WorkerSessionState::Failed
        && let Some(harness) = s.harness.get(&runtime.id)
    {
        return Ok((runtime, harness, None));
    }

    let guard = lock_card(&s.planner_recovery_locks, card_id.as_str()).await;
    // Re-fetch under the lock and use only this row: `/planner/reset` or a racing Send may have moved it.
    let Some(runtime) = planner_recovery::candidate(s, card_id, human_send).await? else {
        if !human_send {
            return Err(dormant(card_id));
        }
        return start_fresh(s, cs, card_id, actor, guard).await;
    };
    let runtime = if runtime.status == WorkerSessionState::Failed {
        planner_recovery::recover(s, w, cs, runtime).await?
    } else {
        runtime
    };
    if let Some(harness) = s.harness.get(&runtime.id) {
        return Ok((runtime, harness, Some(guard)));
    }
    // A `starting` row means `planner-harness-start` is still in flight: the adapter writes the row BEFORE the harness is registered, so recovering here would spawn a harness the start op then shuts down, dropping any queued input. 503 so the client retries.
    if runtime.status == WorkerSessionState::Starting {
        return Err(starting());
    }
    // Pre-validate the snapshot: the strict deserializer inside recovery panics on unknown shapes.
    let snapshot_value = match runtime.handle_state_json.as_ref() {
        Some(value) if is_harness_snapshot_value(value) => value,
        _ => return Err(dormant(card_id)),
    };
    // A half-failed start can leave an active row without a thread; mirror boot recovery's fallback to the snapshot's `last_thread_id`, and only when BOTH are absent is the row unrecoverable.
    let has_thread = |t: Option<&str>| t.map(str::trim).is_some_and(|trimmed| !trimmed.is_empty());
    if !has_thread(runtime.thread_id.as_deref())
        && !has_thread(snapshot_value.get("last_thread_id").and_then(Value::as_str))
    {
        return Err(dormant(card_id));
    }
    // A recovered harness can't issue turns without its backend; surface that instead of spawning a silently-wedged task.
    let provider = if runtime.kind == WorkerSessionKind::SharedPlanner
        && runtime.agent_provider == Some(AgentProvider::Claude)
    {
        AgentProvider::Claude
    } else {
        AgentProvider::Codex
    };
    require_backend(s, cs, provider).await?;
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
    .ok_or_else(|| dormant(card_id))?;
    tracing::info!(
        card_id = %card_id,
        runtime_id = %runtime_id,
        "planner harness lazily recovered on /planner/input registry miss"
    );
    Ok((runtime, harness, Some(guard)))
}

/// A person's send found no session to use. Only a card whose start would lose nothing is
/// started: a carrier holding a thread, or a transcript, is a conversation, and minting a new one
/// over it is `/planner/reset`'s decision, never a send's. The start is unkeyed, so a refused
/// start (the backend down: 503) leaves nothing behind and the next send starts it.
async fn start_fresh(
    s: &RouteState,
    cs: &CodexShellState,
    card_id: &CardId,
    actor: &Actor,
    guard: PerCardLockGuard,
) -> Result<(
    WorkerSessionProjection,
    PlannerHarness,
    Option<PerCardLockGuard>,
)> {
    match s
        .repo
        .session_projection_conversation_for_card(&card_id.to_string())
        .await?
    {
        CardConversation::NoThreadToPreserve => {}
        CardConversation::StartInFlight => return Err(starting()),
        CardConversation::ThreadToPreserve => return Err(dormant(card_id)),
    }
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point(crate::test_seams::PLANNER_FIRST_START, card_id.as_str()).await;
    let card = s
        .repo
        .card_get(card_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    // The start adapter's own readiness refusal is a 500; this is the send's 503, as for recovery.
    let provider = s
        .write
        .verify_role(&card.id)
        .and_then(|role| PlannerBinding::from_card(&card, role))
        .ok_or_else(|| CalmError::Forbidden(format!("card {card_id} is not a harness card")))?
        .provider;
    require_backend(s, cs, provider).await?;
    start_harness_card(s, actor, &card, HarnessCardStart::Fresh).await?;
    let runtime = s
        .repo
        .session_projection_active_for_card(&card_id.to_string())
        .await?
        .ok_or_else(|| {
            CalmError::Internal(format!(
                "planner harness start left card {card_id} without an active session"
            ))
        })?;
    let harness = s.harness.get(&runtime.id).ok_or_else(|| {
        CalmError::Internal(format!(
            "planner harness start left session {} unregistered",
            runtime.id
        ))
    })?;
    tracing::info!(
        card_id = %card_id,
        worker_session_id = %runtime.id,
        "planner harness started on a send to a card with no thread to preserve"
    );
    Ok((runtime, harness, Some(guard)))
}
