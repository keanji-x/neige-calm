//! `POST /api/tracks`, the keyed half: **safe retry** (#1384).
//!
//! #1299 S1 made the create deliver the synthesiser page's first sentence
//! atomically. It deliberately did not make the create *retryable*: a client
//! that repeated one got a second track. This module is that second half.
//!
//! # Why a track create could not simply reuse the conversation machinery
//!
//! The two conversation write mouths are idempotent for free, because their
//! card id is `sha256(scope, Idempotency-Key)` — recomputable, so "does this
//! key already have one" is answered by looking the id up. A track id is
//! `new_id()` inside `track_create_tx`; it is not a function of any request
//! field. "Which track did this key create" therefore has to be **remembered**.
//!
//! Before #1384 the only row that remembered it was the `operations` row, and
//! `OperationRuntime::submit` writes that row *after* `adapter.validate`
//! succeeds. `PlannerHarnessStartAdapter::validate` refuses while the shared
//! codex app-server is down, so during a daemon outage the track, its two
//! cards, its folder claim and its workspace were all committed with **nothing
//! pointing at them**, and the next request under the same key minted another
//! track — one per retry, for as long as the outage lasted.
//!
//! The fix is a durable binding written **inside the mint transaction**:
//! `track_create_idempotency`, keyed `(area_id, Idempotency-Key)`, carrying the
//! track id and both card ids. On the arm that writes it there is no interval
//! in which the track exists and the binding does not, because they are the
//! same commit. That is what a preflight check could never buy: a preflight
//! only narrows the window (the daemon can stop between the check and
//! `submit`), and it regresses every in-transaction 4xx into a 500 during an
//! outage. It was measured, rejected, and is not re-proposed here.
//!
//! # What `Idempotency-Key` means here, and when it is required
//!
//! **Required if and only if the body carries `first_message`.** The new-track
//! route sends the header with its first message; message-less callers do not.
//! Making it unconditionally required would 400 those legacy creates, while
//! making it optional *with* a `first_message` would leave no dedup key at all,
//! so a retried create could mint a second track and deliver the instruction
//! twice.
//!
//! Given the key, the contract is the four-arm one `create_track_conversation`
//! documents, reused through `retryable_operation_key`: a success replays, a
//! terminal failure genuinely retries under a `#N` operation key, a `Stuck`
//! predecessor keeps failing closed, and 64 failed attempts exhaust the key
//! (409 `idempotency_key_exhausted`). A fifth statement follows from those: the
//! same operation attempt with a **different `first_message`** is 409
//! `conflict`. The base attempt is bound in the durable track-create row. After
//! a persisted terminal failure, a fresh `#N` operation is a new delivery
//! attempt whose own payload becomes the replay authority if it succeeds.
//! The create shape itself never gets that exception: once the track exists,
//! those inputs have already taken effect and no operation can reapply them.
//!
//! # A message-less create is still NOT idempotent, and that is stated
//!
//! [`plan_first_message`] returns [`CreatePlan::Legacy`] from its first
//! statement when `first_message` is absent — before the header is read. So a
//! message-less create writes no binding row, derives no key, and is
//! byte-for-byte the pre-#1299 path; a retry mints a second track exactly as it
//! always has. Writing the binding on `Legacy` too would be actively wrong:
//! `Legacy` has already returned from this dispatch, so there is no `Resume`
//! arm for a primary-key collision to map onto, and a message-less same-key
//! retry would turn a working 201 into an error. Pinned by
//! `a_message_less_create_writes_no_binding_row`.
//!
//! # The arm is decided BEFORE the create path validates the request
//!
//! Both resuming arms — replay and genuine retry — mint nothing: the track, its
//! cards and its folder claim already exist. So `create_track` decides the arm
//! first and, on those arms, returns through [`resume_prior_attempt`] **without
//! running a single one of the create path's request checks** (`cwd` shape,
//! attached-workspace existence, area 404, template admission, `template_input`
//! binding, folder claim).
//!
//! That is self-consistent rather than a carve-out. Those checks exist to
//! protect a *mint*, and there is no mint on these arms; and the very same
//! request already passed every one of them when it was first accepted.
//! Re-running them re-reads **mutable** state, and the state moves: delete the
//! directory a successful create attached, and a byte-identical replay used to
//! be answered `400 attached workspace ... does not exist` forever, for a track
//! that is alive.
//!
//! # A replay resubmits the chosen operation's payload, not today's state
//!
//! Which request is a replay is decided by **what already sits on the operation
//! key `retryable_operation_key` chose**, never by whether that key's name
//! carries a `#N` suffix — a suffix says a predecessor failed, not that this
//! request got a blank slot. [`select_arm`] is that decision as a table, and it
//! reads the binding row first.
//!
//! # What this module does NOT fix
//!
//! `create_track` is still not a compensating handler: it mints five kinds of
//! row and `materialize_workspace` runs after the commit, so "non-201 ⇒ no side
//! effect" remains false for it. What is guaranteed is the narrower thing:
//! under one `Idempotency-Key`, at most one track — and a rejected message
//! leaves no track at all. A create that carries no `first_message` keeps every
//! one of its old properties, good and bad.

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

