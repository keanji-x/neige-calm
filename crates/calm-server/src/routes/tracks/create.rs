//! `POST /api/tracks`, the keyed half: safe retry under an `Idempotency-Key`.
//! Under one key, at most one track; a create that sends no key keeps its old properties.

use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::{Json, http::StatusCode};

use crate::actor::Actor;
use crate::db::sqlite::TrackCreateRequestFingerprint;
use crate::error::{CalmError, Result};
use crate::ids::CardId;
use crate::model::{NewTrack, RequestTheme, Track};
use crate::operation::planner_harness_start_adapter::PlannerHarnessStartOperationPayload;
use crate::operation::{OperationKey, OperationOutcome};
use crate::per_card_lock::lock_card;
use crate::routes::conversations_shared::{
    PLANNER_HARNESS_START, first_message_digest, retryable_operation_key, validate_first_message,
};
use crate::routes::terminal_cards::{
    calm_error_from_operation_failure, parse_idempotency_key_header, stable_payload_hash,
};
use crate::state::RouteState;

use super::{CreateTrackOptions, TrackCreateIdempotencyClaim, create_track_structure};

/// Which arm this request takes: binding miss + vacant key → `Mint`; hit + occupied →
/// `Replay`; hit + vacant → `GenuineRetry`; miss + occupied → `BindingLost` (unreachable;
/// 500, fail closed — minting there would leave an orphan track behind a 409).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SelectedArm {
    Mint,
    Replay,
    GenuineRetry,
    BindingLost,
}

/// `chosen_is_occupied` is the state of the selected key, not the shape of its name.
fn select_arm(binding_hit: bool, chosen_is_occupied: bool) -> SelectedArm {
    match (binding_hit, chosen_is_occupied) {
        (false, false) => SelectedArm::Mint,
        (false, true) => SelectedArm::BindingLost,
        (true, true) => SelectedArm::Replay,
        (true, false) => SelectedArm::GenuineRetry,
    }
}

/// Whether the payload this request submits must be frozen to what the predecessor
/// submitted or re-derived from current state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PriorArm {
    /// The chosen key already holds an operation: a replay must resubmit its payload byte
    /// for byte, or `submit` answers 409 `conflict`.
    Replay,
    /// The chosen key is vacant: this request genuinely executes and must describe the
    /// world as it is now (a stale `cwd` may have been repointed or recycled).
    GenuineRetry,
}

/// What a previous attempt under this `Idempotency-Key` already minted; the ids come
/// from the binding row, not from an operation payload.
struct PriorAttempt {
    arm: PriorArm,
    track_id: String,
    planner_card_id: String,
    report_card_id: String,
    /// The chosen operation's `cwd`, replayed verbatim on `Replay`; `None` on `GenuineRetry`,
    /// which takes `track.workspace.path`. Any future payload field read from mutable
    /// server state belongs in this struct too.
    cwd: Option<String>,
}

/// What `POST /api/tracks` decided before it validated any of the create path. A type,
/// not an `Option<PriorAttempt>`: the minting arms cannot carry a prior attempt and the
/// resuming arms always do.
pub(super) enum CreatePlan {
    /// No `first_message` and no `Idempotency-Key`: no lookup, no binding row; a retry
    /// mints a second track.
    Legacy,
    /// No `first_message`, but a key with no binding to adopt; the binding row is written
    /// inside the mint transaction.
    MessageLessMint(MessageLessPlan),
    /// No `first_message`, and a key a prior create already minted under. Mints nothing.
    MessageLessResume(MessageLessResume),
    /// A `first_message` on a key with no binding to adopt; this request mints.
    Mint(FirstMessagePlan),
    /// A `first_message` on a key a prior attempt already minted under; the create path,
    /// validation included, is skipped.
    Resume(ResumeFirstMessage),
}

/// A [`CreatePlan::Resume`]'s payload: the shared plan plus the prior attempt
/// that makes it a resume.
pub(super) struct ResumeFirstMessage {
    plan: FirstMessagePlan,
    prior: PriorAttempt,
}

