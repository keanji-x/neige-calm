//! `/api/cards/{id}/planner/*` — the headless planner-harness card surface: interrupt, run
//! snapshot, reset and the harness start they share, plus the input-length rule and the
//! shared-planner turn teardown used by card, track and area deletion.

use crate::actor::Actor;
use crate::db::RouteRepo;
use crate::error::{CalmError, ErrorBody, Result};
use crate::extract::{Json, Path};
use crate::harness::{HarnessPhaseTag, QueueEntry, RunningTurn, TokenUsage};
use crate::ids::CardId;
use crate::model::{Card, CardRole, Track, new_id};
use crate::operation::planner_harness_interrupt_adapter::PlannerHarnessInterruptOperationPayload;
use crate::operation::planner_harness_shutdown_adapter::PlannerHarnessShutdownOperationPayload;
use crate::operation::planner_harness_start_adapter::{
    HarnessProfile, PlannerHarnessStartOperationPayload,
};
use crate::operation::{OperationKey, OperationOutcome};
use crate::routes::idempotency_key::{calm_error_from_operation_failure, stable_payload_hash};
use crate::routes::planner_start_fence::CardStartFence;
use crate::session_projection_lookup::card_is_shared_planner;
use crate::session_projection_repo::WorkerSessionProjection;
use crate::state::{CodexShellState, RouteState};

use axum::extract::State;
use calm_types::planner_attachment::PlannerAttachment;
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

/// Whether the persisted card shape is allowed to use the headless harness routes. Unknown/malformed profile values fail closed.
pub(crate) fn card_runs_headless_harness(card: &Card, role: CardRole) -> bool {
    crate::harness::profile::PlannerBinding::from_card(card, role).is_some()
}

pub(crate) async fn interrupt_shared_card_active_turn(
    repo: &dyn RouteRepo,
    cs: &CodexShellState,
    card: &Card,
) {
    let active_runtime = match repo
        .session_projection_active_for_card(&card.id.to_string())
        .await
    {
        Ok(runtime) => runtime,
        Err(e) => {
            tracing::warn!(
                target: "session_projection_lookup::fallback",
                card_id = %card.id,
                error = %e,
                "runtime shared-card discriminator query failed; falling back to card payload"
            );
            None
        }
    };
    if !card_is_shared_planner(card, active_runtime.as_ref()) {
        return;
    }
    if let Err(e) = cs
        .shared_codex_appserver
        .interrupt_active_turn_for_card(card.id.as_str())
        .await
    {
        tracing::warn!(
            target: "shared_codex_daemon::orphan_turn",
            card_id = %card.id,
            track_id = %card.track_id,
            error = %e,
            "failed to interrupt active shared codex turn during card teardown"
        );
    }
}

