//! The one place a message send decides what a harness card's session needs before its text is
//! queued: reuse a live one, rebuild a registry miss, recover a failed one, or start a card with
//! no thread to preserve (#2184). Everything else is 409 `planner_harness_dormant` or 503.

use crate::actor::Actor;
use crate::error::{CalmError, Result};
use crate::harness::profile::{HarnessProfile, PlannerBinding};
use crate::harness::{PlannerHarness, effective_runtime_thread_id, is_harness_snapshot_value};
use crate::ids::CardId;
use crate::model::Card;
use crate::operation::planner_harness_start_adapter::profile_mints_its_own_card;
use crate::operation::planner_start_fence::CardStartFence;
use crate::routes::planner_cards::{HarnessCardStart, start_harness_card};
use crate::session_projection_repo::{
    AgentProvider, CardConversation, WorkerSessionKind, WorkerSessionProjection, WorkerSessionState,
};
use crate::state::{CodexShellState, RouteState, WorkerState};

use super::planner_recovery;

/// The 409 `planner_harness_dormant` of every planner write route. A client may show the text to
/// the person as it is, so it names the way out (`/planner/restart`) in their words.
pub(crate) fn dormant() -> CalmError {
    CalmError::PlannerHarnessDormant(
        "This conversation's session can't be resumed; start a fresh session (history is kept)"
            .into(),
    )
}