/// Every request field that decides the minted track, as the caller sent it (cloned
/// before `CreationSource::stamp` rewrites the template spelling).
pub(super) struct CreateRequestShape {
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub title: String,
    pub sort: Option<f64>,
    pub cwd: Option<String>,
    pub template_id: Option<String>,
    pub recipe_id: Option<String>,
    pub template_input: Option<serde_json::Value>,
    pub attach_folder: bool,
    pub allow_cross_area_cwd: Option<super::CrossAreaCwdAuthorization>,
    pub theme: RequestTheme,
    pub fork_report_from: Option<String>,
}

/// What a keyed message-less create needs: no text, no message digest, no operation key.
pub(super) struct MessageLessPlan {
    /// The caller's `Idempotency-Key`, verbatim — half of the binding row's
    /// primary key.
    idempotency_key: String,
    create_request_sha256: String,
    /// Held from before the binding lookup until after the mint settles, so two concurrent
    /// same-key creates in one process cannot both read "no binding"; the binding table's
    /// primary key is the cross-instance wall underneath it.
    _same_key_claim: crate::per_card_lock::PerCardLockGuard,
}

impl MessageLessPlan {
    /// `first_message_sha256: None` is the fact that this create carried no message; it
    /// makes the binding row fingerprint version 2.
    pub(super) fn claim(&self) -> TrackCreateIdempotencyClaim {
        TrackCreateIdempotencyClaim {
            key: self.idempotency_key.clone(),
            create_request_sha256: self.create_request_sha256.clone(),
            first_message_sha256: None,
        }
    }
}

/// A [`CreatePlan::MessageLessResume`]'s payload; carries no `NewTrack`, so this arm
/// cannot mint.
pub(super) struct MessageLessResume {
    track_id: String,
    planner_card_id: String,
    report_card_id: String,
    /// See [`MessageLessPlan::_same_key_claim`]. Held through the resume too,
    /// so a resume and a concurrent mint under one key cannot interleave.
    _same_key_claim: crate::per_card_lock::PerCardLockGuard,
}

/// Everything a keyed `POST /api/tracks` needs to submit the operation.
pub(super) struct FirstMessagePlan {
    text: String,
    /// The digest of [`CreateRequestShape`]; travels on the plan so `resume_prior_attempt`
    /// needs no mint input.
    create_request_sha256: String,
    /// The initial message digest is stored in the binding row too. Unlike the
    /// create digest, it may be relaxed after a persisted terminal failure,
    /// when the next `#N` operation is a new delivery attempt.
    first_message_sha256: String,
    /// The caller's `Idempotency-Key`, verbatim; half of the binding row's primary key on
    /// the `Mint` arm.
    idempotency_key: String,
    /// The key to submit the `planner-harness-start` operation under, already
    /// stepped past any terminally failed predecessor.
    operation_key: String,
    /// Held from before the two lookups until after the operation settles, so two concurrent
    /// creates under one key cannot both mint. In-process only; the binding primary key is
    /// the cross-process wall. Taken OUTER, never nested inside `planner_recovery_locks`.
    _same_key_claim: crate::per_card_lock::PerCardLockGuard,
}

/// `SHA-256("track-create:{area_id}:{key}")`, prefixed `track-create-`: its own
/// namespace, so it cannot collide with the area-chat flavour's `(area_id, key)` pair.
fn derive_track_create_operation_key(area_id: &str, idempotency_key: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(format!("track-create:{area_id}:{idempotency_key}"));
    format!("track-create-{}", hex::encode(hasher.finalize()))
}

