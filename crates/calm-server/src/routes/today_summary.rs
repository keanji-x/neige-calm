//! `POST /api/today/summary`: ask an agent to write today's progress into Today's
//! document. An empty day is refused having created nothing; otherwise ensure the
//! launchpad, create the summary conversation if absent, and unconditionally send one
//! planner input carrying the activity summary.

use axum::{
    Json, Router,
    extract::{FromRef, Path, State},
    http::{HeaderMap, HeaderValue},
    routing::post,
};
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use utoipa::ToSchema;

use crate::activity_window::{
    WorkspaceActivityWindow, activity_counts_block, todays_workspace_activity,
};
use crate::actor::Actor;
use crate::conversation_keys::{DerivedConversationKeys, derive_track_conversation_keys};
use crate::error::{CalmError, ErrorBody, Result};
use crate::ids::ActorId;
use crate::operation::planner_harness_start_adapter::{
    HarnessProfile, OpeningBriefing, PlannerHarnessStartOperationPayload,
};
use crate::per_card_lock::lock_card;
use crate::prompts::render_named;
use crate::routes::cards::{
    SendPlannerInputRequest, run_planner_card_operation, send_planner_input,
};
use crate::routes::conversations_shared::user_message_enqueued_on_active_runtime;
use crate::routes::today::ensure_today_launchpad;
use crate::routes::track_conversations::{
    NewTrackConversationBody, create_track_conversation_inner,
};
use crate::state::{AppState, CodexShellState, RouteState, WorkerState};

pub fn router() -> Router<AppState> {
    Router::new().route("/api/today/summary", post(write_today_summary))
}

/// The `Idempotency-Key` the summary conversation is derived from. A bare constant:
/// `derive_track_conversation_keys` feeds one digest to both the card id and the
/// operation key, so mixing in a workspace digest, actor or date would derive a second
/// conversation card.
pub const TODAY_SUMMARY_CONVERSATION_KEY: &str = "today-summary";

/// The conversation this endpoint talks to, derived; the only route from a track id to
/// a card id in this module, so a golden on it is a statement about the endpoint.
pub(crate) fn summary_conversation_keys(track_id: &str) -> DerivedConversationKeys {
    derive_track_conversation_keys(track_id, TODAY_SUMMARY_CONVERSATION_KEY)
}

/// The derived summary-conversation card id, for tests.
#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn today_summary_card_id_for_test(track_id: &str) -> String {
    summary_conversation_keys(track_id).card_id
}

/// The actor every request from this endpoint is attributed to. Fixed, not read from
/// the request: the payload is hashed into the operation's `payload_hash`, and a
/// same-key different-hash submit is a 409 that never expires. `user` because the act
/// is a human pressing a button; `Actor("kernel").to_actor_id()` degrades to `User` anyway.
fn synthetic_actor() -> Actor {
    Actor(Actor::DEFAULT.to_string())
}

/// The standing instruction the summary conversation is opened with. Sent when the
/// card's currently ACTIVE runtime has no `harness.user_message.enqueued` row of its
/// own, and never again while that runtime stays active.
pub const TODAY_SUMMARY_BOOTSTRAP_TEXT: &str =
    include_str!("../../prompts/today-summary/bootstrap.md");

/// The summary prompt's prose, with `{counts}` where the server-counted activity block goes.
const TODAY_SUMMARY_WRITE_TEXT: &str = include_str!("../../prompts/today-summary/write.md");

/// A rendezvous the create-under-a-fixed-key race can be created at. `None` in
/// production; a test arms it with a `Barrier::new(2)` so both requests park after
/// `card_get` returned `None` and before either submits. Lives on `AppState` rather
/// than a `static` so a threaded `cargo test` cannot share it across cases.
pub type TodaySummaryCreateRendezvous = Option<std::sync::Arc<tokio::sync::Barrier>>;

/// A rendezvous the first-message race can be created at. Separate from
/// [`TodaySummaryCreateRendezvous`]: one barrier serving both would be waited on twice
/// by a single request in the create case and hang. `None` in production.
pub type TodaySummaryBootstrapRendezvous = Option<std::sync::Arc<tokio::sync::Barrier>>;