/// Deletion-grade form of [`interrupt_shared_card_active_turn`]: every failure is propagated, since a destructive workspace move may only follow a confirmed quiesce.
pub(crate) async fn quiesce_shared_card_active_turn(
    s: &RouteState,
    cs: &CodexShellState,
    card: &Card,
) -> Result<Option<String>> {
    let active_runtime = s
        .repo
        .session_projection_active_for_card(&card.id.to_string())
        .await?;
    if card_is_shared_planner(card, active_runtime.as_ref()) {
        let thread_id = active_runtime
            .as_ref()
            .and_then(crate::harness::effective_runtime_thread_id);
        let mut seals = crate::thread_seals::DeletionThreadSeals::new(s.thread_seals.clone());
        if let Some(thread_id) = thread_id.clone() {
            seals.seal(thread_id);
        }
        if let Some(thread_id) = thread_id.as_deref() {
            cs.shared_codex_appserver
                .interrupt_active_turn(thread_id)
                .await?;
        }
        return Ok(seals.retain().pop());
    }
    Ok(None)
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ResetPlannerCardResponse {
    #[schema(value_type = String)]
    pub card_id: CardId,
    pub terminal_id: String,
    pub new_thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track: Option<Track>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct InterruptPlannerCardResponse {
    #[schema(value_type = String)]
    pub card_id: CardId,
    pub worker_session_id: String,
    /// True when a turn was running and an interrupt was dispatched at it; false when idle or a `turn/start` was still in flight. Means the interrupt was *issued* — completion is asynchronous.
    pub stopped: bool,
}

/// Current planner-harness run snapshot for a card, so a page opened mid-turn can seed its phase. Dormancy is NOT an error: it is the `{worker_session_id: null, phase: null}` answer.
#[derive(Debug, Serialize, ToSchema)]
pub struct GetPlannerRunResponse {
    #[schema(value_type = String)]
    pub card_id: CardId,
    /// Active worker-session id, or null when the harness is dormant.
    pub worker_session_id: Option<String>,
    /// Current harness phase, or null when the harness is dormant.
    pub phase: Option<HarnessPhaseTag>,
    /// Latest context-window usage, or null when the harness is dormant or codex has not pushed a `thread/tokenUsage/updated` frame yet. A dormant conversation reports `null` even though the reading is on disk.
    pub token_usage: Option<PlannerRunTokenUsage>,
    /// The model slug this conversation's turns run with, or `null` to follow the installation default. Read off the CARD, so answered for a dormant conversation too.
    pub model: Option<String>,
    /// The chosen reasoning effort, or `null` for the default. Same source as `model`.
    pub reasoning_effort: Option<String>,
    /// Why this conversation's queue is not draining, or `null` when there is nothing worth saying: an undeterminable model/effort, a refused turn start, or a long codex outage. A client should render it as a standing notice, not a request error.
    /// A brief outage fills nothing, so `null` is not evidence that anything succeeded.
    pub blocked_reason: Option<String>,
    /// The addressable user entries still waiting for the next turn, in queue order. Empty when dormant. Dispatcher observations and pre-id user entries never appear; the latter are counted in `pending_overflow`.
    pub pending: Vec<PendingQueueEntry>,
    /// User-authored entries that exist in the queue but are NOT in `pending`: pre-id entries, plus anything past the page budget.
    pub pending_overflow: u32,
    /// Whether this card can take image attachments at all: not when the track's workspace is an attached directory, since neige never writes into one. Answered by the same function the upload runs (`planner_attachments::attachment_root`).
    pub attachments_supported: bool,
    /// The turn the harness is running. Non-null exactly when this response's `phase` is `turn_running`: both come from one read of the harness state.
    pub running_turn: Option<PlannerRunningTurn>,
}

/// A running turn and how long it has run, by the harness's monotonic clock since it accepted that turn's `TurnStarted`. A duplicate or stale start never resets it. Not a wall-clock time and not persisted: a client anchors it to when the response arrived.
#[derive(Debug, Serialize, ToSchema)]
pub struct PlannerRunningTurn {
    pub turn_id: String,
    pub elapsed_ms: u64,
}

impl From<RunningTurn> for PlannerRunningTurn {
    fn from(running: RunningTurn) -> Self {
        Self {
            turn_id: running.turn_id,
            elapsed_ms: u64::try_from(running.elapsed.as_millis()).unwrap_or(u64::MAX),
        }
    }
}

/// One addressable user entry from the harness pending queue.
#[derive(Debug, Serialize, ToSchema)]
pub struct PendingQueueEntry {
    /// Stable identity. Never empty, and unique within one response: the snapshot decoder refuses an empty or duplicated id, demoting the slot to `LegacyUser`.
    pub entry_id: String,
    /// The complete text. Never truncated — an entry that would not fit the page budget is left out entirely.
    pub text: String,
    /// CAS token for the edit/delete endpoints. Bumped whenever the text is rewritten, folding included.
    pub rev: u32,
    /// Wall-clock ms at which the entry entered the queue.
    pub queued_at_ms: i64,
    /// The images this queued message carries. Each is already bound; the client addresses an attachment by id, never by host path.
    pub attachments: Vec<PlannerAttachment>,
}

/// Hard cap on entries in one `pending` page.
const PENDING_PAGE_MAX: usize = 64;

/// Soft cap on the UTF-8 size of one `pending` page; entries are packed whole until the next one would cross it. A judgement about acceptable response size, not a measurement.
const PENDING_PAGE_BYTES: usize = 1_536 * 1_024;

/// Split the queue into one page of addressable entries plus a count of the user-authored entries that did not make it.
/// The budget ALWAYS admits at least one entry: a head entry over budget would make the whole queue unpageable, so the user could not delete the thing blocking it.
fn page_pending_entries(card_id: &CardId, entries: &[QueueEntry]) -> (Vec<PendingQueueEntry>, u32) {
    let mut page = Vec::new();
    let mut used_bytes = 0usize;
    let mut overflow = 0u32;
    let mut budget_exhausted = false;
    for entry in entries {
        let Some(view) = entry.user_view() else {
            // Not addressable. A dispatcher observation is not the user's; a pre-id user entry is counted.
            if entry.is_user_authored() {
                overflow = overflow.saturating_add(1);
            }
            continue;
        };
        // The page is a PREFIX of the queue, not a greedy pack: packing around a hole would show a list whose order and adjacency lie.
        budget_exhausted = budget_exhausted
            || page.len() >= PENDING_PAGE_MAX
            || (!page.is_empty()
                && used_bytes.saturating_add(view.text.len()) > PENDING_PAGE_BYTES);
        if budget_exhausted {
            overflow = overflow.saturating_add(1);
            continue;
        }
        used_bytes = used_bytes.saturating_add(view.text.len());
        page.push(PendingQueueEntry {
            entry_id: view.id.as_str().to_string(),
            text: view.text.to_string(),
            rev: view.rev,
            queued_at_ms: view.queued_at_ms,
            attachments: view
                .attachments
                .iter()
                .map(|attachment| attachment.wire(card_id))
                .collect(),
        });
    }
    (page, overflow)
}

/// The context-usage half of [`GetPlannerRunResponse`]. `percent` is computed on the server so there is one place to get it wrong, and `total_tokens` is NOT shipped: it is a cumulative sum across the thread, and a meter drawn from it is the most likely UI bug.
#[derive(Debug, Serialize, ToSchema)]
pub struct PlannerRunTokenUsage {
    /// Tokens in the model's context as of the most recent response. Always present, even when `percent` is not.
    pub used_tokens: i64,
    /// The model's context window, or null when codex has never reported one.
    pub context_window: Option<i64>,
    /// Context occupancy as a whole percentage, `0.0..=100.0`. Null when no percentage can honestly be stated (no window, window at or below baseline, or usage above the window — deliberately NOT clamped).
    pub percent: Option<f64>,
    /// Wall-clock ms of the codex frame this reading came from. The reading survives a reboot via the runtime snapshot, so without this a rehydrated reading is indistinguishable from a live one.
    pub at_ms: i64,
}

impl From<&TokenUsage> for PlannerRunTokenUsage {
    fn from(usage: &TokenUsage) -> Self {
        Self {
            used_tokens: usage.used_tokens,
            context_window: usage.context_window,
            percent: usage.percent(),
            at_ms: usage.at_ms,
        }
    }
}

pub(crate) const MAX_PLANNER_INPUT_CHARS: usize = 32_768;

/// The one body check for planner input, shared by the send route and the edit route: an edit is a send by another name.
pub(crate) fn validate_planner_input_text(text: &str) -> Result<usize> {
    validate_planner_input(text, false)
}

/// The same check, told whether the message carries an image: an empty text beside an attachment is a message, not a refusal. The length limit still applies to whatever text there is.
/// The edit route keeps requiring text: `PATCH` cannot change attachments, so it cannot tell 'this message is its picture' from 'this message is now empty'.
pub(crate) fn validate_planner_input(text: &str, has_attachments: bool) -> Result<usize> {
    if text.trim().is_empty() && !has_attachments {
        return Err(CalmError::BadRequest("text must not be empty".into()));
    }
    let char_count = text.chars().count();
    if char_count > MAX_PLANNER_INPUT_CHARS {
        return Err(CalmError::BadRequest(format!(
            "text must be at most {MAX_PLANNER_INPUT_CHARS} characters",
        )));
    }
    Ok(char_count)
}

/// Stop the running planner turn. Guard chain mirrors `/planner/input` but WITHOUT lazy recovery: a harness that needs recovering has no running turn to stop, so a registry miss is the same 409 `planner_harness_dormant`.
/// Idle is a graceful no-op (`stopped: false`). The phase read and the dispatch are not atomic; the user presses Stop again.
/// `IssuingTurn` also reports `stopped: false`: while `turn/start` is in flight the app-server may not know the turn yet, so the interrupt is dispatched best-effort but only `TurnRunning` guarantees a target.
#[utoipa::path(
    post,
    path = "/api/cards/{id}/planner/interrupt",
    tag = "cards",
    params(("id" = String, Path, description = "Planner card id")),
    responses(
        (status = 200, description = "Interrupt dispatched at the running turn (`stopped: true`); `stopped: false` when no turn was running (graceful no-op) or a turn was still being issued (best-effort dispatch only — press Stop again once the turn is running)", body = InterruptPlannerCardResponse),
        (status = 403, description = "Card is not a planner codex card", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 409, description = "No live planner harness session for this card — reset to start a session (code `planner_harness_dormant`)", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn interrupt_planner_card(
    State(s): State<RouteState>,
    actor: Actor,
    Path(id): Path<String>,
) -> Result<Json<InterruptPlannerCardResponse>> {
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

    let dormant = || {
        CalmError::PlannerHarnessDormant(format!(
            "no live planner harness session for card {id}; reset to start a session",
        ))
    };
    let runtime = s
        .repo
        .session_projection_active_for_card(&card.id.to_string())
        .await?
        .ok_or_else(dormant)?;
    let harness = s.harness.get(&runtime.id).ok_or_else(dormant)?;

    let phase = harness.snapshot().await.phase;
    // Dispatch for IssuingTurn too (best-effort), but only TurnRunning reports `stopped: true`.
    let dispatch = matches!(
        phase,
        HarnessPhaseTag::TurnRunning | HarnessPhaseTag::IssuingTurn
    );
    let stopped = matches!(phase, HarnessPhaseTag::TurnRunning);
    if dispatch {
        let payload = serde_json::to_value(PlannerHarnessInterruptOperationPayload {
            worker_session_id: runtime.id.clone(),
            reason: "user_stop".into(),
        })?;
        run_planner_card_operation(&s, "planner-harness-interrupt", payload).await?;
    }

    tracing::info!(
        actor = %actor.as_str(),
        card_id = %card.id,
        runtime_id = %runtime.id,
        ?phase,
        stopped,
        "planner harness user stop requested"
    );

    Ok(Json(InterruptPlannerCardResponse {
        card_id: card.id,
        worker_session_id: runtime.id.clone(),
        stopped,
    }))
}

/// Read the current planner-harness phase for a card. Unlike the write routes, a dormant harness is a normal `200 {worker_session_id: null, phase: null}`, not a 409.
#[utoipa::path(
    get,
    path = "/api/cards/{id}/planner/run",
    tag = "cards",
    params(("id" = String, Path, description = "Planner card id")),
    responses(
        (status = 200, description = "Current run snapshot; `worker_session_id`/`phase` are null when no live harness session exists (dormant is not an error for a read)", body = GetPlannerRunResponse),
        (status = 403, description = "Card is not a planner codex card", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn get_planner_run(
    State(s): State<RouteState>,
    Path(id): Path<String>,
) -> Result<Json<GetPlannerRunResponse>> {
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

    // Unreadable model keys are reported as 'no selection' by this READ rather than as a 500; the turn-issuing path refuses on the same payload, so the conversation still stops but this surface can show why.
    let selection =
        crate::planner_model::CardModelSelection::from_payload(&card.payload).unwrap_or_default();
    // The same predicate the upload endpoint enforces, so the answer cannot drift from the refusal.
    let attachments_supported = match s.repo.track_get(card.track_id.as_str()).await? {
        Some(track) => {
            crate::planner_attachments::attachment_root(&track.workspace, &s.workspace_root).is_ok()
        }
        // No track means no workspace to write into; this field is not the place to raise it, and 'supported' would be the wrong guess.
        None => false,
    };
    let mut dormant = GetPlannerRunResponse {
        card_id: card.id.clone(),
        worker_session_id: None,
        phase: None,
        model: selection.model.clone(),
        reasoning_effort: selection.reasoning_effort.clone(),
        // A dormant conversation has no harness to be blocked and nothing waiting.
        blocked_reason: None,
        token_usage: None,
        pending: Vec::new(),
        pending_overflow: 0,
        attachments_supported,
        running_turn: None,
    };
    let Some(runtime) = s
        .repo
        .session_projection_active_for_card(&card.id.to_string())
        .await?
    else {
        if let Some(runtime) = s
            .repo
            .session_projection_projectable_for_card(&card.id.to_string())
            .await?
        {
            if let Some(snapshot) = super::planner_recovery::unconfirmed_stop_snapshot(&runtime) {
                dormant.worker_session_id = Some(runtime.id.clone());
                dormant.phase = Some(snapshot.phase);
                dormant.blocked_reason =
                    Some(calm_types::harness::HARNESS_INTERRUPT_TIMEOUT_MESSAGE.into());
                dormant.token_usage = snapshot
                    .token_usage
                    .as_ref()
                    .map(PlannerRunTokenUsage::from);
                (dormant.pending, dormant.pending_overflow) =
                    page_pending_entries(&card.id, &snapshot.pending_entries());
            } else if let Some(snapshot) =
                super::planner_recovery::recoverable_snapshot(&s, &runtime).await?
            {
                dormant.blocked_reason = Some(super::planner_recovery::RECOVERY_NOTICE.into());
                (dormant.pending, dormant.pending_overflow) =
                    page_pending_entries(&card.id, &snapshot.pending_entries());
            }
        }
        return Ok(Json(dormant));
    };
    let Some(harness) = s.harness.get(&runtime.id) else {
        return Ok(Json(dormant));
    };
    // Phase and the running turn come from one state read, so a running turn is never paired with another phase.
    let (snapshot, running_turn) = harness.snapshot_with_running_turn().await;
    let (pending, pending_overflow) = page_pending_entries(&card.id, &snapshot.pending_entries());
    Ok(Json(GetPlannerRunResponse {
        attachments_supported,
        card_id: card.id,
        worker_session_id: Some(runtime.id.clone()),
        phase: Some(snapshot.phase),
        model: selection.model,
        reasoning_effort: selection.reasoning_effort,
        blocked_reason: harness.issuance_block().await,
        token_usage: snapshot
            .token_usage
            .as_ref()
            .map(PlannerRunTokenUsage::from),
        pending,
        pending_overflow,
        running_turn: running_turn.map(PlannerRunningTurn::from),
    }))
}

#[utoipa::path(
    post,
    path = "/api/cards/{id}/planner/reset",
    tag = "cards",
    params(("id" = String, Path, description = "Planner card id")),
    responses(
        (status = 200, description = "Planner session reset", body = ResetPlannerCardResponse),
        (status = 403, description = "Card is not a planner codex card", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 409, description = "The card's provider cannot start now: Claude is not configured or its version cannot be confirmed (`conflict`)", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn reset_planner_card(
    State(s): State<RouteState>,
    actor: Actor,
    Path(id): Path<String>,
) -> Result<Json<ResetPlannerCardResponse>> {
    let card = s
        .repo
        .card_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    let role = s
        .write
        .verify_role(&card.id)
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    let binding = crate::harness::profile::PlannerBinding::from_card(&card, role)
        .ok_or_else(|| CalmError::Forbidden(format!("card {id} is not a planner codex card")))?;
    let response = reset_planner_card_shared(s, actor, card, binding.profile).await?;
    Ok(Json(response))
}

async fn reset_planner_card_shared(
    s: RouteState,
    actor: Actor,
    card: Card,
    profile: HarnessProfile,
) -> Result<ResetPlannerCardResponse> {
    // Reset takes the SAME per-card fence as `/planner/input` lazy recovery, or a reset racing a registry-miss Send could resurrect the reset-away session. Deadlock-free: neither adapter re-enters `planner_recovery_locks`.
    let fence = CardStartFence::lock(&s, &card.id).await;
    let active_runtime = s
        .repo
        .session_projection_active_for_card(&card.id.to_string())
        .await?;
    reset_planner_harness_card(s, &fence, actor, card, profile, active_runtime).await
}

async fn reset_planner_harness_card(
    s: RouteState,
    fence: &CardStartFence,
    actor: Actor,
    card: Card,
    profile: HarnessProfile,
    runtime: Option<WorkerSessionProjection>,
) -> Result<ResetPlannerCardResponse> {
    start_harness_card(&s, fence, &actor, &card, profile, HarnessCardStart::Reset).await?;

    if let Some(runtime) = runtime {
        let shutdown_payload = serde_json::to_value(PlannerHarnessShutdownOperationPayload {
            worker_session_id: runtime.id.clone(),
        })?;
        run_planner_card_operation(&s, "planner-harness-shutdown", shutdown_payload).await?;
    }

    let active = s
        .repo
        .session_projection_active_for_card(&card.id.to_string())
        .await?
        .ok_or_else(|| CalmError::Internal(format!("runtime for card {} missing", card.id)))?;
    let new_thread_id = active.thread_id.clone().ok_or_else(|| {
        CalmError::Internal(format!(
            "planner harness reset succeeded without a thread_id for card {}",
            card.id
        ))
    })?;
    let track = s
        .repo
        .track_get(card.track_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {}", card.track_id)))?;

    Ok(ResetPlannerCardResponse {
        card_id: card.id,
        terminal_id: String::new(),
        new_thread_id,
        track: Some(track),
    })
}

/// How [`start_harness_card`] starts an existing harness card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HarnessCardStart {
    /// `/planner/reset`: a new thread, and the transcript cleared.
    Reset,
    /// A send to a card with no thread to preserve (#2184): there is nothing to clear.
    Fresh,
}

/// Run one `planner-harness-start` for an existing harness card and wait for it. The one
/// derivation of that start's payload, shared by reset and a send's fresh start, submitted under
/// the card's [`CardStartFence`]. `profile` is the card's OWN, from its
/// [`PlannerBinding`](crate::harness::profile::PlannerBinding): starting an assistant under
/// `Planner` would mint its thread with the planner prompt while the card row still says `assistant`.
pub(crate) async fn start_harness_card(
    s: &RouteState,
    fence: &CardStartFence,
    actor: &Actor,
    card: &Card,
    profile: HarnessProfile,
    start: HarnessCardStart,
) -> Result<()> {
    let track = s
        .repo
        .track_get(card.track_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {}", card.track_id)))?;
    // The start adapter refuses this too; answering here keeps the HTTP 403 contract rather than a generic operation failure.
    if profile == HarnessProfile::Planner
        && track.purpose.as_deref() == Some(crate::AREA_CHAT_PURPOSE)
    {
        return Err(CalmError::Forbidden(format!(
            "planner harness is disabled for area chat track {}",
            track.id
        )));
    }
    // No profile inherits the track title as a goal on these user-driven paths.
    let reset = start == HarnessCardStart::Reset;
    let start_request = PlannerHarnessStartOperationPayload {
        actor: actor.to_actor_id(),
        track_id: track.id.to_string(),
        planner_card_id: card.id.clone(),
        report_card_id: None,
        sort: None,
        cwd: track.workspace.agent_cwd().to_string(),
        goal: None,
        reset_harness_items: reset,
        force_new_thread: reset,
        profile,
        create_card: None,
        first_message: None,
        create_request_sha256: None,
        // Not a conversation create; nothing to brief. `None` is skipped by serde.
        opening_briefing: None,
    };
    let payload_hash = stable_payload_hash(&serde_json::to_value(&start_request)?)?;
    let result = fence
        .start(
            &start_request,
            OperationKey {
                operation_key: new_id(),
                idempotency_key: None,
                payload_hash,
            },
        )
        .await?;
    planner_card_operation_outcome(result.outcome)
}

/// Submit one planner-card operation and wait for it, mapping its outcome onto a `CalmError`, so every planner-card route maps failure classes identically.
pub(crate) async fn run_planner_card_operation(
    s: &RouteState,
    kind: &str,
    payload: Value,
) -> Result<()> {
    let payload_hash = stable_payload_hash(&payload)?;
    let op_id = s
        .operation_runtime
        .submit(
            kind,
            OperationKey {
                operation_key: new_id(),
                idempotency_key: None,
                payload_hash,
            },
            payload,
        )
        .await?;
    let result = s.operation_runtime.wait(&op_id).await?;
    planner_card_operation_outcome(result.outcome)
}

/// The one mapping of a planner-card operation's outcome onto a `CalmError`.
fn planner_card_operation_outcome(outcome: OperationOutcome) -> Result<()> {
    match outcome {
        OperationOutcome::Succeeded { .. } | OperationOutcome::SucceededViaCollision { .. } => {
            Ok(())
        }
        OperationOutcome::Failed {
            last_error,
            from_phase,
            last_error_class,
        } => Err(calm_error_from_operation_failure(
            last_error_class.as_deref(),
            last_error,
            from_phase,
        )),
        OperationOutcome::Stuck { .. } => {
            Err(CalmError::Internal("operation stuck, see DB".to_string()))
        }
    }
}

#[cfg(test)]
mod pending_page_tests {
    use super::{PENDING_PAGE_BYTES, PENDING_PAGE_MAX, page_pending_entries};
    use crate::harness::{HARNESS_MODE, HarnessSnapshot, Observation, QueueEntry};
    use crate::ids::CardId;
    use serde_json::json;

    fn test_card_id() -> CardId {
        CardId::from("card-paging")
    }

    /// A legacy entry built the ONLY way production can produce one: a row whose `pending_entry_meta` slot is absent.
    fn legacy(text: &str) -> QueueEntry {
        let row = json!({
            "schema_version": 1,
            "mode": HARNESS_MODE,
            "phase": "idle",
            "pending_queue": [{"type": "user_message", "text": text}],
        });
        HarnessSnapshot::from_value_strict(row)
            .pending_entries()
            .remove(0)
    }

    fn user(text: &str) -> QueueEntry {
        QueueEntry::user_message(text.to_string(), None, Vec::new())
    }

    fn system() -> QueueEntry {
        QueueEntry::system(
            Observation::TrackGoal {
                text: "goal".into(),
            },
            None,
        )
        .expect("a track goal is a system entry")
    }

    #[test]
    fn only_addressable_user_entries_reach_the_page() {
        let entries = vec![system(), user("mine"), legacy("older")];
        let (page, overflow) = page_pending_entries(&test_card_id(), &entries);
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].text, "mine");
        assert_eq!(
            overflow, 1,
            "the legacy entry is counted, the system entry is not"
        );
    }

    #[test]
    fn the_page_is_capped_by_entry_count() {
        let entries = (0..PENDING_PAGE_MAX + 5)
            .map(|i| user(&format!("m{i}")))
            .collect::<Vec<_>>();
        let (page, overflow) = page_pending_entries(&test_card_id(), &entries);
        assert_eq!(page.len(), PENDING_PAGE_MAX);
        assert_eq!(overflow, 5);
        assert_eq!(page[0].text, "m0", "the page starts at the queue head");
    }

    #[test]
    fn the_page_is_capped_by_byte_budget_and_entries_stay_whole() {
        let big = "x".repeat(PENDING_PAGE_BYTES / 2 + 1);
        let entries = vec![user(&big), user(&big), user("tiny")];
        let (page, overflow) = page_pending_entries(&test_card_id(), &entries);
        assert_eq!(page.len(), 1, "the second entry would cross the budget");
        assert_eq!(
            page[0].text.len(),
            big.len(),
            "an entry that IS returned is returned whole; nothing is truncated"
        );
        assert_eq!(
            overflow, 2,
            "`tiny` would have fitted, but the page is a prefix: packing around \
             the entry that did not fit would put a hole in the middle of the \
             queue the user is looking at"
        );
    }

    /// The budget always admits the head entry, however large; unreachable today, but an over-budget head entry would make the whole queue unaddressable.
    #[test]
    fn an_over_budget_head_entry_is_still_returned_whole() {
        let huge = "y".repeat(PENDING_PAGE_BYTES + 4_096);
        let entries = vec![user(&huge), user("behind it")];
        let (page, overflow) = page_pending_entries(&test_card_id(), &entries);
        assert_eq!(page.len(), 1, "the budget never returns an empty page");
        assert_eq!(page[0].text.len(), huge.len());
        assert_eq!(overflow, 1);
    }

    #[test]
    fn an_empty_queue_pages_to_nothing() {
        let (page, overflow) = page_pending_entries(&test_card_id(), &[]);
        assert!(page.is_empty());
        assert_eq!(overflow, 0);
    }
}
