//! #1253 D5 — `POST /api/today/summary`: ask an agent to write today's
//! progress into Today's document.
//!
//! This is the action the whole issue was opened for ("有一个 conversation 总结
//! 今天做了些什么"). The document is the launchpad wave's `wave-report` card
//! (design D1); the writer is an assistant conversation on that wave, which the
//! user can read in Today's Conversations module — it *is* the conversation
//! they asked for.
//!
//! # The endpoint is server-synthesised
//!
//! It takes no request body and no client prompt, and — deliberately — it does
//! not extract [`Actor`] from the request either. Both absences are load-bearing
//! and are explained where they bite: the prompt in [`summary_prompt`], the
//! actor in [`synthetic_actor`].
//!
//! # The order of operations, and why it is that order
//!
//! 1. compute the day's activity window (`activity_window`);
//! 2. **empty ⇒ refuse, having created nothing** (INV-TODAYDOC-007);
//! 3. ensure the launchpad exists;
//! 4. create the summary conversation *if it is not there yet*, with static
//!    bootstrap text and nothing else;
//! 5. **unconditionally** send one spec input carrying the activity summary.
//!
//! Step 2 comes before step 3 so that a refusal materialises no workspace and
//! starts no harness, not merely "no conversation".
//!
//! Steps 4 and 5 are **one path, not two branches**. An earlier revision gave
//! the summary to a "re-run" branch and let the create path carry only its
//! first message; because `create_wave_conversation` skips its send once
//! `user_message_already_enqueued` is true, that shape produced a summary with
//! no material on the very first use — the one impression the user gets. See
//! the design's §0c.1.

use axum::{
    Json, Router,
    extract::{FromRef, Path, State},
    http::{HeaderMap, HeaderValue},
    routing::post,
};
use serde::Serialize;
use utoipa::ToSchema;

use crate::activity_window::{WorkspaceActivityWindow, local_day_window, workspace_activity_window};
use crate::actor::Actor;
use crate::conversation_keys::{DerivedConversationKeys, derive_wave_conversation_keys};
use crate::error::{CalmError, ErrorBody, Result};
use crate::ids::ActorId;
use crate::model::now_ms;
use crate::operation::spec_harness_start_adapter::{
    HarnessProfile, SpecHarnessStartOperationPayload,
};
use crate::routes::cards::{SendSpecInputRequest, run_spec_card_operation, send_spec_input};
use crate::routes::today::ensure_today_launchpad;
use crate::routes::wave_conversations::{NewWaveConversationBody, create_wave_conversation};
use crate::state::{AppState, CodexShellState, RouteState, WorkerState};

pub fn router() -> Router<AppState> {
    Router::new().route("/api/today/summary", post(write_today_summary))
}

/// The `Idempotency-Key` the summary conversation is derived from.
///
/// **A bare constant. Nothing may be mixed in — not the workspace digest, not
/// the actor, not the date.** This has been got wrong once already and the
/// failure is quiet, so the reason is written out in full.
///
/// `derive_wave_conversation_keys` feeds one digest to **both** ids: the card is
/// `conv-{digest[..32]}` and the operation key is `wave-conversation-{digest}`
/// (`conversation_keys.rs`). So anything mixed into the key changes the **card
/// id** too. Mix in `workspace_key_digest(cwd)` and one workspace re-point
/// derives a *second* conversation card, which is precisely the thing D5 rules
/// out ("one summary conversation for the launchpad's lifetime"); worse, code
/// that looked the card up under the bare key and created it under the digest
/// key would create a card it never finds again.
///
/// The `today.rs` precedent that *does* mix a digest in does not transfer: its
/// key names an operation only. This one carries conversation identity.
///
/// What that precedent was defending against — `insert_operation`'s permanent
/// 409 on "same key, different payload hash", with no pruner on `operations` —
/// is instead handled by removing the variables from the payload, which is what
/// [`synthetic_actor`], the `card_get` branch and [`BOOTSTRAP_TEXT`] between
/// them do.
pub const TODAY_SUMMARY_CONVERSATION_KEY: &str = "today-summary";