/// Parse and validate the first-message half of the request, and pick the arm — before
/// `create_track` validates, let alone mints, anything.
pub(super) async fn plan_first_message(
    s: &RouteState,
    headers: &HeaderMap,
    first_message: Option<String>,
    area_id: &str,
    shape: CreateRequestShape,
) -> Result<CreatePlan> {
    // The header is parsed BEFORE the `first_message` fork because it decides the
    // message-less fork too; a malformed key on a message-less create is a 400.
    let idempotency_key = parse_idempotency_key_header(headers)?;
    let Some(text) = first_message else {
        // No key means the legacy path verbatim; a key opts this shape into the same binding
        // row the `first_message` path uses.
        let Some(idempotency_key) = idempotency_key else {
            return Ok(CreatePlan::Legacy);
        };
        return plan_message_less(s, area_id, shape, idempotency_key).await;
    };
    let idempotency_key = idempotency_key.ok_or_else(|| {
        CalmError::BadRequest(
            "Idempotency-Key header is required when `first_message` is present, so a retried create cannot mint a second track or deliver the message twice"
                .into(),
        )
    })?;
    // Run before the folder claim, the track row, the cards and `materialize_workspace`,
    // so a rejected message leaves no track behind.
    validate_first_message(&text)?;

    let create_request_sha256 = create_request_digest(&shape)?;
    let first_message_sha256 = first_message_digest(&text);
    let base_key = derive_track_create_operation_key(area_id, &idempotency_key);
    // Taken before either lookup, released when the plan is dropped at the end of the request.
    let same_key_claim = lock_card(&s.conversation_first_message_locks, &base_key).await;

    // Lookup 1 — a row that committed with the id, so it is still answered after every
    // failure that leaves no operation row behind.
    let binding = s
        .repo
        .track_create_idempotency_get(area_id, &idempotency_key)
        .await?;
    if let Some(binding) = binding.as_ref() {
        ensure_binding_create_matches(
            binding,
            &create_request_sha256,
            &idempotency_key,
            CreateShape::WithFirstMessage,
        )?;
    }

    // Lookup 2 — which harness-start attempt this request joins. May 409
    // `idempotency_key_exhausted`; deliberately before any mint.
    let operation_key = retryable_operation_key(s, &base_key).await?;
    let chosen_existing = s
        .operation_runtime
        .find_by_kind_and_idempotency(PLANNER_HARNESS_START, &operation_key)
        .await?;

    let plan = FirstMessagePlan {
        text,
        create_request_sha256,
        first_message_sha256,
        idempotency_key,
        operation_key: operation_key.clone(),
        _same_key_claim: same_key_claim,
    };

    let selected_arm = select_arm(binding.is_some(), chosen_existing.is_some());
    match selected_arm {
        SelectedArm::Mint => Ok(CreatePlan::Mint(plan)),
        SelectedArm::BindingLost => Err(CalmError::Internal(format!(
            "operation {operation_key} exists under this Idempotency-Key but no \
             track_create_idempotency row does. The binding commits inside the transaction that \
             mints the track, strictly before the operation is submitted, so this state is not \
             reachable from POST /api/tracks. Refusing rather than minting: a mint here would \
             commit a track and then collide on the operation's unique key, leaving an orphan \
             track behind a 409."
        ))),
        arm => {
            let binding = binding.expect("both resuming arms are selected by a binding hit");
            // Consume the chosen operation once so both the message criterion and the replayed
            // cwd come from the exact attempt this request is joining.
            let (prior_arm, cwd) = match arm {
                SelectedArm::Replay => {
                    let op = chosen_existing
                        .expect("the Replay arm is selected by an occupied chosen key");
                    let payload: PlannerHarnessStartOperationPayload =
                        serde_json::from_value(op.payload)?;
                    ensure_replay_message_matches(&payload, &plan)?;
                    (PriorArm::Replay, Some(payload.cwd))
                }
                SelectedArm::GenuineRetry => {
                    let allow_edited_message = operation_key != base_key;
                    ensure_binding_message_matches(&binding, &plan, allow_edited_message)?;
                    (PriorArm::GenuineRetry, None)
                }
                _ => unreachable!("mint and binding-lost arms returned above"),
            };
            let prior = PriorAttempt {
                arm: prior_arm,
                track_id: binding.track_id,
                planner_card_id: binding.planner_card_id,
                report_card_id: binding.report_card_id,
                cwd,
            };
            Ok(CreatePlan::Resume(ResumeFirstMessage { plan, prior }))
        }
    }
}