/// Which arm of the contract this request takes, decided from **two** lookups:
/// the durable `track_create_idempotency` binding, and what sits on the chosen
/// operation key.
///
/// | binding row | operation on the chosen key | arm | mints? |
/// |---|---|---|---|
/// | miss | vacant | [`Self::Mint`] | yes |
/// | hit | occupied (non-`Failed`) | [`Self::Replay`] | no |
/// | hit | vacant | [`Self::GenuineRetry`] | no |
/// | miss | occupied | [`Self::BindingLost`] | no — 500, fail closed |
///
/// Two rows deserve their reason spelled out.
///
/// **`hit + vacant` is one row, not two.** It covers both "everything before
/// the chosen `#N` key terminally failed" and "there is no operation row under
/// this key at all" — the variant-4 shape, where `validate` refused before
/// `insert_operation` ever ran. Both mean the same thing: a track exists for
/// this key and nothing is currently executing against it, so this request
/// genuinely re-executes. Before the binding existed the second case was
/// indistinguishable from a fresh key, which is exactly how a daemon outage
/// minted one track per retry.
///
/// **`miss + occupied` is impossible and answers 500.** The binding commits
/// strictly before the operation is submitted, so an operation under this key
/// with no binding cannot arise from this route. Treating it as `Mint` — which
/// an earlier draft did — fails *open*: the mint would commit a track and its
/// cards, and `insert_operation` would then raise `idempotency_payload_conflict`
/// on the unique violation, leaving an orphan track behind a 409. That is
/// precisely the failure class this module exists to abolish, so the honest
/// answer to an unreachable state is an error, not a mint.
///
/// The criterion is deliberately **not** "does the chosen key carry a `#N`
/// suffix". `retryable_operation_key` stops at the first key that is absent
/// **or non-`Failed`**, so it can hand back a `#N` key that already holds a
/// *succeeded* attempt — base fails, `#2` succeeds, and the third,
/// byte-identical request is a replay of `#2`, not a retry of the base. Reading
/// the suffix answers `GenuineRetry` there, rebuilds `cwd` from a workspace a
/// `PATCH /api/tracks/{id}` may have repointed since, and turns a
/// byte-identical replay into a 409 forever
/// (`a_replay_of_a_success_that_happened_on_a_retry_key_survives_a_repoint`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SelectedArm {
    Mint,
    Replay,
    GenuineRetry,
    BindingLost,
}

/// Evaluate the table on [`SelectedArm`]. Pure, so every cell is unit-testable
/// without a database.
///
/// `chosen_is_occupied` is "`find_by_kind_and_idempotency` found an operation
/// under the chosen key" — the state of the *selected* key, not the shape of
/// its name.
fn select_arm(binding_hit: bool, chosen_is_occupied: bool) -> SelectedArm {
    match (binding_hit, chosen_is_occupied) {
        (false, false) => SelectedArm::Mint,
        (false, true) => SelectedArm::BindingLost,
        (true, true) => SelectedArm::Replay,
        (true, false) => SelectedArm::GenuineRetry,
    }
}

