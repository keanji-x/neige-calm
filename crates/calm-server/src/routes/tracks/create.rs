//! `POST /api/tracks`, the keyed half: safe retry under an `Idempotency-Key`.
//! Under one key, at most one track; a create that sends no key keeps its old properties.

use crate::extract::Json;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::actor::Actor;
use crate::db::sqlite::TrackCreateRequestFingerprint;
use crate::error::{CalmError, Result};
use crate::ids::ActorId;
use crate::model::{NewTrack, RequestTheme, Track};
use crate::operation::planner_harness_start_adapter::PlannerHarnessStartOperationPayload;
use crate::per_card_lock::lock_card;
use crate::routes::conversations_shared::{
    PLANNER_HARNESS_START, first_message_digest, retryable_operation_key, validate_first_message,
};
use crate::routes::idempotency_key::{parse_idempotency_key_header, stable_payload_hash};
use crate::state::RouteState;

use super::{CreateTrackOptions, TrackCreateIdempotencyClaim, create_track_structure};

mod delivery;
use delivery::{SubmitArm, start_planner_harness_with_first_message};

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
    pub planner_provider: crate::session_projection_repo::AgentProvider,
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

/// The binding-key prefix of `neige_track_add`, followed by `<creator_track_id>/<key>`. The tool
/// owns this namespace: `POST /api/tracks` refuses a key that starts with it.
pub(super) const TRACK_ADD_KEY_PREFIX: &str = "track-add/";

/// Who a keyed create acts as. `POST /api/tracks`: the declared header actor throughout.
/// `neige_track_add`: the Planner session for the create transaction, whose role gate resolves it
/// live, and that session's Planner card for the first-message delivery, an operation that may
/// outlive the session.
pub(super) struct KeyedActor {
    pub(super) create: ActorId,
    pub(super) start: ActorId,
    /// The `actor` the delivery's payload hash binds; a REST create keeps the header's spelling.
    pub(super) start_label: String,
}

impl KeyedActor {
    pub(super) fn rest(actor: &Actor) -> Self {
        Self {
            create: actor.to_actor_id(),
            start: actor.to_actor_id(),
            start_label: actor.as_str().to_string(),
        }
    }
}

/// One keyed create with a first message, as its caller derives it: the binding row's
/// `(area_id, idempotency_key)` and the request fingerprint that key binds.
pub(super) struct KeyedCreate {
    pub(super) area_id: String,
    pub(super) idempotency_key: String,
    pub(super) create_request_sha256: String,
    pub(super) text: String,
}

/// What [`plan_keyed_create`] decided: mint under a vacant key, or resume what it minted.
pub(super) enum KeyedPlan {
    Mint(FirstMessagePlan),
    Resume(ResumeFirstMessage),
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
    if let Some(key) = &idempotency_key
        && key.starts_with(TRACK_ADD_KEY_PREFIX)
    {
        return Err(CalmError::IdempotencyKeyInvalid(format!(
            "the key must not start with `{TRACK_ADD_KEY_PREFIX}`: that namespace belongs to {}",
            crate::mcp_server::tools::track_add::TOOL_TRACK_ADD
        )));
    }
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
    let create_request_sha256 = create_request_digest(&shape)?;
    Ok(
        match plan_keyed_create(
            s,
            KeyedCreate {
                area_id: area_id.to_string(),
                idempotency_key,
                create_request_sha256,
                text,
            },
        )
        .await?
        {
            KeyedPlan::Mint(plan) => CreatePlan::Mint(plan),
            KeyedPlan::Resume(resume) => CreatePlan::Resume(resume),
        },
    )
}