/// The backend a harness of `provider` turns on, as a 503 when it is not ready. A Claude Planner
/// needs its config and its pinned binary, not the shared app-server (#1791 §4.1 row 11).
async fn require_backend(
    s: &RouteState,
    cs: &CodexShellState,
    provider: AgentProvider,
) -> Result<()> {
    match provider {
        AgentProvider::OpenCode => s.acp_planner.check_ready(&provider).await,
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
/// * a person's send to a card nothing can start any more but the send ([`send_owns_first_start`]:
///   a managed Track's card, or a self-minted conversation whose starts all failed) with no
///   conversation to preserve → one `planner-harness-start`;
/// * a `starting` row, or a start in flight on a card with nothing to preserve → 503, retry;
/// * anything else (a live or retired carrier, a transcript, or a card whose creator may still
///   start it) → 409 `planner_harness_dormant`. Row-intrinsic dormancy is checked before daemon
///   liveness, so such a row is 409 even with the daemon down.
///
/// Everything past the fast path runs under the card's [`CardStartFence`], which `/planner/reset`
/// and every other start take too, and re-reads the card under it, so racing sends neither
/// double-spawn nor double-start. The fence is RETURNED so the caller holds it through enqueue and
/// audit.
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
    Option<CardStartFence>,
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

    let fence = CardStartFence::lock(
        &s.planner_recovery_locks,
        &s.repo,
        &s.operation_runtime,
        card_id,
    )
    .await;
    // Re-fetch under the lock and use only this row: `/planner/reset` or a racing Send may have moved it.
    let Some(runtime) = planner_recovery::candidate(s, card_id, human_send).await? else {
        if !human_send {
            return Err(dormant());
        }
        return start_fresh(s, cs, card_id, actor, fence).await;
    };
    if runtime.status == WorkerSessionState::Failed {
        let (runtime, harness) = restore_failed_session(s, w, cs, runtime).await?;
        return Ok((runtime, harness, Some(fence)));
    }
    if let Some(harness) = s.harness.get(&runtime.id) {
        return Ok((runtime, harness, Some(fence)));
    }
    // A `starting` row means `planner-harness-start` is still in flight: the adapter writes the row BEFORE the harness is registered, so recovering here would spawn a harness the start op then shuts down, dropping any queued input. 503 so the client retries.
    if runtime.status == WorkerSessionState::Starting {
        return Err(starting());
    }
    // Pre-validate the snapshot: the strict deserializer inside recovery panics on unknown shapes.
    if !runtime
        .handle_state_json
        .as_ref()
        .is_some_and(is_harness_snapshot_value)
    {
        return Err(dormant());
    }
    // A half-failed start can leave an active row without a thread; boot recovery's rule falls back to the snapshot's `last_thread_id`, and only when BOTH are absent is the row unrecoverable.
    if effective_runtime_thread_id(&runtime).is_none() {
        return Err(dormant());
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
    let harness = install_preserved_session(s, w, cs, runtime.clone()).await?;
    tracing::info!(
        card_id = %card_id,
        runtime_id = %runtime_id,
        "planner harness lazily recovered on /planner/input registry miss"
    );
    Ok((runtime, harness, Some(fence)))
}

/// Whether no creator can still submit this card's initial start, so a send may run it: a
/// managed Track's cards (`managed_track::planner_starts_on_first_send`), and a self-minted
/// conversation card whose minting start already ran and failed. Any other card's creator (an
/// ordinary create and its keyed retry, the launchpad's ensure, a child bootstrap) may start it
/// again, superseding whatever session a send had started.
async fn send_owns_first_start(
    s: &RouteState,
    card: &Card,
    profile: HarnessProfile,
    conversation: CardConversation,
) -> Result<bool> {
    if crate::managed_track::planner_starts_on_first_send(&s.mcp_context, card.track_id.as_str())
        .await?
    {
        return Ok(true);
    }
    Ok(conversation == CardConversation::OnlyFailedStarts && profile_mints_its_own_card(profile))
}

/// A person's send found no session to use. Only a card whose start would lose nothing is
/// started: a live or retired carrier, or a transcript, is a conversation, and minting a new one
/// over it is `/planner/restart`'s or `/planner/reset`'s decision, never a send's. Nor is a card started while a
/// creator's start can still come: that start would supersede this send's session. The start is unkeyed, so a refused
/// start (the backend down: 503) leaves nothing behind and the next send starts it.
async fn start_fresh(
    s: &RouteState,
    cs: &CodexShellState,
    card_id: &CardId,
    actor: &Actor,
    fence: CardStartFence,
) -> Result<(
    WorkerSessionProjection,
    PlannerHarness,
    Option<CardStartFence>,
)> {
    let card = s
        .repo
        .card_get(card_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    let conversation = fence.conversation().await?;
    match conversation {
        CardConversation::ThreadToPreserve => return Err(dormant()),
        CardConversation::StartInFlight => return Err(starting()),
        CardConversation::NeverStarted | CardConversation::OnlyFailedStarts => {}
    }
    // The card as re-read under the lock: what it is (the start's profile) and which backend runs it.
    let binding = s
        .write
        .verify_role(&card.id)
        .and_then(|role| PlannerBinding::from_card(&card, role))
        .ok_or_else(|| CalmError::Forbidden(format!("card {card_id} is not a harness card")))?;
    if !send_owns_first_start(s, &card, binding.profile, conversation).await? {
        return Err(dormant());
    }
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point(crate::test_seams::PLANNER_FIRST_START, card_id.as_str()).await;
    // The start adapter's own readiness refusal is a 500; this is the send's 503, as for recovery.
    require_backend(s, cs, binding.provider).await?;
    start_harness_card(
        s,
        &fence,
        actor,
        &card,
        binding.profile,
        HarnessCardStart::Fresh,
    )
    .await?;
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
    Ok((runtime, harness, Some(fence)))
}

/// Resume only the original failed thread, under the caller's CardStartFence.
/// A refusal leaves the conversation eligible for its explicit recovery UI; no mint fallback.
pub(super) async fn restore_failed_session(
    s: &RouteState,
    w: &WorkerState,
    cs: &CodexShellState,
    runtime: WorkerSessionProjection,
) -> Result<(WorkerSessionProjection, PlannerHarness)> {
    let runtime = planner_recovery::recover(s, w, cs, runtime).await?;
    let harness = install_preserved_session(s, w, cs, runtime.clone()).await?;
    Ok((runtime, harness))
}
#[allow(deprecated)]
async fn install_preserved_session(
    s: &RouteState,
    w: &WorkerState,
    cs: &CodexShellState,
    runtime: WorkerSessionProjection,
) -> Result<PlannerHarness> {
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
    Ok(harness)
}