/// Whether the payload this request submits must be **frozen** to what the
/// predecessor submitted or **re-derived** from current state.
///
/// Modelled as a field on [`PriorAttempt`] rather than recomputed at the use
/// site: two copies of the same criterion drift, and the two arms want
/// *opposite* answers here, so a drift would silently swap them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PriorArm {
    /// The chosen key already holds an operation: this request is a **replay**
    /// of it.
    ///
    /// A replay must resubmit that operation's payload *byte for byte*, or
    /// `OperationRuntime::submit` compares a different `payload_hash` and
    /// answers 409 `conflict` — telling a caller who sent a byte-identical
    /// request that it changed its message, permanently and indistinguishably
    /// from the genuine different-body conflict.
    Replay,
    /// The chosen key is vacant: this request **genuinely executes**.
    ///
    /// It must therefore describe the world as it is **now**, not as the failed
    /// attempt saw it: no earlier payload hash is bound to a vacant key, so
    /// there is nothing to stay byte-identical to, and reusing a stale `cwd`
    /// would start the harness in a directory that may since have been moved or
    /// recycled out from under the track.
    GenuineRetry,
}

/// What a previous attempt under this `Idempotency-Key` already minted.
///
/// The three ids come from the **binding row**, not from an operation payload.
/// That is the change #1384 makes: the payload cannot be the source, because in
/// the variant-4 shape there is no operation row at all. Reading them back from
/// a role query would be well-defined — `idx_cards_one_planner_per_track` and
/// `idx_cards_one_report_per_track` are both single-valued — but it would be a
/// second source of truth for a value the mint already knew.
struct PriorAttempt {
    arm: PriorArm,
    track_id: String,
    planner_card_id: String,
    report_card_id: String,
    /// The chosen operation's `cwd`, replayed verbatim on [`PriorArm::Replay`].
    /// `None` on [`PriorArm::GenuineRetry`], which takes `track.workspace.path`
    /// instead — there is no operation on a vacant key to read one from.
    ///
    /// # Why `cwd` is the whole class of frozen fields
    ///
    /// `payload_hash` covers the whole payload, so *any* field this route
    /// derives from mutable server state has the same "must freeze on replay"
    /// property. Going through `PlannerHarnessStartOperationPayload` as
    /// [`start_planner_harness_with_first_message`] fills it:
    ///
    /// - `actor` — from the request's authenticated principal, not from state.
    /// - `track_id`, `planner_card_id`, `report_card_id` — taken from the
    ///   binding row, i.e. already frozen on both arms.
    /// - `cwd` — **the one remaining field read from live state**
    ///   (`track.workspace.path`), and mutable: `PATCH /api/tracks/{id}`
    ///   repoints a managed workspace to an attached one at any time. Hence
    ///   this field.
    /// - `first_message`, `first_message_sha256`, `create_request_sha256` —
    ///   pure functions of the request body. The create digest is always checked
    ///   against the durable binding. The message is checked against the chosen
    ///   operation on Replay, against the binding on an operation-less base
    ///   resume, and may be edited only on the genuine `#N` retry after a
    ///   persisted terminal failure.
    /// - `sort`, `goal`, `create_card` — hard-coded `None` here.
    /// - the two reset/force-new-thread flags — hard-coded `false`.
    /// - `profile` — hard-coded `Default::default()`.
    ///
    /// If a future field is added here that reads the track, a card, the
    /// workspace root or any other row, it belongs in this struct too.
    cwd: Option<String>,
}

/// What `POST /api/tracks` decided before it validated — let alone ran — any of
/// the create path.
///
/// The three variants are the handler's whole fork, and they are a *type*
/// rather than an `Option<PriorAttempt>` field on purpose: a
/// [`FirstMessagePlan`] structurally cannot carry a prior attempt, so the
/// minting path cannot be reached with one, and [`ResumeFirstMessage`]
/// structurally always has one, so the resuming path cannot be reached without
/// one.
pub(super) enum CreatePlan {
    /// The body carried no `first_message`: the pre-#1299 path verbatim. The
    /// `Idempotency-Key` header is not read, no key is derived, no lookup
    /// happens, no binding row is written, and `create_track` runs its checks in
    /// the order it always did.
    Legacy,
    /// A `first_message` on a key with no binding to adopt. This request
    /// **mints**, so the create path's request validation runs in full.
    Mint(FirstMessagePlan),
    /// A `first_message` on a key a prior attempt already minted under. This
    /// request mints nothing, so the create path — validation included — is
    /// skipped entirely; see the module docs.
    Resume(ResumeFirstMessage),
}