/// The keyed create's decision, before anything is validated or minted: the binding lookup,
/// the fingerprint check and the arm. `POST /api/tracks` and `neige_track_add` both run it.
pub(super) async fn plan_keyed_create(s: &RouteState, request: KeyedCreate) -> Result<KeyedPlan> {
    let KeyedCreate {
        area_id,
        idempotency_key,
        create_request_sha256,
        text,
    } = request;
    let area_id = area_id.as_str();
    // Run before the folder claim, the track row, the cards and `materialize_workspace`,
    // so a rejected message leaves no track behind.
    validate_first_message(&text)?;

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
        SelectedArm::Mint => Ok(KeyedPlan::Mint(plan)),
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
            Ok(KeyedPlan::Resume(ResumeFirstMessage { plan, prior }))
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
    // Every binding before `planner_provider` existed is a Codex create: Codex keeps that exact
    // digest, and any other provider can never match one.
    if shape.planner_provider != crate::session_projection_repo::AgentProvider::Codex {
        payload["planner_provider"] = serde_json::to_value(&shape.planner_provider)?;
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
        // `idempotency_key_reused`: the key is bound to a create this request cannot be shown to be,
        // so no retry under it can be answered for this one.
        TrackCreateRequestFingerprint::LegacyUnknown => {
            Err(CalmError::IdempotencyKeyReused(format!(
                "this Idempotency-Key names track {} but predates durable request fingerprints, so \
             the server cannot safely decide whether this is the same create; inspect that track \
             before choosing a new key",
                binding.track_id
            )))
        }
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
    actor: &KeyedActor,
    p: NewTrack,
    mut options: CreateTrackOptions,
    plan: FirstMessagePlan,
) -> Result<Track> {
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
        create_track_structure(s.clone(), actor.create.clone(), p, options).await?;
    let cwd = track.workspace.agent_cwd().to_string();
    start_planner_harness_with_first_message(
        &s,
        actor,
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
    Ok(track)
}

/// Adopt the track a previous attempt under this `Idempotency-Key` minted, and repair
/// its workspace — the half both resuming arms share. `ensure_worktree` re-runs the track
/// worktree ensure for an arm that submits the world as it is now; a replay passes `false`,
/// because its recorded payload proves the mint's ensure succeeded.
async fn adopt_prior_track(s: &RouteState, track_id: &str, ensure_worktree: bool) -> Result<Track> {
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
    // Inside the same fence as the materialization above. Its own error, never the
    // `idempotency_key_exhausted` mapping: that makes the FE rotate the key and mint a second
    // track, while a diverged checkout or a missing commit is the user's to fix.
    if ensure_worktree {
        crate::operation::workspace_lease::track_worktree::ensure_track_worktree(&track).await?;
    }
    drop(track_delete_guard);
    Ok(track)
}

/// The arms where this key already minted a track. Takes neither `NewTrack` nor
/// `CreateTrackOptions`: nothing here can mint.
pub(super) async fn resume_prior_attempt(
    s: RouteState,
    actor: &KeyedActor,
    resume: ResumeFirstMessage,
) -> Result<Track> {
    let ResumeFirstMessage { plan, prior } = resume;
    let track = adopt_prior_track(
        &s,
        &prior.track_id,
        matches!(prior.arm, PriorArm::GenuineRetry),
    )
    .await?;
    // The one place the two arms diverge: a replay owes the caller the selected
    // operation's payload byte for byte, a genuine retry owes it the world as it is now.
    let cwd = match prior.arm {
        PriorArm::Replay => prior
            .cwd
            .clone()
            .unwrap_or_else(|| track.workspace.agent_cwd().to_string()),
        PriorArm::GenuineRetry => track.workspace.agent_cwd().to_string(),
    };
    start_planner_harness_with_first_message(
        &s,
        actor,
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
    Ok(track)
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
    let track = adopt_prior_track(&s, &track_id, true).await?;
    super::start_planner_harness(&s, &actor, &track, planner_card_id, report_card_id).await?;
    Ok((StatusCode::CREATED, Json(track)).into_response())
}

#[cfg(test)]
mod provider_tests;

#[cfg(test)]
mod tests;
