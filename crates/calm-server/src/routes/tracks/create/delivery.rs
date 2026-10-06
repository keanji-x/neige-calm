//! The first-message delivery both keyed-create arms submit: one `planner-harness-start`
//! operation carrying the message, and the reading of its outcome per arm.

use crate::error::{CalmError, Result};
use crate::ids::CardId;
use crate::mail::TOOL_MAIL_SEND;
use crate::model::Track;
use crate::operation::planner_harness_start_adapter::PlannerHarnessStartOperationPayload;
use crate::operation::planner_start_fence::CardStartFence;
use crate::operation::{OperationKey, OperationOutcome};
use crate::routes::idempotency_key::{calm_error_from_operation_failure, stable_payload_hash};
use crate::state::RouteState;

use super::{KeyedActor, ResumedStart, SendPath, resumed_start};

/// Which arm submitted: it decides whether the card's conversation is read first and how an
/// `OperationOutcome` reads.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum SubmitArm {
    Mint,
    /// Joins the chosen operation under its key; submits no start of its own.
    Replay,
    /// Submits a new start onto a card an earlier attempt already tried to start.
    GenuineRetry,
}

/// Map an operation outcome onto this route's answer, per arm. `SucceededViaCollision`
/// is globally unreachable today (nothing writes the `idempotency_collision`
/// completion); the split is a fail-closed statement, not a signal the route depends on.
pub(super) fn response_for(arm: SubmitArm, outcome: OperationOutcome) -> Result<()> {
    match outcome {
        OperationOutcome::Succeeded { .. } => Ok(()),
        // A fresh key cannot collide with itself; a 201 would promise a delivery this request
        // did not make.
        OperationOutcome::SucceededViaCollision { .. } if arm == SubmitArm::Mint => {
            Err(CalmError::Internal(
                "track create: a freshly minted Idempotency-Key resolved to an earlier \
                 planner-harness-start operation, so this request delivered nothing"
                    .to_string(),
            ))
        }
        // On the resuming arms it is the expected reading: an earlier request
        // under this key already delivered the message, and this one joins it.
        OperationOutcome::SucceededViaCollision { .. } => Ok(()),
        OperationOutcome::Failed {
            last_error,
            from_phase,
            last_error_class,
        } => Err(calm_error_from_operation_failure(
            last_error_class.as_deref(),
            harness_start_failure_message(&format!(
                "operation failed in {from_phase:?}: {last_error}"
            )),
            from_phase,
        )),
        OperationOutcome::Stuck { reason, from_phase } => Err(CalmError::Internal(
            harness_start_failure_message(&format!("operation stuck in {from_phase:?}: {reason}")),
        )),
    }
}

/// What a create that promised a delivery says when the harness start did not complete.
/// The endpoint cannot say whether the message was delivered; it only promises that a
/// retry under the same key creates no second track and delivers no second copy.
fn harness_start_failure_message(reason: &str) -> String {
    format!(
        "track create: the track was created but its planner harness start did not complete, so \
         the server cannot tell whether the first message reached the agent ({reason}). Nothing \
         is rolled back — the track, its cards and its workspace are already committed, and this \
         response does not assert that the track is usable. Retrying this create under the SAME \
         Idempotency-Key is safe in the two senses the server can prove: it creates no second \
         track, and it delivers no second copy of this message. Open the track and look before \
         doing anything else."
    )
}

/// A genuine retry's refusal: what it observed, and where the message goes instead. The same
/// sentence on every surface; only the send path differs.
fn planner_has_a_session(
    send_path: SendPath,
    request: &PlannerHarnessStartOperationPayload,
) -> CalmError {
    let send = match send_path {
        SendPath::PlannerInput => format!(
            "through POST /api/cards/{}/planner/input",
            request.planner_card_id
        ),
        SendPath::Mail => format!("with {TOOL_MAIL_SEND} to track {}", request.track_id),
    };
    CalmError::Conflict(format!(
        "track create: this Track's Planner already has a session, so this retry started \
         nothing and did not deliver the first message; send it to that Planner {send}"
    ))
}

/// Submit `planner-harness-start` carrying the first message. Deliberately NOT the
/// best-effort shape `start_planner_harness` uses: a 201 for an operation that never
/// enqueued the sentence would lie, and a 5xx is what makes the genuine-retry arm usable.
#[allow(clippy::too_many_arguments)]
pub(super) async fn start_planner_harness_with_first_message(
    s: &RouteState,
    actor: &KeyedActor,
    arm: SubmitArm,
    track: &Track,
    planner_card_id: String,
    report_card_id: String,
    // `cwd` is NOT `track.workspace.path`: on a replay it is the chosen operation's `cwd`,
    // so the resubmitted payload hashes to the same value after a repoint.
    cwd: String,
    text: String,
    create_request_sha256: String,
    operation_key: String,
) -> Result<()> {
    let request = PlannerHarnessStartOperationPayload {
        actor: actor.start.clone(),
        track_id: track.id.to_string(),
        planner_card_id: CardId::from(planner_card_id),
        report_card_id: Some(report_card_id),
        sort: None,
        cwd,
        // The user's sentence is a `UserMessage`; `goal` stays reserved for the
        // machine-written child-track bootstrap.
        goal: None,
        reset_harness_items: false,
        force_new_thread: false,
        profile: Default::default(),
        create_card: None,
        // Enqueued by `prepare_tx` inside the mint transaction; being part of the payload it
        // also binds the body into `payload_hash`, so a different sentence under one key is a 409.
        first_message: Some(text),
        // Also carried in the operation payload for its local collision check; the durable
        // authority is the binding row. Other producers leave the field `None`.
        create_request_sha256: Some(create_request_sha256),
        // Not a conversation create; nothing to brief.
        opening_briefing: None,
    };
    // Same hash shape as `start_planner_harness`, so the two paths cannot drift on what a
    // payload is.
    let payload_hash = stable_payload_hash(&serde_json::json!({
        "actor": actor.start_label,
        "request": &request,
    }))?;
    // Taken inside the same-key claim the plan holds (`state.rs` lock order); the card exists
    // already, so a send or a reset may be using it.
    let fence = CardStartFence::lock(
        &s.planner_recovery_locks,
        &s.repo,
        &s.operation_runtime,
        &request.planner_card_id,
    )
    .await;
    // A genuine retry follows a failed start, and a reset or re-point may have given the card a
    // conversation since (#2212). That is the send's to continue, not this start's to supersede.
    if arm == SubmitArm::GenuineRetry
        && resumed_start(fence.conversation().await?)? == ResumedStart::Conversation
    {
        return Err(planner_has_a_session(actor.send_path, &request));
    }
    let result = fence
        .start(
            &request,
            OperationKey {
                operation_key: operation_key.clone(),
                // Set, unlike the legacy path's `None`: this is the column
                // `find_by_kind_and_idempotency` reads to recognise a replay.
                idempotency_key: Some(operation_key),
                payload_hash,
            },
        )
        .await?;
    response_for(arm, result.outcome)
}