/// The message-less twin of [`plan_first_message`]'s keyed half: one lookup, a two-cell
/// table. The lock is taken on the SAME derived key, so the two shapes serialize
/// against each other on the one binding row.
async fn plan_message_less(
    s: &RouteState,
    area_id: &str,
    shape: CreateRequestShape,
    idempotency_key: String,
) -> Result<CreatePlan> {
    let create_request_sha256 = create_request_digest(&shape)?;
    let base_key = derive_track_create_operation_key(area_id, &idempotency_key);
    let same_key_claim = lock_card(&s.conversation_first_message_locks, &base_key).await;
    let binding = s
        .repo
        .track_create_idempotency_get(area_id, &idempotency_key)
        .await?;
    let Some(binding) = binding else {
        return Ok(CreatePlan::MessageLessMint(MessageLessPlan {
            idempotency_key,
            create_request_sha256,
            _same_key_claim: same_key_claim,
        }));
    };
    // The create shape is permanent once its track commits, so a mismatching request is a
    // conflict rather than something to act on.
    ensure_binding_create_matches(
        &binding,
        &create_request_sha256,
        &idempotency_key,
        CreateShape::MessageLess,
    )?;
    Ok(CreatePlan::MessageLessResume(MessageLessResume {
        track_id: binding.track_id,
        planner_card_id: binding.planner_card_id,
        report_card_id: binding.report_card_id,
        _same_key_claim: same_key_claim,
    }))
}

/// The create-shape digest, in one place so both plans agree about whether one key
/// names the same create.
fn create_request_digest(shape: &CreateRequestShape) -> Result<String> {
    let mut payload = serde_json::json!({
        "title": shape.title,
        "sort": shape.sort,
        "cwd": shape.cwd,
        "template_id": shape.template_id,
        "recipe_id": shape.recipe_id,
        "template_input": shape.template_input,
        "attach_folder": shape.attach_folder,
        "theme": shape.theme,
        "fork_report_from": shape.fork_report_from,
    });
    // Existing durable bindings predate this optional field. Preserve their
    // exact payload shape when no authorization was supplied.
    if let Some(authorization) = &shape.allow_cross_area_cwd {
        payload["allow_cross_area_cwd"] = serde_json::to_value(authorization)?;
    }
    // Preserve the exact pre-selection digest for omitted/null defaults.
    if let Some(model) = &shape.model {
        payload["model"] = serde_json::json!(model);
    }
    if let Some(effort) = &shape.reasoning_effort {
        payload["reasoning_effort"] = serde_json::json!(effort);
    }
    stable_payload_hash(&payload)
}

/// Which create shape a request is. `create_request_sha256` omits `first_message`, so
/// without this a message-carrying create could resume onto a message-less binding and
/// answer 201 for a delivery that never happened.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CreateShape {
    WithFirstMessage,
    MessageLess,
}

/// `None` means "that create sent no message", never "the digest is unavailable" —
/// the legacy-unknown row refuses outright.
fn binding_fingerprint(
    binding: &crate::db::sqlite::TrackCreateBinding,
) -> Result<(&str, Option<&str>)> {
    match &binding.request_fingerprint {
        TrackCreateRequestFingerprint::LegacyUnknown => Err(CalmError::Conflict(format!(
            "this Idempotency-Key names track {} but predates durable request fingerprints, so \
             the server cannot safely decide whether this is the same create; inspect that track \
             before choosing a new key",
            binding.track_id
        ))),
        TrackCreateRequestFingerprint::V1 {
            create_request_sha256,
            first_message_sha256,
        } => Ok((create_request_sha256, Some(first_message_sha256))),
        TrackCreateRequestFingerprint::V2MessageLess {
            create_request_sha256,
        } => Ok((create_request_sha256, None)),
    }
}