/// A [`CreatePlan::Resume`]'s payload: the shared plan plus the prior attempt
/// that makes it a resume.
pub(super) struct ResumeFirstMessage {
    plan: FirstMessagePlan,
    prior: PriorAttempt,
}

/// Every request field that decides the minted track, in its deserialized
/// request shape. Its digest is persisted in the same row as the track id, so a
/// missing operation row cannot erase request identity.
///
/// Cloned at the call site right after the body is deserialized and **before**
/// `CreationSource::stamp` writes the roster's template spelling. The
/// fingerprint deliberately describes what the caller sent; deriving it from
/// admitted or normalized live state would rerun mutable validation on Resume,
/// which is the variant-3 class this module avoids.
pub(super) struct CreateRequestShape {
    pub title: String,
    pub sort: Option<f64>,
    pub cwd: Option<String>,
    pub template_id: Option<String>,
    pub recipe_id: Option<String>,
    pub template_input: Option<serde_json::Value>,
    pub attach_folder: bool,
    pub theme: RequestTheme,
    pub fork_report_from: Option<String>,
}

/// Everything a keyed `POST /api/tracks` needs to submit the operation.
pub(super) struct FirstMessagePlan {
    text: String,
    /// The digest of [`CreateRequestShape`], carried here so it reaches
    /// `resume_prior_attempt` as well.
    ///
    /// It travels on the plan rather than as a `resume_prior_attempt`
    /// parameter on purpose: that function's signature takes neither
    /// `NewTrack` nor `CreateTrackOptions`, and that absence is what makes
    /// "this arm cannot mint" compiler-enforced. A digest is not a mint input,
    /// so the invariant survives.
    create_request_sha256: String,
    /// The initial message digest is stored in the binding row too. Unlike the
    /// create digest, it may be relaxed after a persisted terminal failure,
    /// when the next `#N` operation is a new delivery attempt.
    first_message_sha256: String,
    /// The caller's `Idempotency-Key`, verbatim. On the `Mint` arm it is half
    /// of the binding row's primary key; on the resuming arms it is unused,
    /// because the binding has already been read.
    idempotency_key: String,
    /// The key to submit the `planner-harness-start` operation under, already
    /// stepped past any terminally failed predecessor.
    operation_key: String,
    /// Held from before the two lookups until after the operation settles, so
    /// two concurrent creates under one key cannot both read "no binding" and
    /// each mint a track.
    ///
    /// In-process only, so it degrades on a multi-instance deployment — which
    /// is why the binding table's primary key exists underneath it as the
    /// cross-process wall. Taken OUTER, never nested inside
    /// `planner_recovery_locks`; this path never calls `send_planner_input`, so
    /// it takes no inner map at all and closes no cycle.
    ///
    /// The map is keyed by card id elsewhere; the key used here is the
    /// `track-create-{sha256}` operation key, which no card id can spell.
    _same_key_claim: crate::per_card_lock::PerCardLockGuard,
}

/// `SHA-256("track-create:{area_id}:{key}")`, prefixed `track-create-`.
///
/// Its own namespace, deliberately. `conversation_keys` hashes two other
/// prefixes for the two lazy-mint conversation flavours (see that module — the
/// literals there are frozen hash INPUT, which is why they are not restated
/// here); a track create keyed on an area id would collide with the area-chat
/// flavour's `(area_id, key)` pair if it shared a prefix, and one
/// `Idempotency-Key` would then address a conversation card and a track create
/// at once.
fn derive_track_create_operation_key(area_id: &str, idempotency_key: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(format!("track-create:{area_id}:{idempotency_key}"));
    format!("track-create-{}", hex::encode(hasher.finalize()))
}