/// The conversation this endpoint talks to, derived.
///
/// A named function rather than an inline call so that INV-TODAYDOC-011 has
/// something to pin: `the_summary_conversation_key_is_bare` below is a golden on
/// **this** function. Copying `conversation_keys.rs`'s own golden would prove
/// nothing here — that one stays green with every line of this module deleted.
///
/// It is also the only route from a wave id to a card id in this module, which
/// is what makes the golden a statement about the endpoint rather than about a
/// helper the endpoint might not use.
pub(crate) fn summary_conversation_keys(wave_id: &str) -> DerivedConversationKeys {
    derive_wave_conversation_keys(wave_id, TODAY_SUMMARY_CONVERSATION_KEY)
}

/// The derived summary-conversation card id, for tests that must assert the
/// endpoint landed on *the* card rather than on *a* card.
#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn today_summary_card_id_for_test(wave_id: &str) -> String {
    summary_conversation_keys(wave_id).card_id
}

/// The actor every request from this endpoint is attributed to.
///
/// **Fixed, and not read from the request.** `SpecHarnessStartOperationPayload`
/// carries `actor`, the whole payload is hashed into the operation's
/// `payload_hash`, and `insert_operation` answers "same idempotency key,
/// different hash" with a 409 that never expires — `operations` has no pruner.
/// `today.rs` records that exact accident verbatim: *"409, on every request,
/// forever"*. So the payload must not contain anything that varies between two
/// presses of one button.
///
/// The channel that actually varies is the client's `X-Calm-Actor` header:
/// `Actor::to_actor_id` maps `"user"` **and every non-`ai:codex` value** to
/// `ActorId::User`, so two human accounts cannot differ, but `ai:codex` can.
/// This handler closes it structurally — it has no `Actor` extractor at all, so
/// there is no header to forward and no future edit can quietly start
/// forwarding one.
///
/// **Why `user` and not `kernel`.** The message is composed by the server, but
/// the act is a human pressing a button, and that is what the audit log should
/// say. The alternative is also not reachable honestly: `send_spec_input`
/// derives its audit actor from the `Actor` it is handed, and
/// `Actor("kernel").to_actor_id()` silently degrades to `ActorId::User` — so
/// "attribute this to the kernel" would mean reimplementing the send rather
/// than calling it, and reimplementing it is how the two paths drift.
/// `ActorId::Kernel` is constructed directly in the one place that can: the
/// harness restart below, which submits its own operation payload.
fn synthetic_actor() -> Actor {
    Actor(Actor::DEFAULT.to_string())
}

/// The first message the summary conversation is ever sent — **static, forever**.
///
/// Not a formality. `POST /api/waves/{id}/conversations` binds this text into
/// the operation payload as a SHA-256 (arm (e)), so a date, a timestamp or the
/// activity counts in here would make every later retry under the same key a
/// 409. It is one of the three variables D5 has to eliminate to keep the
/// deterministic key safe; the other two are the actor above and `cwd`, which
/// cannot move because a re-point never re-enters the create branch (the card
/// exists by then, and it is `deletable: false`).
///
/// **Why it says "stand by".** A first press can run a bootstrap-only turn:
/// `UserMessage` is hard-fire, so if this message reaches an issuable drain
/// before the summary does, the agent takes a turn with only this in hand. An
/// agent told to write the report would then write one with no material. Told
/// to wait, it spends a harmless empty turn instead. Same purpose as
/// INV-TODAYDOC-007, one layer in.
const BOOTSTRAP_TEXT: &str = "You are this workspace's daily-progress writer. \
     Stand by and do nothing yet: do not read or touch the wave report until a \
     later message tells you the day's activity. When that message arrives, \
     rewrite the report in full following the maintenance contract carried in \
     its body.";