/// The create shape is permanent once its track commits, so compare it as soon
/// as the binding is read — before retry-slot exhaustion can mask a payload
/// conflict and before any resume side effect.
fn ensure_binding_create_matches(
    binding: &crate::db::sqlite::TrackCreateBinding,
    create_request_sha256: &str,
    idempotency_key: &str,
    shape: CreateShape,
) -> Result<()> {
    let (bound_create_request_sha256, bound_first_message_sha256) = binding_fingerprint(binding)?;
    // Checked first, because the digest below omits `first_message`: a create that added
    // or dropped the sentence hashes to the same value.
    let bound_shape = match bound_first_message_sha256 {
        Some(_) => CreateShape::WithFirstMessage,
        None => CreateShape::MessageLess,
    };
    if bound_shape != shape {
        return Err(crate::operation::idempotency_payload_conflict(Some(
            idempotency_key,
        )));
    }
    if bound_create_request_sha256 != create_request_sha256 {
        return Err(crate::operation::idempotency_payload_conflict(Some(
            idempotency_key,
        )));
    }
    Ok(())
}

/// The message is the sole fingerprint exception. It is compared after arm
/// selection because a persisted terminal failure and fresh `#N` key represent
/// a new delivery attempt whose text may be edited.
fn ensure_binding_message_matches(
    binding: &crate::db::sqlite::TrackCreateBinding,
    plan: &FirstMessagePlan,
    allow_edited_message: bool,
) -> Result<()> {
    // `None` is unreachable here: `ensure_binding_create_matches` already
    // refused a message-less binding for this request's shape. Fail closed
    // rather than assume it, because the two checks are not adjacent.
    let (_, first_message_sha256) = binding_fingerprint(binding)?;
    let Some(first_message_sha256) = first_message_sha256 else {
        return Err(crate::operation::idempotency_payload_conflict(Some(
            &plan.idempotency_key,
        )));
    };
    if !allow_edited_message && first_message_sha256 != plan.first_message_sha256 {
        return Err(crate::operation::idempotency_payload_conflict(Some(
            &plan.idempotency_key,
        )));
    }
    Ok(())
}

/// A replay joins the chosen operation attempt, not necessarily the base attempt; once
/// an edited `#N` retry succeeds, its digest is the durable replay identity.
fn ensure_replay_message_matches(
    payload: &PlannerHarnessStartOperationPayload,
    plan: &FirstMessagePlan,
) -> Result<()> {
    // Fail closed on absence: a track-create operation always carries the sentence, so a
    // payload without one is corruption.
    let payload_text = payload.first_message.as_deref().ok_or_else(|| {
        CalmError::Internal(format!(
            "track-create operation {} has no first message",
            plan.operation_key
        ))
    })?;
    if first_message_digest(payload_text) != plan.first_message_sha256 {
        return Err(crate::operation::idempotency_payload_conflict(Some(
            &plan.idempotency_key,
        )));
    }
    Ok(())
}

/// The `first_message` twin of `create_track_with_planner_harness`, for the arm that mints.
pub(super) async fn create_track_with_first_message(
    s: RouteState,
    actor: Actor,
    p: NewTrack,
    mut options: CreateTrackOptions,
    plan: FirstMessagePlan,
) -> Result<Response> {
    // Conditioned on the plan, not on the closure running: `create_track_structure` is
    // reached by the unkeyed create too.
    // The rendezvous is `None` in production; a test seam for the cross-instance
    // primary-key race, held after lookup 1 missed and before the binding transaction opens.
    if let Some(gate) = s.track_create_mint_rendezvous.clone() {
        gate.hold().await;
    }
    options.idempotency_claim = Some(TrackCreateIdempotencyClaim {
        key: plan.idempotency_key.clone(),
        create_request_sha256: plan.create_request_sha256.clone(),
        first_message_sha256: Some(plan.first_message_sha256.clone()),
    });
    let (track, _created, planner_card_id, report_card_id) =
        create_track_structure(s.clone(), actor.clone(), p, options).await?;
    let cwd = track.workspace.path.clone();
    start_planner_harness_with_first_message(
        &s,
        &actor,
        SubmitArm::Mint,
        &track,
        planner_card_id,
        report_card_id,
        cwd,
        plan.text,
        plan.create_request_sha256,
        plan.operation_key,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(track)).into_response())
}