/// Parse and validate the first-message half of the request, and pick the arm —
/// **before** `create_track` validates, let alone mints, anything.
///
/// Returns [`CreatePlan::Legacy`] when the body carried no `first_message`,
/// which is the unchanged legacy path: the header is not read, no key is
/// derived, no lookup happens, and the create proceeds exactly as it did before.
pub(super) async fn plan_first_message(
    s: &RouteState,
    headers: &HeaderMap,
    first_message: Option<String>,
    area_id: &str,
    shape: CreateRequestShape,
) -> Result<CreatePlan> {
    let Some(text) = first_message else {
        return Ok(CreatePlan::Legacy);
    };
    let idempotency_key = parse_idempotency_key_header(headers)?.ok_or_else(|| {
        CalmError::BadRequest(
            "Idempotency-Key header is required when `first_message` is present, so a retried create cannot mint a second track or deliver the message twice"
                .into(),
        )
    })?;
    // Byte-identical to `POST /api/cards/{id}/planner/input`'s rules, and run
    // here — before the folder claim, the track row, the planner/report cards,
    // the overlays and `materialize_workspace` — so a rejected message leaves no
    // track behind.
    validate_first_message(&text)?;

    let create_request_sha256 = stable_payload_hash(&serde_json::json!({
        "title": shape.title,
        "sort": shape.sort,
        "cwd": shape.cwd,
        "template_id": shape.template_id,
        "recipe_id": shape.recipe_id,
        "template_input": shape.template_input,
        "attach_folder": shape.attach_folder,
        "theme": shape.theme,
        "fork_report_from": shape.fork_report_from,
    }))?;
    let first_message_sha256 = first_message_digest(&text);
    let base_key = derive_track_create_operation_key(area_id, &idempotency_key);
    // Taken before either lookup, released when the plan is dropped at the end
    // of the request. See the field's doc comment.
    let same_key_claim = lock_card(&s.conversation_first_message_locks, &base_key).await;

    // Lookup 1 — the new authority for "does a track already exist for this
    // key". This is the whole of #1384: it is answered by a row that committed
    // with the id, so it is still answered after every failure that leaves no
    // operation row behind.
    let binding = s
        .repo
        .track_create_idempotency_get(area_id, &idempotency_key)
        .await?;
    if let Some(binding) = binding.as_ref() {
        ensure_binding_create_matches(binding, &create_request_sha256, &idempotency_key)?;
    }

    // Lookup 2 — unchanged in role: which harness-start *attempt* this request
    // joins, and whether a `Failed` predecessor is stepped over with `#N`.
    // May 409 `idempotency_key_exhausted`. Deliberately before any mint: a
    // used-up key must not create a track on its way to the refusal.
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
            // Consume the chosen operation once so both the message criterion
            // and the replayed cwd come from the exact attempt this request is
            // joining. A successful edited `#N` retry is not represented by the
            // binding's original-message digest; its operation payload is the
            // durable authority for later replays.
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

fn binding_fingerprint(binding: &crate::db::sqlite::TrackCreateBinding) -> Result<(&str, &str)> {
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
        } => Ok((create_request_sha256, first_message_sha256)),
    }
}

/// The create shape is permanent once its track commits, so compare it as soon
/// as the binding is read — before retry-slot exhaustion can mask a payload
/// conflict and before any resume side effect.
fn ensure_binding_create_matches(
    binding: &crate::db::sqlite::TrackCreateBinding,
    create_request_sha256: &str,
    idempotency_key: &str,
) -> Result<()> {
    let (bound_create_request_sha256, _) = binding_fingerprint(binding)?;
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
    let (_, first_message_sha256) = binding_fingerprint(binding)?;
    if !allow_edited_message && first_message_sha256 != plan.first_message_sha256 {
        return Err(crate::operation::idempotency_payload_conflict(Some(
            &plan.idempotency_key,
        )));
    }
    Ok(())
}