/// What the caller gets back on success.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TodaySummaryStarted {
    /// The launchpad wave, whose report the agent is being asked to rewrite.
    pub wave_id: String,
    /// The summary conversation's card. Stable for the launchpad's lifetime
    /// (INV-TODAYDOC-011) and openable in Today's Conversations module.
    pub card_id: String,
}

/// Render the prompt. Template text plus five integers, and that is the whole
/// contract.
///
/// The length bound is why: `send_spec_input` rejects anything over
/// `MAX_SPEC_INPUT_CHARS` (32,768), and a prompt built from a fixed template
/// and five `i64`s has a maximum length that can be computed by reading it —
/// `the_prompt_is_bounded_far_below_the_spec_input_ceiling` computes it. Adding
/// wave titles or a detail list would remove that property, and the design says
/// what re-adding it would then cost (a deterministic character budget plus
/// 32,768/32,769/CJK boundary cases).
///
/// The counts are stated as counts, and the prompt says so: the agent has no
/// way to query for more (design D4 deleted that layer), so a prompt implying
/// it could would be an instruction to hallucinate.
fn summary_prompt(activity: &WorkspaceActivityWindow) -> String {
    format!(
        "Write today's progress into this wave's report card, following the \
         maintenance contract carried in the report body: it is a snapshot of \
         now, it has four fixed sections, and each write REWRITES it in full.\n\
         \n\
         Today's activity across the workspace, counted by the server. These \
         counts are all the activity data available to you — there is no tool \
         to query for more, so write about what these numbers plus the \
         workspace itself support, and do not invent specifics:\n\
         - waves whose lifecycle changed: {}\n\
         - report edits: {}\n\
         - tasks completed: {}\n\
         - tasks failed: {}\n\
         - distinct waves touched: {}\n",
        activity.wave_lifecycle_changed,
        activity.wave_report_edited,
        activity.task_completed,
        activity.task_failed,
        activity.waves_touched,
    )
}

