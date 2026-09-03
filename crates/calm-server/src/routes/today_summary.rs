//! #1253 D5 — `POST /api/today/summary`: ask an agent to write today's
//! progress into Today's document.
//!
//! This is the action the whole issue was opened for ("有一个 conversation 总结
//! 今天做了些什么"). The document is the launchpad track's `track-report` card
//! (design D1); the writer is an assistant conversation on that track, which the
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
//! 5. **unconditionally** send one planner input carrying the activity summary.
//!
//! Step 2 comes before step 3 so that a refusal materialises no workspace and
//! starts no harness, not merely "no conversation".
//!
//! Steps 4 and 5 are **one path, not two branches**. An earlier revision gave
//! the summary to a "re-run" branch and let the create path carry only its
//! first message; because `create_track_conversation` skips its send once
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
use std::sync::atomic::{AtomicU64, Ordering};
use utoipa::ToSchema;

use crate::activity_window::{
    WorkspaceActivityWindow, local_day_window, workspace_activity_window,
};
use crate::actor::Actor;
use crate::conversation_keys::{DerivedConversationKeys, derive_track_conversation_keys};
use crate::error::{CalmError, ErrorBody, Result};
use crate::ids::ActorId;
use crate::model::now_ms;
use crate::operation::planner_harness_start_adapter::{
    HarnessProfile, PlannerHarnessStartOperationPayload,
};
use crate::per_card_lock::lock_card;
use crate::routes::cards::{
    SendPlannerInputRequest, run_planner_card_operation, send_planner_input,
};
use crate::routes::conversations_shared::user_message_already_enqueued;
use crate::routes::today::ensure_today_launchpad;
use crate::routes::track_conversations::{NewTrackConversationBody, create_track_conversation};
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
/// `derive_track_conversation_keys` feeds one digest to **both** ids: the card is
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
/// [`synthetic_actor`], the `card_get` branch and [`TODAY_SUMMARY_BOOTSTRAP_TEXT`] between
/// them do.
pub const TODAY_SUMMARY_CONVERSATION_KEY: &str = "today-summary";

/// The conversation this endpoint talks to, derived.
///
/// A named function rather than an inline call so that INV-TODAYDOC-011 has
/// something to pin: `the_summary_conversation_key_is_bare` below is a golden on
/// **this** function. Copying `conversation_keys.rs`'s own golden would prove
/// nothing here — that one stays green with every line of this module deleted.
///
/// It is also the only route from a track id to a card id in this module, which
/// is what makes the golden a statement about the endpoint rather than about a
/// helper the endpoint might not use.
pub(crate) fn summary_conversation_keys(track_id: &str) -> DerivedConversationKeys {
    derive_track_conversation_keys(track_id, TODAY_SUMMARY_CONVERSATION_KEY)
}

/// The derived summary-conversation card id, for tests that must assert the
/// endpoint landed on *the* card rather than on *a* card.
#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn today_summary_card_id_for_test(track_id: &str) -> String {
    summary_conversation_keys(track_id).card_id
}

/// The actor every request from this endpoint is attributed to.
///
/// **Fixed, and not read from the request.** `PlannerHarnessStartOperationPayload`
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
/// say. The alternative is also not reachable honestly: `send_planner_input`
/// derives its audit actor from the `Actor` it is handed, and
/// `Actor("kernel").to_actor_id()` silently degrades to `ActorId::User` — so
/// "attribute this to the kernel" would mean reimplementing the send rather
/// than calling it, and reimplementing it is how the two paths drift.
/// `ActorId::Kernel` is constructed directly in the one place that can: the
/// harness restart below, which submits its own operation payload.
fn synthetic_actor() -> Actor {
    Actor(Actor::DEFAULT.to_string())
}