/// Adopt the track a previous attempt under this `Idempotency-Key` minted, and repair
/// its workspace — the half both resuming arms share.
async fn adopt_prior_track(s: &RouteState, track_id: &str) -> Result<Track> {
    // Direct replay materialization bypasses OperationRuntime, so take the same per-track
    // fence as lazy harness recovery; released before the operation is submitted to keep
    // the operation-drive → track-delete lock order.
    let track_delete_guard = crate::per_card_lock::lock_key(&s.track_delete_locks, track_id).await;
    // Fail closed: the binding row has no `ON DELETE CASCADE`, so a deleted track poisons
    // its key. 409 `idempotency_key_exhausted` is what makes the FE rotate to a fresh key.
    let track = s.repo.track_get(track_id).await?.ok_or_else(|| {
        CalmError::IdempotencyKeyExhausted(format!(
            "this Idempotency-Key names track {}, which has been deleted, so no retry under this \
             key can produce a working track; retry under a new Idempotency-Key, which mints a \
             fresh track",
            track_id
        ))
    })?;
    // `Resume` re-materializes: a 201 for a track whose workspace does not exist would
    // replay the create failure one layer down. `materialize_workspace` is designed to be re-run.
    crate::workspace_materialize::materialize_workspace(
        &track.workspace,
        &s.workspace_root,
        track.id.as_str(),
    )
    .map_err(|error| {
        tracing::error!(
            track_id = %track.id,
            path = %track.workspace.path,
            error = %error,
            "track create replay: workspace materialization failed"
        );
        // 409 `idempotency_key_exhausted`, not a 500: an unmarked non-empty directory is
        // reachable from a create crash and the fence refuses it forever, so the key is
        // poisoned; a new key mints a different path.
        CalmError::IdempotencyKeyExhausted(format!(
            "this Idempotency-Key names track {}, whose workspace can no longer be materialized, \
             so no retry under this key can produce a working track; retry under a new \
             Idempotency-Key, which mints a fresh track at a different path ({error})",
            track.id
        ))
    })?;
    drop(track_delete_guard);
    Ok(track)
}

/// The arms where this key already minted a track. Takes neither `NewTrack` nor
/// `CreateTrackOptions`: nothing here can mint.
pub(super) async fn resume_prior_attempt(
    s: RouteState,
    actor: Actor,
    resume: ResumeFirstMessage,
) -> Result<Response> {
    let ResumeFirstMessage { plan, prior } = resume;
    let track = adopt_prior_track(&s, &prior.track_id).await?;
    // The one place the two arms diverge: a replay owes the caller the selected
    // operation's payload byte for byte, a genuine retry owes it the world as it is now.
    let cwd = match prior.arm {
        PriorArm::Replay => prior
            .cwd
            .clone()
            .unwrap_or_else(|| track.workspace.path.clone()),
        PriorArm::GenuineRetry => track.workspace.path.clone(),
    };
    start_planner_harness_with_first_message(
        &s,
        &actor,
        SubmitArm::Resume,
        &track,
        prior.planner_card_id,
        prior.report_card_id,
        cwd,
        plan.text,
        plan.create_request_sha256,
        plan.operation_key,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(track)).into_response())
}

/// The message-less resuming arm: 201 with the key's own track, or 409
/// `idempotency_key_exhausted` when the track was deleted or its workspace can no longer
/// be materialized. Derives no operation key: `start_planner_harness` submits with
/// `idempotency_key: None`, so there is nothing to join.
pub(super) async fn resume_message_less(
    s: RouteState,
    actor: Actor,
    resume: MessageLessResume,
) -> Result<Response> {
    let MessageLessResume {
        track_id,
        planner_card_id,
        report_card_id,
        _same_key_claim,
    } = resume;
    let track = adopt_prior_track(&s, &track_id).await?;
    super::start_planner_harness(&s, &actor, &track, planner_card_id, report_card_id).await?;
    Ok((StatusCode::CREATED, Json(track)).into_response())
}

/// Which arm submitted, for the sole purpose of reading an `OperationOutcome`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SubmitArm {
    Mint,
    Resume,
}