/// A replay joins the chosen operation attempt, not necessarily the base
/// attempt recorded by the track binding. After a terminal failure the caller
/// may edit the message on a fresh `#N` key; once that attempt succeeds, its
/// digest is the durable replay identity.
fn ensure_replay_message_matches(
    payload: &PlannerHarnessStartOperationPayload,
    plan: &FirstMessagePlan,
) -> Result<()> {
    let first_message_sha256 = payload.first_message_sha256.as_deref().ok_or_else(|| {
        CalmError::Internal(format!(
            "track-create operation {} has no first-message fingerprint",
            plan.operation_key
        ))
    })?;
    if first_message_sha256 != plan.first_message_sha256 {
        return Err(crate::operation::idempotency_payload_conflict(Some(
            &plan.idempotency_key,
        )));
    }
    Ok(())
}

/// The `first_message` twin of `create_track_with_planner_harness`, for the arm
/// that actually mints ([`CreatePlan::Mint`]).
///
/// Reached only after the create path's request validation, exactly like the
/// message-less path: this is the one arm that consumes a `NewTrack` and
/// `CreateTrackOptions`, so it is the one arm those checks protect.
pub(super) async fn create_track_with_first_message(
    s: RouteState,
    actor: Actor,
    p: NewTrack,
    mut options: CreateTrackOptions,
    plan: FirstMessagePlan,
) -> Result<Response> {
    // #1384 — the `Mint`-arm condition on the binding write, in one place.
    //
    // `create_track_structure` is reached by BOTH arms of the message-less
    // dispatch too (`create_track_with_planner_harness` calls it), so
    // conditioning the write on "the closure ran" would write a binding for
    // `Legacy` creates as well. It is conditioned on the plan instead: this
    // function is the only writer of the field, and it is reachable only from
    // `CreatePlan::Mint`. Pinned by
    // `a_message_less_create_writes_no_binding_row`.
    options.idempotency_claim = Some(TrackCreateIdempotencyClaim {
        key: plan.idempotency_key.clone(),
        create_request_sha256: plan.create_request_sha256.clone(),
        first_message_sha256: plan.first_message_sha256.clone(),
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
        plan.first_message_sha256,
        plan.create_request_sha256,
        plan.operation_key,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(track)).into_response())
}