/// The standing instruction the summary conversation is opened with.
///
/// **Not "the first message ever sent", and the claim was wrong when it said
/// so.** What the code does is narrower and is stated exactly:
///
/// > this text is sent when the derived card has **no
/// > `harness.user_message.enqueued` row at all**, and never again once any
/// > such row exists.
///
/// So a user who types into this conversation before the first trigger
/// suppresses it **permanently** — the card is `deletable: false` and the
/// evidence row is permanent, so the suppression never lifts. That is a
/// deliberate ruling, and the reason it is acceptable is the only reason this
/// text exists at all.
///
/// **What the bootstrap is for.** `UserMessage` is hard-fire, so if it reaches
/// an issuable drain before the summary does, the agent takes a turn holding
/// only it. An agent told "write the report" would write one with no material;
/// told to stand by, it spends a harmless empty turn. The hazard is therefore
/// *specifically* "the agent's first turn happens with no material". If any
/// other message already preceded the summary, that turn has already happened
/// with something else in hand, and this text has nothing left to protect.
///
/// **Why not a bootstrap-aware predicate.** Researched rather than assumed, and
/// none of the three candidates beats the kind-level read:
///
/// * `harness.user_message.enqueued` carries `char_count` and no text
///   (`calm-types/src/event.rs`), so matching a length is a collision, not a
///   predicate.
/// * `harness_items` does hold the bytes, but it is written when **codex echoes
///   the turn back**, not when the message is enqueued. Two triggers before
///   that echo lands would both read "not delivered" and both send — turning a
///   rare suppressed bootstrap into a routine duplicated one, which is the
///   failure the per-card claim below exists to prevent. It is also erased by
///   `/planner/reset` and by legacy-Today adoption.
/// * A new marker means either a write-only flag — which
///   `conversations_shared::user_message_already_enqueued`'s own docs reject as
///   "wrong in one direction either way" — or adding a text digest to a shared
///   event kind used by two other endpoints, which is an event-version bump
///   plus frontend schema and goldens, and is outside this slice.
///
/// **What the text must still be: static, byte-for-byte, forever.**
/// `POST /api/tracks/{id}/conversations` binds it into the operation payload as
/// a SHA-256 (arm (e)), so a date, a timestamp or the activity counts in here
/// would make every later retry under the same key a 409. It is one of the
/// three variables D5 has to eliminate to keep the deterministic key safe; the
/// other two are the actor above and `cwd`.
///
/// **What is true about `cwd`, stated narrowly.** Once the derived card exists,
/// the create arm is not entered again, so a workspace re-point after that
/// point cannot resubmit the key under a new payload. It does **not** hold
/// before the card exists: a create that lands `Stuck` *without* leaving the
/// card is not stepped over by `retryable_operation_key` (which appends `#N`
/// only for `Phase::Failed`), so a re-point followed by a press resubmits the
/// same key with a different hash and 409s permanently. That window is narrow,
/// fail-closed and already named in D5's residual-window paragraph; what it is
/// not is impossible, and an earlier wording here said it was.
pub const TODAY_SUMMARY_BOOTSTRAP_TEXT: &str = "You are this workspace's daily-progress writer. \
     Stand by and do nothing yet: do not read or touch the track report until a \
     later message tells you the day's activity. When that message arrives, \
     rewrite the report in full following the maintenance contract carried in \
     its body.";

/// A rendezvous the create-under-a-fixed-key race can be **created** at.
///
/// `None` in production — the create arm costs one `Option` check and never
/// waits. A test arms it with a `Barrier::new(2)`; both requests then park here
/// after their `card_get` has returned `None` and before either submits, so the
/// second provably cannot see the first one's card and must take the 409
/// fallback.
///
/// **Why it exists at all.** The fallback's window is one request wide and
/// `tokio::join!` does not order two requests, so a case that merely fired two
/// and hoped would be green on a scheduler that serialises them — reporting
/// success for a run in which the arm was never entered. Our box is not an
/// environment that can falsify that; a CI runner is. The same reasoning, and
/// the same shape, as `routes::today`'s [`SystemAreaMintRendezvous`], down to
/// living on [`AppState`] rather than in a `static`: a process-global is shared
/// by every `AppState` in the process, which a threaded `cargo test` turns into
/// cross-case interference.
///
/// [`SystemAreaMintRendezvous`]: crate::routes::today::SystemAreaMintRendezvous
/// [`AppState`]: crate::state::AppState
pub type TodaySummaryCreateRendezvous = Option<std::sync::Arc<tokio::sync::Barrier>>;