#[utoipa::path(
    post,
    path = "/api/today/summary",
    tag = "waves",
    responses(
        (status = 200, description = "The summary conversation has been asked to write today's progress. The conversation is created on first use and reused thereafter; the reply arrives asynchronously as a report edit, not in this response.", body = TodaySummaryStarted),
        (status = 409, description = "Distinguished by the body's `code`:\n* `today_summary_no_activity` — nothing happened in the workspace today, so no conversation was created and no message was sent (INV-TODAYDOC-007).\n* `conflict` / `spec_harness_dormant` — from the underlying conversation create or spec input; a dormant harness is retried once automatically before it can reach here.", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
        (status = 503, description = "Shared codex app-server not running, a harness start is still in flight, or the observation queue is saturated — retry shortly", body = ErrorBody),
    ),
)]
/// Ask the summary conversation to write today's progress.
///
/// See the module docs for the shape of the whole path; the comments below only
/// say what each step's alternative got wrong.
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

    // Read the launchpad, do NOT ensure it: the reflexive exclusion needs its
    // id when it has one, and an absent launchpad simply excludes nothing.
    // Ensuring here would mean an empty day still materialized a workspace and
    // started a harness — the thing step 2 exists to avoid.
    let launchpad = app.repo.wave_get_launchpad().await?;
    let (start_ms, end_ms) = local_day_window(now_ms());
    let activity = workspace_activity_window(
        &pool,
        start_ms,
        end_ms,
        launchpad.as_ref().map(|wave| wave.id.as_str()),
    )
    .await?;

    // INV-TODAYDOC-007's enforcement point, and it is here rather than in the
    // frontend on purpose: hiding the button is UI, and a POST straight at this
    // endpoint would sail past it. The statement is deliberately narrow — it is
    // about *this* endpoint. `POST /api/waves/{id}/conversations` and
    // `POST /api/cards/{id}/spec/input` stay reachable and are not in scope: a
    // user typing to an agent by hand is not the thing being prevented.
    if activity.is_empty() {
        return Err(CalmError::TodaySummaryNoActivity(
            "nothing happened in this workspace today, so there is nothing to \
             summarise; no conversation was created and no message was sent"
                .into(),
        ));
    }

    // Idempotent, and the only bootstrap on this path. It materializes the
    // workspace and waits on a `spec-harness-start`, which is exactly why the
    // page-load resolve must never call it (INV-TODAYDOC-001) and why this —
    // an explicit action — is where it belongs (§5.1).
    let (_status, Json(launchpad)) =
        ensure_today_launchpad(State(app.clone()), synthetic_actor()).await?;
    let wave_id = launchpad.wave_id;
    let derived = summary_conversation_keys(&wave_id);

    // **Branch on the card, never on a list or a heuristic.** A `Stuck`
    // compensation leaves the derived card behind with no runtime, and that
    // card is `deletable: false` so the user cannot clear it. A "does the wave
    // have any assistant conversation?" test would pick the create branch on a
    // wave that already holds the card and get a 409 from `validate`; a "did we
    // ever succeed?" test cannot see that state at all. `card_get` on the
    // derived id answers the only question that matters.
    if s.repo.card_get(&derived.card_id).await?.is_none() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "idempotency-key",
            HeaderValue::from_static(TODAY_SUMMARY_CONVERSATION_KEY),
        );
        // The real handler, not a reimplementation of it: the mint, the
        // derived-id guard, the four retry arms and the first-message claim all
        // have to be the ones production uses.
        let _created = create_wave_conversation(
            State(s.clone()),
            State(w.clone()),
            State(cs.clone()),
            synthetic_actor(),
            headers,
            Path(wave_id.clone()),
            Json(NewWaveConversationBody {
                text: BOOTSTRAP_TEXT.to_string(),
            }),
        )
        .await?;
    }

    // Unconditional. This is the only channel the summary ever travels on, and
    // the create above never carries it.
    send_summary(&s, &w, &cs, &derived.card_id, summary_prompt(&activity)).await?;

    Ok(Json(TodaySummaryStarted {
        wave_id,
        card_id: derived.card_id,
    }))
}