/// The arms where this key **already** minted a track ([`CreatePlan::Resume`]).
///
/// Takes neither `NewTrack` nor `CreateTrackOptions`, and that absence is the
/// structural statement: nothing here can mint, so `create_track` is right to
/// have skipped the request validation that guards minting — see the module docs
/// for why re-running it was actively wrong.
pub(super) async fn resume_prior_attempt(
    s: RouteState,
    actor: Actor,
    resume: ResumeFirstMessage,
) -> Result<Response> {
    let ResumeFirstMessage { plan, prior } = resume;
    // Direct replay materialization bypasses OperationRuntime, so take the
    // same per-track fence as lazy harness recovery. It is released before the
    // operation is submitted, preserving the operation-drive → track-delete
    // order used by DELETE while preventing a replay from recreating a path
    // already being moved to trash.
    let track_delete_guard =
        crate::per_card_lock::lock_key(&s.track_delete_locks, &prior.track_id).await;
    // Fail closed. A 201 here would have to mint a replacement track under a key
    // that already means "that track", i.e. answer a byte-identical request with
    // a *different* track. The binding row deliberately has no `ON DELETE
    // CASCADE`, so a deleted track poisons its key rather than silently
    // recycling it.
    let track = s.repo.track_get(&prior.track_id).await?.ok_or_else(|| {
        CalmError::Internal(format!(
            "track {} recorded by an earlier attempt under this Idempotency-Key no longer exists",
            prior.track_id
        ))
    })?;
    // #1384 — `Resume` re-materializes, and the mint arm's failure semantics is
    // inherited rather than softened.
    //
    // The failure points this arm exists for include "process died between the
    // COMMIT and `materialize_workspace`" and "`materialize_workspace` returned
    // `Err`". Inheriting the reference branch's resume verbatim — `track_get`
    // then submit — would answer 201 for a track whose workspace does not exist,
    // which is the #1147 failure replayed one layer down.
    //
    // Re-running it is safe because the function is *designed* to be re-run:
    // `Attached` is an unconditional no-op; on `Managed` the owner marker gates
    // everything and its own comment says a half-built directory left by a crash
    // is repairable; steady state costs one `rev-parse`, and the worker lease
    // path already calls it on every acquisition for exactly this reason.
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
        // 409 `idempotency_key_exhausted`, not a generic 500, and this is the
        // one behavioural change this arm makes.
        //
        // The fence in `materialize_workspace` refuses an unmarked non-empty
        // directory forever, and that state IS reachable from a create crash:
        // `write_owner_marker` creates `<path>/.git` and only then writes the
        // marker, so death between those two syscalls leaves a directory that
        // has entries and no marker. Relaxing the fence would mean allowlisting
        // "the only entry is `.git/`", a marker-absence heuristic no positive
        // fingerprint can replace, so the fence stands.
        //
        // The trade, in both directions: before #1384 that window produced a
        // *second* track at a fresh path and the user got a working one. Now the
        // key is poisoned and every retry under it re-materializes the same dead
        // path. That is a liveness regression in a narrow window, bought for a
        // correctness fix — and the escape needs no new machinery, because the
        // poisoning is per-key: a new `Idempotency-Key` misses the binding, mints
        // a fresh id, and a managed path is derived from *that* id, so it is a
        // different directory. `idempotency_key_exhausted` already means "this
        // key is used up; retry under a new one", which is exactly the actionable
        // instruction. The underlying message is carried verbatim so the dead
        // path is named. Pinned by
        // `a_resume_onto_an_unmarked_non_empty_workspace_is_key_exhausted` and
        // `a_new_idempotency_key_recovers_from_a_poisoned_workspace`.
        CalmError::IdempotencyKeyExhausted(format!(
            "this Idempotency-Key names track {}, whose workspace can no longer be materialized, \
             so no retry under this key can produce a working track; retry under a new \
             Idempotency-Key, which mints a fresh track at a different path ({error})",
            track.id
        ))
    })?;
    drop(track_delete_guard);
    // The one place the two arms diverge. See `PriorArm`: a replay owes the
    // caller the selected operation's payload byte for byte, a genuine retry
    // owes it the world as it is now.
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
        plan.first_message_sha256,
        plan.create_request_sha256,
        plan.operation_key,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(track)).into_response())
}

/// Which arm submitted, for the sole purpose of reading an `OperationOutcome`.
/// See [`response_for`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SubmitArm {
    Mint,
    Resume,
}