/// Per-server observation of the create arm. `attempts` is incremented before the
/// rendezvous so a test knows a request passed `card_get` and found nothing;
/// `conflicts` is what proves the fallback actually ran. Unconditional rather than
/// fixtures-gated so the tested binary is the shipped one.
#[derive(Debug, Default)]
pub struct TodaySummaryCreateCounters {
    /// Requests that found no derived card and therefore entered the create arm.
    pub attempts: AtomicU64,
    /// Creates that lost the key race and took the 409 fallback.
    pub conflicts: AtomicU64,
    /// Requests that reached the bootstrap decision block. Incremented before the
    /// transcript is read, so it does not witness "both saw an empty transcript".
    pub bootstrap_arrivals: AtomicU64,
}

impl TodaySummaryCreateCounters {
    /// Reads for tests; production never looks.
    pub fn snapshot(&self) -> (u64, u64, u64) {
        (
            self.attempts.load(Ordering::Relaxed),
            self.conflicts.load(Ordering::Relaxed),
            self.bootstrap_arrivals.load(Ordering::Relaxed),
        )
    }
}

/// What the caller gets back on success.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TodaySummaryStarted {
    /// The launchpad track, whose report the agent is being asked to rewrite.
    pub track_id: String,
    /// The summary conversation's card. Stable for the launchpad's lifetime.
    pub card_id: String,
}

/// Render the prompt: template text plus five integers, so its maximum length can be
/// computed by reading it and stays under `MAX_PLANNER_INPUT_CHARS`.
fn summary_prompt(activity: &WorkspaceActivityWindow) -> Result<String> {
    Ok(render_named(
        TODAY_SUMMARY_WRITE_TEXT,
        &[("counts", &activity_counts_block(activity))],
    )?)
}