/// Map an operation outcome onto this route's answer, per arm. `SucceededViaCollision`
/// is globally unreachable today (nothing writes the `idempotency_collision`
/// completion); the split is a fail-closed statement, not a signal the route depends on.
fn response_for(arm: SubmitArm, outcome: OperationOutcome) -> Result<()> {
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
async fn start_planner_harness_with_first_message(
    s: &RouteState,
    actor: &Actor,
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
        actor: actor.to_actor_id(),
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
        "actor": actor.as_str(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cross_area_authorization_is_part_of_idempotent_create_identity() {
        let shape = |allow_cross_area_cwd| CreateRequestShape {
            model: None,
            reasoning_effort: None,
            title: "shared cwd".into(),
            sort: None,
            cwd: Some("/repo".into()),
            template_id: None,
            recipe_id: None,
            template_input: None,
            attach_folder: false,
            allow_cross_area_cwd,
            theme: RequestTheme {
                fg: (1, 2, 3),
                bg: (4, 5, 6),
            },
            fork_report_from: None,
        };
        let denied = create_request_digest(&shape(None)).unwrap();
        let allowed =
            create_request_digest(&shape(Some(super::super::CrossAreaCwdAuthorization {
                folder_id: 7,
                area_id: "owner".into(),
            })))
            .unwrap();
        assert_ne!(
            denied, allowed,
            "authorization must require a distinct idempotency key"
        );
    }

    /// Golden, not a round trip: a self-consistency check would stay green if the namespace
    /// were merged into the conversation flavours'.
    #[test]
    fn the_track_create_key_is_a_pure_function_of_area_and_idempotency_key() {
        let key = derive_track_create_operation_key("area-1", "key-a");
        assert_eq!(
            key,
            // Independently computed: `sha256("track-create:area-1:key-a")`.
            "track-create-1c14cc746b371ade3520c32701cb2ff76e25a1bab237884e200a7d528c7af95f"
        );
        assert_ne!(key, derive_track_create_operation_key("area-1", "key-b"));
        assert_ne!(key, derive_track_create_operation_key("area-2", "key-a"));
    }

    /// The namespace separation, asserted by feeding ONE literal id to both derivations.
    #[test]
    fn the_track_create_namespace_never_collides_with_a_conversation_key() {
        let create = derive_track_create_operation_key("id-1", "key-a");
        let track = crate::conversation_keys::derive_track_conversation_keys("id-1", "key-a");
        assert_ne!(create, track.operation_key);
    }

    /// [`SelectedArm`]'s table, cell by cell.
    #[test]
    fn the_arm_is_decided_by_the_binding_then_by_what_sits_on_the_chosen_key() {
        let table = [
            // (binding_hit, chosen_is_occupied, expected)
            (false, false, SelectedArm::Mint),
            (false, true, SelectedArm::BindingLost),
            (true, true, SelectedArm::Replay),
            (true, false, SelectedArm::GenuineRetry),
        ];
        for (binding_hit, occupied, want) in table {
            assert_eq!(
                select_arm(binding_hit, occupied),
                want,
                "binding_hit={binding_hit} occupied={occupied}"
            );
        }
    }

    /// A collision outcome is a success only on a resuming arm. Constructed directly: the
    /// variant is globally unreachable, so there is no integration construction.
    #[test]
    fn a_collision_outcome_is_a_success_only_on_a_resume_arm() {
        let collision = || OperationOutcome::SucceededViaCollision {
            existing_op_id: "op-1".to_string(),
            result: serde_json::json!({}),
        };
        let plain = || OperationOutcome::Succeeded {
            result: serde_json::json!({}),
        };
        assert!(response_for(SubmitArm::Resume, collision()).is_ok());
        assert!(response_for(SubmitArm::Mint, plain()).is_ok());
        assert!(response_for(SubmitArm::Resume, plain()).is_ok());
        let refused = response_for(SubmitArm::Mint, collision())
            .expect_err("a fresh key cannot collide with itself");
        assert!(
            matches!(refused, CalmError::Internal(_)),
            "the mint arm must fail closed, not answer 201 for a delivery it did not make: \
             {refused:?}"
        );
    }
}