/// Map an operation outcome onto this route's answer, per arm.
///
/// Split out of the `match` it used to be folded into, because the fold's
/// written justification was "`SucceededViaCollision` is unreachable from this
/// call site, since it submits `idempotency_key: None`" — and this module
/// submits one. That ground is gone; a comment that says something false is
/// worse than no comment.
///
/// **The variant nevertheless stays globally unreachable**, on a second and
/// independent ground this issue does not touch: its sole producer,
/// `operation_result_from`, requires a persisted
/// `phase_detail.completion == "idempotency_collision"`, and nothing in this
/// repository writes that key. `submit`'s collision short-circuit returns the
/// *existing* operation's id, and `wait` then reads that operation's own durable
/// row, whose `phase_detail` carries no `completion`. So a replay comes back as
/// plain `Succeeded`.
///
/// The arm is therefore what actually decides replay semantics — "the message
/// was delivered, but not by THIS request" is exactly what `CreatePlan::Resume`
/// means, computed before anything is submitted. The split below is a
/// fail-closed statement about a state that should not arise, not a runtime
/// signal the route depends on.
fn response_for(arm: SubmitArm, outcome: OperationOutcome) -> Result<()> {
    match outcome {
        OperationOutcome::Succeeded { .. } => Ok(()),
        // A fresh key cannot collide with itself. Reaching this on the minting
        // arm would mean the operation this request just submitted resolved to
        // an *earlier* one — i.e. the key was not fresh after all, and a 201
        // would promise a delivery this request did not make.
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

/// What a create that promised a delivery says when the harness start did not
/// complete.
///
/// **The endpoint still cannot say whether the message was delivered, and this
/// text does not pretend otherwise.** `harness.user_message.enqueued` proves
/// only an *attempt*: `prepare_tx` seeds the observation and writes the audit row
/// in a transaction that commits at `TxCommitted`, the later `AppServerInteract`
/// can still fail, `events` is append-only, and compensation only marks the
/// runtime failed. There is no other durable record of the turn leaving, so no
/// read the handler can perform answers the question. And a *negative* claim
/// would be a lie on the `Stuck` path, where `spawn_side_effect` has already
/// installed a live harness and fired the turn while the phase write failed.
///
/// What #1384 does add is the actionable half, and only the two things it can
/// prove: a retry under the same `Idempotency-Key` creates no second track (the
/// binding row) and delivers no second copy (`retryable_operation_key` does not
/// step over `Stuck`, so the retry resolves to the same operation and replays
/// the recorded failure). It deliberately does **not** promise the track is
/// usable — a replay does not repair an attached workspace whose directory was
/// deleted — so the text says so rather than implying health by omission.
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

/// Submit `planner-harness-start` carrying the first message.
///
/// Deliberately NOT the `tracing::warn!` + `Ok(())` best-effort shape
/// `start_planner_harness` uses for the message-less path. There the track is
/// the whole deliverable and an inert planner agent is recoverable; here the
/// request also promised to deliver a sentence, and answering 201 for an
/// operation that never enqueued it would tell the user their instruction
/// arrived when it did not. A 5xx is also what makes the genuine-retry arm
/// usable: the client retries under the same key and the retry re-executes.
#[allow(clippy::too_many_arguments)]
async fn start_planner_harness_with_first_message(
    s: &RouteState,
    actor: &Actor,
    arm: SubmitArm,
    track: &Track,
    planner_card_id: String,
    report_card_id: String,
    // `cwd` is NOT `track.workspace.path`: on a replay it is the chosen
    // operation's `cwd`, so the resubmitted payload hashes to the same value
    // even if the workspace was repointed in between. See `PriorArm`.
    cwd: String,
    text: String,
    first_message_sha256: String,
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
        // Binds the body into `payload_hash` (belt to the braces of the text
        // field below, which is already part of the payload): replaying one key
        // with a different sentence is a 409 instead of a silent replay of the
        // first one.
        first_message_sha256: Some(first_message_sha256),
        first_message: Some(text),
        // #1384 / #1434 — also carried in the operation payload for its local
        // collision check. The durable authority is now the binding row, which
        // covers every mint input and exists even when this operation does not.
        // Other producers leave the field `None`, preserving their payload
        // bytes across deployment.
        create_request_sha256: Some(create_request_sha256),
    };
    let op_payload = serde_json::to_value(&request)?;
    // Same hash shape as `start_planner_harness`, so the two paths cannot drift
    // on what a payload is.
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

    /// Golden, not a round trip. A self-consistency check would stay green if
    /// the namespace were merged into the conversation flavours', which is the
    /// one thing this derivation has to keep apart.
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

    /// The namespace separation, asserted where it can actually be constructed:
    /// feed ONE literal id to both derivations. A route-level test could never
    /// distinguish "separate namespaces" from "different inputs".
    #[test]
    fn the_track_create_namespace_never_collides_with_a_conversation_key() {
        let create = derive_track_create_operation_key("id-1", "key-a");
        let track = crate::conversation_keys::derive_track_conversation_keys("id-1", "key-a");
        assert_ne!(create, track.operation_key);
    }

    /// T-ARM-1 — [`SelectedArm`]'s table, cell by cell, over the two inputs that
    /// decide it.
    ///
    /// The load-bearing cells are the two on the right: the binding row alone
    /// decides whether this request may mint, and what sits on the chosen
    /// operation key decides only whether it replays or re-executes. Before
    /// #1384 the second input was asked to answer both questions, and it cannot
    /// answer the first when `validate` refused before the row existed.
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

    /// T-COLL-1 — a collision outcome is a success only on a resuming arm.
    ///
    /// Constructed directly: the variant is globally unreachable (see
    /// [`response_for`]'s doc comment for the surviving reason), so there is no
    /// integration construction and none is faked.
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