#[utoipa::path(
    post,
    path = "/api/today/summary",
    tag = "tracks",
    responses(
        (status = 200, description = "The summary conversation has been asked to write today's progress. The conversation is created on first use and reused thereafter; the reply arrives asynchronously as a report edit, not in this response.", body = TodaySummaryStarted),
        (status = 409, description = "Distinguished by the body's `code`:\n* `today_summary_no_activity` — nothing happened in the workspace today, so no conversation was created and no message was sent (INV-TODAYDOC-007).\n* `conflict` / `planner_harness_dormant` — from the underlying conversation create or planner input; a dormant harness is retried once automatically before it can reach here.", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
        (status = 503, description = "Shared codex app-server not running, a harness start is still in flight, or the observation queue is saturated — retry shortly", body = ErrorBody),
    ),
)]
/// Ask the summary conversation to write today's progress.
pub(crate) async fn write_today_summary(
    State(app): State<AppState>,
) -> Result<Json<TodaySummaryStarted>> {
    let s = RouteState::from_ref(&app);
    let w = WorkerState::from_ref(&app);
    let cs = CodexShellState::from_ref(&app);
    let pool = w
        .repo
        .sqlite_pool()
        .ok_or_else(|| CalmError::Internal("today summary requires a sqlite-backed repo".into()))?;

    // Read the launchpad, do NOT ensure it: ensuring here would mean an empty day still
    // materialized a workspace and started a harness.
    let launchpad = app.repo.track_get_launchpad().await?;
    // The shared entry point, not a second computation: the conversation-create path
    // reads the same window.
    let activity =
        todays_workspace_activity(&pool, launchpad.as_ref().map(|track| track.id.as_str())).await?;

    // The empty-day gate lives here rather than in the frontend: hiding the button is UI,
    // and a POST straight at this endpoint would sail past it. Only this endpoint refuses;
    // a user typing to an agent by hand is not what is being prevented.
    if activity.is_empty() {
        return Err(CalmError::TodaySummaryNoActivity(
            "nothing happened in this workspace today, so there is nothing to \
             summarise; no conversation was created and no message was sent"
                .into(),
        ));
    }

    // Idempotent, and the only bootstrap on this path; it materializes the workspace and
    // waits on a `planner-harness-start`.
    let (_status, Json(launchpad)) =
        ensure_today_launchpad(State(app.clone()), synthetic_actor()).await?;
    let track_id = launchpad.track_id;
    let derived = summary_conversation_keys(&track_id);

    // The branch predicate is "the card exists AND its live runtime has already been sent
    // a message", not "the card exists": a create that lands `Stuck` leaves the card
    // behind with its bootstrap stranded on a `failed` session's queue. The recovery is
    // NOT calling `create_track_conversation` again (the adapter refuses to re-mint); what
    // is missing is the message, so the message is what gets sent.
    if s.repo.card_get(&derived.card_id).await?.is_none() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "idempotency-key",
            HeaderValue::from_static(TODAY_SUMMARY_CONVERSATION_KEY),
        );
        // Counted before the rendezvous, so a test can tell "found no card" from "read
        // someone else's".
        app.today_summary_create
            .attempts
            .fetch_add(1, Ordering::Relaxed);
        // Armed only by the concurrency case; `None` in production.
        if let Some(barrier) = &app.today_summary_create_rendezvous {
            barrier.wait().await;
        }
        // The real handler, not a reimplementation of it. `CallerSuppliesItsOwn`: this path
        // already carries the day's counts in `summary_prompt` below; being briefed as well
        // would state them twice and leave a third `harness.user_message.enqueued` row.
        let created = create_track_conversation_inner(
            s.clone(),
            w.clone(),
            synthetic_actor(),
            headers,
            track_id.clone(),
            NewTrackConversationBody {
                text: TODAY_SUMMARY_BOOTSTRAP_TEXT.to_string(),
                model: None,
                reasoning_effort: None,
            },
            OpeningBriefing::CallerSuppliesItsOwn,
        )
        .await;
        // The create-409 fallback: conflict ⇒ resolve the derived card ⇒ carry on to the
        // planner input, if the card is in fact there. A concurrent request under the same key
        // can mint the card between the `card_get` above and this create, and the
        // payload-hash flavour of that 409 is permanent. A 409 with no card is re-raised unchanged.
        if let Err(error) = created {
            let card_exists = s.repo.card_get(&derived.card_id).await?.is_some();
            if !create_conflict_is_recoverable(&error, card_exists) {
                return Err(error);
            }
            app.today_summary_create
                .conflicts
                .fetch_add(1, Ordering::Relaxed);
            tracing::info!(
                card_id = %derived.card_id,
                %error,
                "today summary: create lost a race under the fixed key; the \
                 derived card exists, continuing to the planner input"
            );
        }
    }

    // The standing instruction has to reach the agent before the day's numbers do, if
    // nothing has spoken to the session live on this card. Under the per-card first-message
    // claim: two concurrent requests would both read "no user message yet" and both send.
    // Lock order: `conversation_first_message_locks` → `planner_recovery_locks` is the only
    // permitted nesting, and it is what happens here. At-least-once: the audit row is
    // written after the enqueue.
    {
        // Counts requests that reached this block; the barrier below is what creates the race.
        app.today_summary_create
            .bootstrap_arrivals
            .fetch_add(1, Ordering::Relaxed);
        // Armed only by the concurrency case; `None` in production. Outside the claim on
        // purpose: parking inside it would serialize the two requests before they can race.
        if let Some(barrier) = &app.today_summary_bootstrap_rendezvous {
            barrier.wait().await;
        }
        // Held across genuinely blocking work: on the dormant branch `send_summary` submits a
        // `planner-harness-start` and waits on it, so a wedged app-server holds this claim for
        // as long as that runs. The blast radius is one card.
        let _first_message_claim =
            lock_card(&s.conversation_first_message_locks, &derived.card_id).await;
        if !user_message_enqueued_on_active_runtime(&w, &track_id, &derived.card_id).await? {
            send_summary(
                &s,
                &w,
                &cs,
                &derived.card_id,
                TODAY_SUMMARY_BOOTSTRAP_TEXT.to_string(),
            )
            .await?;
        }
    }

    // Unconditional: this is the only channel the summary ever travels on.
    send_summary(&s, &w, &cs, &derived.card_id, summary_prompt(&activity)?).await?;

    Ok(Json(TodaySummaryStarted {
        track_id,
        card_id: derived.card_id,
    }))
}

/// Is a failed create one this handler may continue past? Only a `conflict` (anything
/// else means the create did not happen), and only if the card is there (a conflict
/// about anything else is still a conflict).
fn create_conflict_is_recoverable(error: &CalmError, card_exists: bool) -> bool {
    matches!(error, CalmError::Conflict(_)) && card_exists
}

