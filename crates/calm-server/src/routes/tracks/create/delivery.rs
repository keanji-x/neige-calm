//! The first-message delivery both keyed-create arms submit: one `planner-harness-start`
//! operation carrying the message, and the reading of its outcome per arm.

use crate::error::{CalmError, Result};
use crate::ids::CardId;
use crate::model::Track;
use crate::operation::planner_harness_start_adapter::PlannerHarnessStartOperationPayload;
use crate::operation::{OperationKey, OperationOutcome};
use crate::routes::conversations_shared::PLANNER_HARNESS_START;
use crate::routes::idempotency_key::{calm_error_from_operation_failure, stable_payload_hash};
use crate::state::RouteState;

use super::KeyedActor;

/// Which arm submitted, for the sole purpose of reading an `OperationOutcome`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum SubmitArm {
    Mint,
    Resume,
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
    let op_payload = serde_json::to_value(&request)?;
    // Same hash shape as `start_planner_harness`, so the two paths cannot drift on what a
    // payload is.
    let payload_hash = stable_payload_hash(&serde_json::json!({
        "actor": actor.start_label,
        "request": &request,
    }))?;
    let op_id = s
        .operation_runtime
        .submit(
            PLANNER_HARNESS_START,
            OperationKey {
                operation_key: operation_key.clone(),
                // Set, unlike the legacy path's `None`: this is the column
                // `find_by_kind_and_idempotency` reads to recognise a replay.
                idempotency_key: Some(operation_key),
                payload_hash,
            },
            op_payload,
        )
        .await?;
    let result = s.operation_runtime.wait(&op_id).await?;
    response_for(arm, result.outcome)
}