/// Send the summary, recovering once from a dormant harness.
///
/// `ensure_live_spec_harness` answers 409 `spec_harness_dormant` for three
/// states — no active runtime, no thread, unreadable snapshot — and with one
/// long-lived conversation any of them would kill this button permanently: there
/// is no other path back to a live session.
///
/// **The recovery re-submits `spec-harness-start`; it must NOT call
/// `/spec/reset`.** The two are not equivalent and the difference is exactly
/// what the user came for: `reset_spec_harness_card` hard-codes
/// `reset_harness_items: true`, which erases the transcript — and that
/// transcript is the conversation this whole feature exists to produce. A plain
/// start with `reset_harness_items: false` and `force_new_thread: true` restores
/// the session and keeps every message. It cannot be short-circuited by
/// idempotency either: it submits under a fresh `operation_key` with no
/// idempotency key, exactly as the reset path's own start does.
///
/// The 503 states (`Starting`, shared app-server down, saturated observation
/// queue) are **not** recovered here. They are transient by construction and
/// surface as 503 so the caller retries — restarting a harness that is already
/// starting would be the wrong move.
async fn send_summary(
    s: &RouteState,
    w: &WorkerState,
    cs: &CodexShellState,
    card_id: &str,
    text: String,
) -> Result<()> {
    let send = |text: String| {
        send_spec_input(
            State(s.clone()),
            State(w.clone()),
            State(cs.clone()),
            synthetic_actor(),
            Path(card_id.to_string()),
            Json(SendSpecInputRequest { text }),
        )
    };
    match send(text.clone()).await {
        Ok(_) => Ok(()),
        Err(CalmError::SpecHarnessDormant(reason)) => {
            tracing::info!(
                card_id,
                reason,
                "today summary: harness dormant, re-submitting spec-harness-start"
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
    let wave = s
        .repo
        .wave_get(card.wave_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {} for card {card_id}", card.wave_id)))?;
    let payload = serde_json::to_value(SpecHarnessStartOperationPayload {
        // Constructed directly, because `Actor::to_actor_id` cannot produce it:
        // `Actor("kernel")` falls through to `ActorId::User`. This restart is
        // not the user's act — nobody asked for it — so it is the kernel's.
        actor: ActorId::Kernel,
        wave_id: wave.id.to_string(),
        spec_card_id: card.id.clone(),
        report_card_id: None,
        sort: None,
        cwd: wave.workspace.path.clone(),
        goal: None,
        // The whole point. `true` here is `/spec/reset`'s behaviour and would
        // delete the conversation.
        reset_harness_items: false,
        force_new_thread: true,
        // This card is the assistant conversation this module minted, so its
        // profile is known rather than inferred. Starting it as `Spec` would
        // give the thread the spec prompt and role while the card row still
        // said `assistant`.
        profile: HarnessProfile::Assistant,
        create_card: None,
        first_message_sha256: None,
    })?;
    run_spec_card_operation(s, "spec-harness-start", payload).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// INV-TODAYDOC-011 — the key this endpoint derives from is the bare
    /// constant, so the card id depends on the wave and on nothing else.
    ///
    /// A golden, and a golden on **this module's** function: mixing anything
    /// into the key — a workspace digest, an actor, a date — changes these two
    /// strings, and both would still satisfy a round-trip check like
    /// `keys(w) == keys(w)`. `conversation_keys.rs`'s own golden is not a
    /// substitute: it pins the derivation, which is not the decision being made
    /// here, and it stays green with this whole file deleted.
    ///
    /// What it cannot see is a second caller that derives its own id without
    /// going through `summary_conversation_keys`. That is covered end to end
    /// instead, by `today_summary::a_repointed_workspace_reuses_the_one_summary_conversation`,
    /// which drives the endpoint from two servers with different workspace
    /// roots and asserts one card.
    #[test]
    fn the_summary_conversation_key_is_bare() {
        assert_eq!(TODAY_SUMMARY_CONVERSATION_KEY, "today-summary");
        let derived = summary_conversation_keys("wave-1");
        assert_eq!(derived.card_id, "conv-fc3a9cd32d1edca695e58ec734b27ec5");
        assert_eq!(
            derived.operation_key,
            "wave-conversation-fc3a9cd32d1edca695e58ec734b27ec52f64dcfb2abe0a43a8f192f4ec10917d"
        );
    }

    /// The prompt's length is bounded by reading it, which is what lets D4 drop
    /// the truncation discipline the MCP layer would have needed.
    ///
    /// `i64::MIN` rather than a plausible count: the bound has to hold for every
    /// value the type admits, and the widest rendering is the longest negative
    /// integer. (Counts cannot go negative — `COUNT(*)` — so this is the bound,
    /// not a case.)
    #[test]
    fn the_prompt_is_bounded_far_below_the_spec_input_ceiling() {
        let widest = summary_prompt(&WorkspaceActivityWindow {
            wave_lifecycle_changed: i64::MIN,
            wave_report_edited: i64::MIN,
            task_completed: i64::MIN,
            task_failed: i64::MIN,
            waves_touched: i64::MIN,
        });
        assert!(
            widest.chars().count() < crate::routes::cards::MAX_SPEC_INPUT_CHARS,
            "the prompt must fit `spec/input` for every possible count; it is \
             {} chars",
            widest.chars().count()
        );
        // The bootstrap travels the same channel and is validated by
        // `validate_first_message` under the identical ceiling.
        assert!(BOOTSTRAP_TEXT.chars().count() < crate::routes::cards::MAX_SPEC_INPUT_CHARS);
        assert!(!BOOTSTRAP_TEXT.trim().is_empty());
    }
}