/// Send the summary, recovering once from a dormant harness. The recovery re-submits
/// `planner-harness-start` and must NOT call `/planner/reset`, which hard-codes
/// `reset_harness_items: true` and would erase the transcript. The 503 states are
/// transient and are not recovered here.
async fn send_summary(
    s: &RouteState,
    w: &WorkerState,
    cs: &CodexShellState,
    card_id: &str,
    text: String,
) -> Result<()> {
    let send = |text: String| {
        send_planner_input(
            State(s.clone()),
            State(w.clone()),
            State(cs.clone()),
            synthetic_actor(),
            Path(card_id.to_string()),
            Json(SendPlannerInputRequest {
                text,
                attachments: Vec::new(),
            }),
        )
    };
    match send(text.clone()).await {
        Ok(_) => Ok(()),
        Err(CalmError::PlannerHarnessDormant(reason)) => {
            tracing::info!(
                card_id,
                reason,
                "today summary: harness dormant, re-submitting planner-harness-start"
            );
            restart_summary_harness(s, card_id).await?;
            send(text).await.map(|_| ())
        }
        Err(other) => Err(other),
    }
}

/// Re-open the summary conversation's harness without touching its transcript.
async fn restart_summary_harness(s: &RouteState, card_id: &str) -> Result<()> {
    let card = s
        .repo
        .card_get(card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    let track = s
        .repo
        .track_get(card.track_id.as_str())
        .await?
        .ok_or_else(|| {
            CalmError::NotFound(format!("track {} for card {card_id}", card.track_id))
        })?;
    let payload = serde_json::to_value(PlannerHarnessStartOperationPayload {
        // Constructed directly, because `Actor("kernel").to_actor_id()` falls through to
        // `ActorId::User`. Nobody asked for this restart, so it is the kernel's.
        actor: ActorId::Kernel,
        track_id: track.id.to_string(),
        planner_card_id: card.id.clone(),
        report_card_id: None,
        sort: None,
        cwd: track.workspace.path.clone(),
        goal: None,
        // `true` here is `/planner/reset`'s behaviour and would delete the conversation.
        reset_harness_items: false,
        force_new_thread: true,
        // This card is the assistant conversation this module minted; starting it as `Planner`
        // would give the thread the planner prompt while the card row still said `assistant`.
        profile: HarnessProfile::Assistant,
        create_card: None,
        first_message: None,
        create_request_sha256: None,
        opening_briefing: None,
    })?;
    run_planner_card_operation(s, "planner-harness-start", payload).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A golden on this module's function: mixing anything into the key changes these strings.
    #[test]
    fn the_summary_conversation_key_is_bare() {
        assert_eq!(TODAY_SUMMARY_CONVERSATION_KEY, "today-summary");
        let derived = summary_conversation_keys("track-1");
        assert_eq!(derived.card_id, "conv-afe76dc78204daed3ab52a9007298eb0");
        assert_eq!(
            derived.operation_key,
            "wave-conversation-afe76dc78204daed3ab52a9007298eb07f6e17761e2ec9da3718288dad41baff"
        );
    }

    #[test]
    fn create_conflict_is_recoverable_only_for_a_conflict_whose_card_exists() {
        let conflict = CalmError::Conflict("card already exists".into());
        let other = CalmError::ServiceUnavailable("app-server down".into());
        assert!(create_conflict_is_recoverable(&conflict, true));
        assert!(!create_conflict_is_recoverable(&conflict, false));
        assert!(!create_conflict_is_recoverable(&other, true));
        assert!(!create_conflict_is_recoverable(&other, false));
    }

    /// `i64::MIN` rather than a plausible count: the widest rendering is the longest
    /// negative integer.
    #[test]
    fn the_prompt_is_bounded_far_below_the_planner_input_ceiling() {
        let widest = summary_prompt(&WorkspaceActivityWindow {
            track_lifecycle_changed: i64::MIN,
            track_report_edited: i64::MIN,
            task_completed: i64::MIN,
            task_failed: i64::MIN,
            tracks_touched: i64::MIN,
        })
        .unwrap();
        assert!(
            widest.chars().count() < crate::routes::cards::MAX_PLANNER_INPUT_CHARS,
            "the prompt must fit `planner/input` for every possible count; it is \
             {} chars",
            widest.chars().count()
        );
        // The bootstrap travels the same channel under the identical ceiling.
        assert!(
            TODAY_SUMMARY_BOOTSTRAP_TEXT.chars().count()
                < crate::routes::cards::MAX_PLANNER_INPUT_CHARS
        );
        assert!(!TODAY_SUMMARY_BOOTSTRAP_TEXT.trim().is_empty());
    }
}