/// A rendezvous the **first-message** race can be created at.
///
/// Separate from [`TodaySummaryCreateRendezvous`] because it guards a different
/// window at a different point in the handler, and a single barrier serving
/// both would be waited on twice by one request in the create case and hang.
/// `None` in production, same as its sibling.
pub type TodaySummaryBootstrapRendezvous = Option<std::sync::Arc<tokio::sync::Barrier>>;

/// Per-server observation of the create arm.
///
/// **`attempts` is what makes the race deterministic rather than hoped for**,
/// and it is incremented *before* the rendezvous for exactly the reason
/// `SystemAreaMintCounters::attempts` is: it lets a test know a request has
/// passed `card_get` and found nothing, which is the only moment at which
/// planting a conflicting card is guaranteed to produce the 409 the fallback
/// exists for. Without it the test would have to guess when to act, and
/// guessing wrong makes the case pass while never entering the arm.
///
/// **`conflicts` is not the assertion, it is the assertion's validity.** Every
/// outcome the case checks — both requests 200, one conversation card, the
/// right enqueued rows — is equally true of a run in which the fallback never
/// ran. This is what tells the two apart.
///
/// Unconditional rather than `fixtures`-gated, for the reason the sibling
/// counters give: a case about ordering has to execute the instructions
/// production executes, and the cost is two relaxed atomic adds on a path taken
/// at most once per launchpad.
#[derive(Debug, Default)]
pub struct TodaySummaryCreateCounters {
    /// Requests that found no derived card and therefore entered the create arm.
    pub attempts: AtomicU64,
    /// Creates that lost the key race and took D5's 409 fallback.
    pub conflicts: AtomicU64,
    /// Requests that reached the bootstrap decision block.
    ///
    /// It is incremented *before* the transcript is read, so it does **not**
    /// witness "both requests saw an empty transcript" — an earlier comment
    /// claimed that and two review channels independently caught it. What
    /// creates the race is the rendezvous; this only says how many requests got
    /// as far as the block.
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
    /// The summary conversation's card. Stable for the launchpad's lifetime
    /// (INV-TODAYDOC-011) and openable in Today's Conversations module.
    pub card_id: String,
}

/// Render the prompt. Template text plus five integers, and that is the whole
/// contract.
///
/// The length bound is why: `send_planner_input` rejects anything over
/// `MAX_PLANNER_INPUT_CHARS` (32,768), and a prompt built from a fixed template
/// and five `i64`s has a maximum length that can be computed by reading it —
/// `the_prompt_is_bounded_far_below_the_planner_input_ceiling` computes it. Adding
/// track titles or a detail list would remove that property, and the design says
/// what re-adding it would then cost (a deterministic character budget plus
/// 32,768/32,769/CJK boundary cases).
///
/// The counts are stated as counts, and the prompt says so: the agent has no
/// way to query for more (design D4 deleted that layer), so a prompt implying
/// it could would be an instruction to hallucinate.
fn summary_prompt(activity: &WorkspaceActivityWindow) -> String {
    format!(
        "Write today's progress into this track's report card, following the \
         maintenance contract carried in the report body: it is a snapshot of \
         now, it has four fixed sections, and each write REWRITES it in full.\n\
         \n\
         Today's activity across the workspace, counted by the server. These \
         counts are all the activity data available to you — there is no tool \
         to query for more, so write about what these numbers plus the \
         workspace itself support, and do not invent specifics:\n\
         - tracks whose lifecycle changed: {}\n\
         - report edits: {}\n\
         - tasks completed: {}\n\
         - tasks failed: {}\n\
         - distinct tracks touched: {}\n",
        activity.track_lifecycle_changed,
        activity.track_report_edited,
        activity.task_completed,
        activity.task_failed,
        activity.tracks_touched,
    )
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
    let launchpad = app.repo.track_get_launchpad().await?;
    let (start_ms, end_ms) = local_day_window(now_ms());
    let activity = workspace_activity_window(
        &pool,
        start_ms,
        end_ms,
        launchpad.as_ref().map(|track| track.id.as_str()),
    )
    .await?;

    // INV-TODAYDOC-007's enforcement point, and it is here rather than in the
    // frontend on purpose: hiding the button is UI, and a POST straight at this
    // endpoint would sail past it. The statement is deliberately narrow — it is
    // about *this* endpoint. `POST /api/tracks/{id}/conversations` and
    // `POST /api/cards/{id}/planner/input` stay reachable and are not in scope: a
    // user typing to an agent by hand is not the thing being prevented.
    if activity.is_empty() {
        return Err(CalmError::TodaySummaryNoActivity(
            "nothing happened in this workspace today, so there is nothing to \
             summarise; no conversation was created and no message was sent"
                .into(),
        ));
    }

    // Idempotent, and the only bootstrap on this path. It materializes the
    // workspace and waits on a `planner-harness-start`, which is exactly why the
    // page-load resolve must never call it (INV-TODAYDOC-001) and why this —
    // an explicit action — is where it belongs (§5.1).
    let (_status, Json(launchpad)) =
        ensure_today_launchpad(State(app.clone()), synthetic_actor()).await?;
    let track_id = launchpad.track_id;
    let derived = summary_conversation_keys(&track_id);

    // **The branch predicate is "the card exists AND it has ever been sent a
    // message", not "the card exists".** Two review channels found the gap from
    // opposite ends, and both end in the same place: a derived card that exists
    // with an empty transcript.
    //
    // * The create operation lands `Stuck`. `plan_compensation` marks it on the
    //   first compensation error and never re-drives it, leaving the card
    //   behind (`deletable: false`, so the user cannot clear it) with no
    //   runtime and no first message.
    // * The create operation *succeeds* — card and harness minted — and then
    //   `create_track_conversation`'s own first `send_planner_input` fails (a 503
    //   from a shared app-server that went down in between). It returns `Err`,
    //   so no summary is sent either, and the card is left with an empty
    //   transcript.
    //
    // Under a card-only predicate the next press skips the create arm and sends
    // only the summary. Two things then break at once: the *first successful*
    // trigger leaves ONE `harness.user_message.enqueued` row where
    // INV-TODAYDOC-010 requires two, and [`TODAY_SUMMARY_BOOTSTRAP_TEXT`] — the
    // standing instruction that keeps a bootstrap-only turn from writing a
    // report with no material — is never delivered at all, permanently.
    //
    // So the transcript is consulted, through the same read the create arm
    // itself uses to decide whether to send (`user_message_already_enqueued`),
    // and it is read *after* the create arm rather than inside its condition:
    // that way one statement covers both entrances, plus the ordinary one where
    // the card was minted seconds ago by this very request.
    //
    // The recovery is NOT "call `create_track_conversation` again". Against an
    // existing card the adapter's `validate` refuses to re-mint and answers 409
    // — correctly, since the card is not the thing missing. What is missing is
    // the message, so the message is what gets sent, down the one channel that
    // sends messages.
    if s.repo.card_get(&derived.card_id).await?.is_none() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "idempotency-key",
            HeaderValue::from_static(TODAY_SUMMARY_CONVERSATION_KEY),
        );
        // Counted before the rendezvous, so a test can tell "this request found
        // no card" from "it read someone else's". See
        // [`TodaySummaryCreateCounters`].
        app.today_summary_create
            .attempts
            .fetch_add(1, Ordering::Relaxed);
        // Armed only by the concurrency case; `None` in production, where this
        // is one `Option` check on a path taken once per launchpad. See
        // [`TodaySummaryCreateRendezvous`] for why the 409 window below has to
        // be created rather than waited for.
        if let Some(barrier) = &app.today_summary_create_rendezvous {
            barrier.wait().await;
        }
        // The real handler, not a reimplementation of it: the mint, the
        // derived-id guard, the four retry arms and the first-message claim all
        // have to be the ones production uses.
        let created = create_track_conversation(
            State(s.clone()),
            State(w.clone()),
            State(cs.clone()),
            synthetic_actor(),
            headers,
            Path(track_id.clone()),
            Json(NewTrackConversationBody {
                text: TODAY_SUMMARY_BOOTSTRAP_TEXT.to_string(),
            }),
        )
        .await;
        // D5's create-409 fallback: **conflict ⇒ resolve the derived card ⇒
        // carry on to the planner input**, if the card is in fact there.
        //
        // The window is real and is exactly one request wide: between the
        // `card_get` above and this create, a concurrent request under the same
        // key can mint the card. Ours then loses on either wall the adapter
        // has — `validate` refusing to re-mint an existing card, or
        // `insert_operation` refusing the same idempotency key under a
        // different payload hash — and both answer 409 `conflict`. Failing
        // outright there would be wrong twice over: the state the caller asked
        // for now exists, and the payload-hash flavour is permanent
        // (`operations` has no pruner), so the button would stay dead forever.
        //
        // The `card_get` re-read is the whole condition, and it is fail-closed:
        // a 409 with no card is a conflict about something else and is
        // re-raised unchanged.
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

    // Whatever route got us here, the standing instruction has to reach the
    // agent before the day's numbers do — *if nothing has spoken to this card
    // yet at all*. The predicate is "has any user message ever been enqueued",
    // not "was the bootstrap delivered", and
    // [`TODAY_SUMMARY_BOOTSTRAP_TEXT`] says why that is the right question
    // rather than a cheap proxy: the hazard is a first turn taken with no
    // material, and any earlier message has already spent it.
    //
    // **Two limits of the evidence, both inherited and neither hidden.**
    // `send_planner_input` enqueues the observation and *then* writes the
    // `harness.user_message.enqueued` row (`routes/cards.rs`), so a send whose
    // audit write fails leaves the agent holding the message with no row, and
    // the next trigger sends it again. And the row is written per enqueue, not
    // per delivery, so it says "queued", not "the model saw it" — the run loop
    // can still drop an observation on a full queue. Both are properties of
    // shared production code this slice does not touch; what would be wrong is
    // claiming an exactly-once guarantee on top of them, so: at-least-once,
    // deduplicated by a permanent row in the ordinary case.
    //
    // **Under the per-card first-message claim, and that is not optional.**
    // This is the same read-then-send `create_track_conversation` performs, and
    // both paths need the same claim for
    // the same reason: two concurrent requests both read "no user message yet"
    // and both send, so the agent gets the same standing instruction twice.
    // Moving this step out of `create_track_conversation` and into here moved it
    // out from under that lock; measured, two concurrent triggers against a
    // card with an empty transcript delivered two bootstraps.
    //
    // The window is open **only** in the empty-transcript state, which makes it
    // worse rather than better: an ordinary double-click on a first trigger is
    // serialized by the create arm's own idempotency, while the state this
    // recovery exists for is persistent — the card is `deletable: false` and
    // never goes away.
    //
    // Lock order, which this path must not be the one to break:
    // `conversation_first_message_locks` → `planner_recovery_locks` is the only
    // permitted nesting (see the field's docs), and it is what happens here —
    // `send_summary` → `send_planner_input` → `ensure_live_planner_harness` takes the
    // recovery lock on a registry miss. The dormant restart nested inside also
    // submits an operation, whose adapter takes its own private per-card mint
    // locks; nothing in the tree takes those and then this claim, so that
    // nesting closes no cycle.
    {
        // Counts requests that reached this block. That is ALL it proves — not
        // that two requests observed an empty transcript, which it cannot know:
        // it increments before `user_message_already_enqueued` runs. **The
        // barrier below is what creates the race**; the counter is a cheap
        // sanity check that the arm was entered the expected number of times,
        // and it is close to vacuous on its own, because a `Barrier::new(2)`
        // with a missing partner parks forever and no assertion is ever
        // reached.
        app.today_summary_create
            .bootstrap_arrivals
            .fetch_add(1, Ordering::Relaxed);
        // Armed only by the concurrency case; `None` in production. Outside the
        // claim on purpose: parking inside it would serialize the two requests
        // before they can race, which is the one thing the case must not do.
        if let Some(barrier) = &app.today_summary_bootstrap_rendezvous {
            barrier.wait().await;
        }
        // Held across genuinely blocking work, which is worth stating because
        // the obvious assumption is the opposite: on the dormant branch
        // `send_summary` submits a `planner-harness-start` and waits on it, and
        // that operation performs a codex `thread/start` RPC. So a slow or
        // wedged app-server holds this claim for as long as the operation runs.
        // The blast radius is one card — the map is per-card and every other
        // taker of it works on a different conversation — and the nesting is
        // still the permitted one, but "only cheap work happens under the
        // claim" is not true here.
        let _first_message_claim =
            lock_card(&s.conversation_first_message_locks, &derived.card_id).await;
        if !user_message_already_enqueued(&w, &track_id, &derived.card_id).await? {
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

    // Unconditional. This is the only channel the summary ever travels on, and
    // the create above never carries it.
    send_summary(&s, &w, &cs, &derived.card_id, summary_prompt(&activity)).await?;

    Ok(Json(TodaySummaryStarted {
        track_id,
        card_id: derived.card_id,
    }))
}

/// Is a failed create one this handler may continue past?
///
/// Both conjuncts are load-bearing and they fail closed in different
/// directions. **Only a `conflict`**: every other failure — a 503 from a
/// shared app-server that is down, a `Stuck` operation's 500, a `BadRequest` —
/// means the create did not happen and nothing is there to carry on to, so it
/// has to reach the caller as itself rather than be re-shaped into a 404 from a
/// send against a card that was never minted. **And only if the card is
/// there**: a conflict about anything else is still a conflict.
///
/// Extracted so the truth table can be pinned
/// (`create_conflict_is_recoverable_only_for_a_conflict_whose_card_exists`).
///
/// **A named gap, stated plainly rather than papered over.** That test binds
/// this function; it does not bind the *call site*. Replacing the call with a
/// bare `true` leaves it green — measured, 8/8 — and no behavioural case
/// catches it either, because the two states that would distinguish it are not
/// constructible in-process without bypassing the production create route: a
/// create that fails with a non-conflict *after* the launchpad's own
/// `planner-harness-start` has already succeeded, and a permanent payload-hash 409
/// under a key whose card does not exist. Do not claim the concurrency case
/// covers this; it exercises the recoverable direction only.
fn create_conflict_is_recoverable(error: &CalmError, card_exists: bool) -> bool {
    matches!(error, CalmError::Conflict(_)) && card_exists
}

/// Send the summary, recovering once from a dormant harness.
///
/// `ensure_live_planner_harness` answers 409 `planner_harness_dormant` for three
/// states — no active runtime, no thread, unreadable snapshot — and with one
/// long-lived conversation any of them would kill this button permanently: there
/// is no other path back to a live session.
///
/// **The recovery re-submits `planner-harness-start`; it must NOT call
/// `/planner/reset`.** The two are not equivalent and the difference is exactly
/// what the user came for: `reset_planner_harness_card` hard-codes
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
        send_planner_input(
            State(s.clone()),
            State(w.clone()),
            State(cs.clone()),
            synthetic_actor(),
            Path(card_id.to_string()),
            Json(SendPlannerInputRequest { text }),
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
        // Constructed directly, because `Actor::to_actor_id` cannot produce it:
        // `Actor("kernel")` falls through to `ActorId::User`. This restart is
        // not the user's act — nobody asked for it — so it is the kernel's.
        actor: ActorId::Kernel,
        track_id: track.id.to_string(),
        planner_card_id: card.id.clone(),
        report_card_id: None,
        sort: None,
        cwd: track.workspace.path.clone(),
        goal: None,
        // The whole point. `true` here is `/planner/reset`'s behaviour and would
        // delete the conversation.
        reset_harness_items: false,
        force_new_thread: true,
        // This card is the assistant conversation this module minted, so its
        // profile is known rather than inferred. Starting it as `Planner` would
        // give the thread the planner prompt and role while the card row still
        // said `assistant`.
        profile: HarnessProfile::Assistant,
        create_card: None,
        first_message_sha256: None,
    })?;
    run_planner_card_operation(s, "planner-harness-start", payload).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// INV-TODAYDOC-011 — the key this endpoint derives from is the bare
    /// constant, so the card id depends on the track and on nothing else.
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
        let derived = summary_conversation_keys("track-1");
        assert_eq!(derived.card_id, "conv-afe76dc78204daed3ab52a9007298eb0");
        assert_eq!(
            derived.operation_key,
            "wave-conversation-afe76dc78204daed3ab52a9007298eb07f6e17761e2ec9da3718288dad41baff"
        );
    }

    /// D5's create-409 fallback is *only* for a conflict whose card is there.
    ///
    /// All four cells, because each is a different failure. A non-conflict must
    /// surface as itself — swallowing a 503 turns "the agent service is down"
    /// into a 404 from a send against a card that was never minted — and a
    /// conflict with no card is a conflict about something else.
    #[test]
    fn create_conflict_is_recoverable_only_for_a_conflict_whose_card_exists() {
        let conflict = CalmError::Conflict("card already exists".into());
        let other = CalmError::ServiceUnavailable("app-server down".into());
        assert!(create_conflict_is_recoverable(&conflict, true));
        assert!(!create_conflict_is_recoverable(&conflict, false));
        assert!(!create_conflict_is_recoverable(&other, true));
        assert!(!create_conflict_is_recoverable(&other, false));
    }

    /// The prompt's length is bounded by reading it, which is what lets D4 drop
    /// the truncation discipline the MCP layer would have needed.
    ///
    /// `i64::MIN` rather than a plausible count: the bound has to hold for every
    /// value the type admits, and the widest rendering is the longest negative
    /// integer. (Counts cannot go negative — `COUNT(*)` — so this is the bound,
    /// not a case.)
    #[test]
    fn the_prompt_is_bounded_far_below_the_planner_input_ceiling() {
        let widest = summary_prompt(&WorkspaceActivityWindow {
            track_lifecycle_changed: i64::MIN,
            track_report_edited: i64::MIN,
            task_completed: i64::MIN,
            task_failed: i64::MIN,
            tracks_touched: i64::MIN,
        });
        assert!(
            widest.chars().count() < crate::routes::cards::MAX_PLANNER_INPUT_CHARS,
            "the prompt must fit `planner/input` for every possible count; it is \
             {} chars",
            widest.chars().count()
        );
        // The bootstrap travels the same channel and is validated by
        // `validate_first_message` under the identical ceiling.
        assert!(
            TODAY_SUMMARY_BOOTSTRAP_TEXT.chars().count()
                < crate::routes::cards::MAX_PLANNER_INPUT_CHARS
        );
        assert!(!TODAY_SUMMARY_BOOTSTRAP_TEXT.trim().is_empty());
    }
}
