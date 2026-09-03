//! `POST /api/tracks`, the first-message half (#1299 S1).
//!
//! The synthesiser page (`/area/{id}/new`) asks the user one question: what is
//! this track for. Before this slice the answer went nowhere — the track was
//! created, the user landed on it, and had to type the sentence a second time.
//! This module delivers it **with** the create, atomically.
//!
//! # Why this is not `goal`
//!
//! `PlannerHarnessStartOperationPayload::goal` becomes an
//! `Observation::TrackGoal` (`harness::initial_snapshot_with_goal`), and
//! `TrackGoal` is a different semantic slot from `UserMessage`: it renders as
//! bare text instead of `"User says:\n…"`, it does not hard-fire, and
//! `run_loop`'s `UserMessage must not fold into TrackGoal` assertion exists to
//! keep the two apart. A sentence a human typed is a `UserMessage`; using
//! `goal` would also drop the human attribution and write no
//! `harness.user_message.enqueued` audit row. `goal`'s one live producer stays
//! the machine-written child-track bootstrap (`scheduler`).
//!
//! # Why the delivery lives in the operation
//!
//! `routes/area_conversations.rs` documents two known gaps — a first-message
//! claim that is neither request-scoped nor transactional — and names the fix:
//! *fold the first message into the same operation*. This slice does exactly
//! that for `POST /api/tracks`; `PlannerHarnessStartAdapter::prepare_tx` seeds the
//! `Observation::UserMessage` and writes its audit row inside the transaction
//! that starts the harness. The two conversation routes migrate in #1314 and
//! are deliberately untouched here.
//!
//! # What `Idempotency-Key` means here, and when it is required
//!
//! **Required if and only if the body carries `first_message`.** That is not a
//! taste call: a caller sweep by directory (`crates/`, `fe/`, `web/`, `e2e/`,
//! `plugins/`, `scripts/`, `docs/`) found that *no* caller in the repository
//! sends the header to this endpoint today — not the two production frontends
//! (`fe/core/domain/track.ts`, `web/src/api/calm.ts`, whose `request()` helper
//! cannot even set extra headers), not the ~150 Rust integration call sites,
//! not the Playwright or shell e2e helpers. Making it unconditionally required
//! would 400 every one of them. Making it optional *with* a `first_message`
//! would be worse: there would be no dedup key at all, so a retried create
//! would mint a second track and deliver the instruction twice.
//!
//! Given the key, the contract is the four-arm one `create_area_conversation`
//! documents, reused verbatim through `retryable_operation_key`: a success
//! replays, a terminal failure genuinely retries under a `#N` operation key, a
//! `Stuck` predecessor keeps failing closed, and 64 failed attempts exhaust the
//! key (409 `idempotency_key_exhausted`).
//!
//! A fifth statement follows from those and is written down because reading it
//! as unconditional contradicts the second arm: the same key with a **different
//! `first_message`** is 409 `conflict` — the text is bound into the payload
//! hash — **except after a terminal failure**, where the retry runs under a
//! fresh `#N` operation key that no earlier payload hash is bound to. An edited
//! sentence resent after a failed attempt is therefore not rejected *for the
//! old hash*: it genuinely re-executes against the track that attempt already
//! created, and its status is whatever that execution produces. That is the
//! kernel's existing behaviour, not a carve-out this route adds, and
//! `the_same_key_after_a_failure_accepts_an_edited_first_message` pins it.
//!
//! # A replay resubmits the chosen operation's payload, not today's state
//!
//! Which request is a replay is decided by **what already sits on the operation
//! key `retryable_operation_key` chose**, never by whether that key's name
//! carries a `#N` suffix — a suffix says a predecessor failed, not that this
//! request got a blank slot. `PriorSelection` is that decision as a table.
//!
//! Arm (a) says a replayed success returns the same track. That only holds if
//! the replay's payload hashes to the same value, and the payload carries the
//! track's `cwd` — which `PATCH /api/tracks/{id}` can repoint from a managed
//! workspace to an attached one at any time. Reading `track.workspace.path`
//! **now** would therefore turn a byte-identical replay of a create into a 409
//! `conflict` claiming the caller changed its message, for that key, forever,
//! and indistinguishably from the genuine arm-(e) conflict. So the replay arm
//! resubmits the chosen operation's own `cwd`; see [`PriorArm`] for why the genuine-retry
//! arm must do the opposite, and [`PriorAttempt::cwd`] for the field-by-field
//! audit that says `cwd` is the only field with this property.
//!
//! Arm (a) has one deliberate hole, and it fails closed: if the track the
//! chosen operation recorded has since been **deleted**, the replay cannot
//! return it and answers 500 rather than minting a replacement under a key that
//! already means "that track". A 201 there would hand the caller a *different*
//! track for a byte-identical request — the exact confusion the key exists to
//! prevent — so the honest answer is the error. Pinned by the `no longer
//! exists` branch in [`resume_prior_attempt`].
//!
//! # The arm is decided BEFORE the create path validates the request
//!
//! Both prior-attempt arms — replay (a/c/d) and genuine retry (b) — mint
//! nothing: the track, its cards and its folder claim already exist. So
//! `create_track` decides the arm first and, on those arms, returns through
//! [`resume_prior_attempt`] **without running a single one of the create path's
//! request checks** (`cwd` shape, attached-workspace existence, area 404,
//! template admission, `template_input` binding, folder claim).
//!
//! That is self-consistent rather than a carve-out. Those checks exist to
//! protect a *mint*, and there is no mint on these arms, so "validate before
//! mint" is vacuously true on them; and the very same request already passed
//! every one of them when it was first accepted. Running them again means
//! re-reading **mutable** state — and the state moves: delete the directory a
//! successful create attached, and a byte-identical replay used to be answered
//! 400 `attached workspace ... does not exist` forever, for a track that is
//! alive and whose workspace `PATCH /api/tracks/{id}` may since have repointed
//! somewhere perfectly valid. Arm (b) was truncated by the same read: the retry
//! is going to start the harness in the workspace the track has **now**, yet it
//! could not reach that decision through a 400 about the directory the *failed*
//! attempt named.
//!
//! Requests with no `first_message` never reach the arm decision at all —
//! `plan_first_message` returns [`CreatePlan::Legacy`] before reading the
//! header — so their ordering is byte-for-byte the pre-#1299 one.
//!
//! # What this slice does NOT fix
//!
//! `create_track` is still not a compensating handler: it mints five kinds of
//! row and `materialize_workspace` runs after the commit, so "non-201 ⇒ no side
//! effect" remains false for it (`routes/tracks.rs` says so in its own words).
//! What this slice does guarantee is the narrower thing it can: the
//! `first_message` validation runs before **any** mint, so a rejected message
//! never leaves a track behind — and so does the daemon-availability preflight
//! (`require_running` in [`create_track_with_first_message`]), because the one
//! failure that used to leave a track behind *repeatedly* was a refusal raised
//! inside `submit`'s `validate`, i.e. before the operation row that records
//! which track this key created ever exists.

use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::{Json, http::StatusCode};

use crate::actor::Actor;
use crate::error::{CalmError, Result};
use crate::ids::CardId;
use crate::model::{NewTrack, Track};
use crate::operation::planner_harness_start_adapter::PlannerHarnessStartOperationPayload;
use crate::operation::{OperationKey, OperationOutcome};
use crate::per_card_lock::lock_card;
use crate::shared_codex_appserver::SharedCodexAppServer;
use crate::routes::conversations_shared::{
    PLANNER_HARNESS_START, first_message_digest, retryable_operation_key, validate_first_message,
};
use crate::routes::terminal_cards::{
    calm_error_from_operation_failure, parse_idempotency_key_header, stable_payload_hash,
};
use crate::state::RouteState;

use super::{CreateTrackOptions, create_track_structure};

/// Which arm of the four-arm contract selected this prior attempt — and
/// therefore whether the payload this request submits must be **frozen** to
/// what the predecessor submitted or **re-derived** from current state.
///
/// Computed once, in `plan_first_message`, by [`select_prior`]. Modelled as a
/// field rather than recomputed at the use site on purpose: two copies of the
/// same criterion drift, and the two arms want *opposite* answers here, so a
/// drift would silently swap them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PriorArm {
    /// The chosen key already holds an operation: this request is a **replay**
    /// of it (arm (a), or a still-in-flight duplicate, or the fail-closed
    /// `Stuck` case).
    ///
    /// A replay must resubmit that operation's payload *byte for byte*, or
    /// `OperationRuntime::submit` compares a different `payload_hash` and
    /// answers 409 `conflict` — telling a caller who sent a byte-identical
    /// request that it changed its message, permanently and
    /// indistinguishably from the genuine arm-(e) conflict.
    Replay,
    /// The chosen key is a **vacant** `#N` slot, i.e. everything before it in
    /// the chain terminally failed: this request is a **genuine retry**
    /// (arm (b)) that will really execute.
    ///
    /// It must therefore describe the world as it is **now**, not as the failed
    /// attempt saw it: no earlier payload hash is bound to the `#N` key, so
    /// there is nothing to stay byte-identical to, and reusing a stale `cwd`
    /// would start the harness in a directory that may since have been moved or
    /// recycled out from under the track.
    GenuineRetry,
}

/// The whole replay-vs-retry decision, as a table over the one thing that
/// actually decides it: **what sits on the operation key
/// `retryable_operation_key` chose**.
///
/// | operation on the chosen key | variant | cwd comes from | response |
/// |---|---|---|---|
/// | none, and the chosen key is the base | [`Self::FreshKey`] | current state | 201, mints a new track |
/// | present and `Succeeded` | [`Self::ReplayChosen`] | that operation's own payload | 201, same track, not re-delivered |
/// | present and in flight | [`Self::ReplayChosen`] | same | 201, joins it; never a second track |
/// | present and `Stuck` | [`Self::ReplayChosen`] | same | the recorded 500, fail-closed |
/// | none, and the chosen key is `#N` | [`Self::RetryAfter`] | current state | 201, starts on the workspace the track has now |
///
/// The criterion is deliberately **not** "does the chosen key carry a `#N`
/// suffix". A suffix says a *predecessor* failed; it does not say this request
/// got a blank slot. `retryable_operation_key` stops at the first key that is
/// absent **or non-`Failed`**, so it can hand back a `#N` key that already
/// holds a succeeded attempt — base fails, `#2` succeeds, and the third,
/// byte-identical request is a replay of `#2`, not a retry of the base. Reading
/// the suffix answers `GenuineRetry` there, rebuilds `cwd` from a workspace a
/// `PATCH /api/tracks/{id}` may have repointed since, and turns a byte-identical
/// replay into a 409 forever
/// (`a_replay_of_a_success_that_happened_on_a_retry_key_survives_a_repoint`).
///
/// `Stuck` and in-flight are **not** a third semantic arm: any non-`Failed`
/// operation on the chosen key replays, and what the caller sees is whatever
/// that operation's outcome already is (a track for a success, the recorded
/// `500 operation stuck, see DB` for a `Stuck`). `Failed` never reaches here —
/// `retryable_operation_key` steps over it, which is the only reason "present"
/// can be collapsed to one row.
#[derive(Clone, PartialEq, Eq, Debug)]
enum PriorSelection {
    /// Nothing under this key at all, and no predecessor to adopt: this is the
    /// first attempt, so `create_track_structure` mints the track.
    FreshKey,
    /// The chosen key already holds a (non-`Failed`) operation. Replay it,
    /// resubmitting its payload verbatim.
    ReplayChosen,
    /// The chosen key is a vacant `#N` slot. Adopt the terminally failed
    /// predecessor named here — its track, planner card and report card — but
    /// describe `cwd` as it is now.
    RetryAfter(String),
}

/// Evaluate the table on [`PriorSelection`]. Pure, so the table is unit-testable
/// without an `operations` row per row of it.
///
/// `chosen_is_occupied` is "`find_by_kind_and_idempotency` found an operation
/// under `chosen`" — the state of the *selected* key, not the shape of its name.
fn select_prior(base: &str, chosen: &str, chosen_is_occupied: bool) -> PriorSelection {
    if chosen_is_occupied {
        return PriorSelection::ReplayChosen;
    }
    match predecessor_operation_key(base, chosen) {
        None => PriorSelection::FreshKey,
        Some(previous) => PriorSelection::RetryAfter(previous),
    }
}

/// What a previous attempt under this `Idempotency-Key` already minted, plus
/// the payload fields a replay has to reproduce verbatim.
///
/// Recovered from the **selected operation's** persisted payload rather than
/// re-derived. Which operation that is differs by arm, and the distinction is
/// the whole point of [`PriorSelection`]: on [`PriorArm::Replay`] it is the
/// operation sitting on the *chosen* key (which may itself be a `#N` key), on
/// [`PriorArm::GenuineRetry`] it is the terminally failed *predecessor* the
/// chosen vacant `#N` slot follows. Either way the operation row is the only
/// place that remembers which track this key created — the track id is minted
/// by `track_create_tx` and is not a function of the key. Reusing it is what
/// makes "same key ⇒ same track" hold on **both** arms; without it a retry
/// after a failed harness start would hand the user a second track.
struct PriorAttempt {
    arm: PriorArm,
    track_id: String,
    planner_card_id: String,
    report_card_id: String,
    /// The selected operation's `cwd`, replayed verbatim on
    /// [`PriorArm::Replay`] — and on that arm the selected operation is the one
    /// on the **chosen** key, not a predecessor of it. (Saying "the
    /// predecessor's cwd" here would describe the criterion the `#N`-suffix bug
    /// used and `PriorSelection` replaced.) It is read but unused on
    /// [`PriorArm::GenuineRetry`], which takes `track.workspace.path` instead.
    ///
    /// # Field-by-field audit of `PlannerHarnessStartOperationPayload`
    ///
    /// `payload_hash` covers the whole payload, so *any* field this route
    /// derives from mutable server state has the same "must freeze on replay"
    /// property. Going through the struct as
    /// `start_planner_harness_with_first_message` fills it:
    ///
    /// - `actor` — from the request's authenticated principal, not from state.
    ///   A byte-identical request by the same caller reproduces it; a
    ///   *different* caller replaying someone else's key is a genuinely
    ///   different request and 409 is the right answer there.
    /// - `track_id`, `planner_card_id`, `report_card_id` — already taken from this
    ///   struct, i.e. already frozen to the predecessor's payload on both arms
    ///   (that is what makes "same key ⇒ same track" hold).
    /// - `cwd` — **the one remaining field read from live state**
    ///   (`track.workspace.path`), and mutable: `PATCH /api/tracks/{id}` repoints
    ///   a managed workspace to an attached one at any time. Hence this field.
    /// - `first_message`, `first_message_sha256` — a pure function of the
    ///   request body. Two different bodies *should* be a 409; that is arm (e).
    /// - `sort`, `goal`, `create_card` — hard-coded `None` here.
    /// - the two reset/force-new-thread flags — hard-coded `false`.
    /// - `profile` — hard-coded `Default::default()`.
    ///
    /// So `cwd` is the whole class, not one instance of it; the constants and
    /// the request-derived fields cannot drift between an attempt and its
    /// replay. If a future field is added here that reads the track, a card, the
    /// workspace root or any other row, it belongs in this struct too.
    cwd: String,
}

/// What `POST /api/tracks` decided before it validated — let alone ran — any
/// of the create path.
///
/// The three variants are the handler's whole fork, and they are a *type*
/// rather than an `Option<PriorAttempt>` field on purpose: a
/// [`FirstMessagePlan`] structurally cannot carry a prior attempt, so the
/// minting path cannot be reached with one, and [`ResumeFirstMessage`]
/// structurally always has one, so the resuming path cannot be reached without
/// one. That is what keeps "the arm decides whether the request is validated"
/// from being a rule someone has to remember.
pub(super) enum CreatePlan {
    /// The body carried no `first_message`: the pre-#1299 path verbatim. The
    /// `Idempotency-Key` header is not read, no key is derived, no operation
    /// lookup happens, and `create_track` runs its checks in the order it
    /// always did.
    Legacy,
    /// A `first_message` on a key with no prior attempt to adopt. This request
    /// **mints**, so the create path's request validation runs in full.
    Mint(FirstMessagePlan),
    /// A `first_message` on a key a prior attempt already minted under (arm
    /// (a)/(c)/(d) replay, or arm (b) genuine retry). This request mints
    /// nothing, so the create path — validation included — is skipped
    /// entirely; see the module docs.
    Resume(ResumeFirstMessage),
}

/// A [`CreatePlan::Resume`]'s payload: the shared plan plus the prior attempt
/// that makes it a resume.
pub(super) struct ResumeFirstMessage {
    plan: FirstMessagePlan,
    prior: PriorAttempt,
}

/// Everything a **minting** `POST /api/tracks` needs to submit the operation.
pub(super) struct FirstMessagePlan {
    text: String,
    /// The key to submit the `planner-harness-start` operation under, already
    /// stepped past any terminally failed predecessor.
    operation_key: String,
    /// Held from before the prior-attempt lookup until after the operation
    /// settles, so two concurrent creates under one key cannot both read "no
    /// prior attempt" and each mint a track.
    ///
    /// Necessary because — unlike the conversation routes — the track id is NOT
    /// derived from the key: `track_create_tx` mints it, so there is no
    /// "`validate` refuses to re-mint an existing id" wall underneath. The
    /// operation row is the only record of which track a key created, and it is
    /// written by the submit this guard spans.
    ///
    /// Same two premises as every other holder of this map (see `state.rs`):
    /// in-process only, so it degrades on a multi-instance deployment; and it
    /// is taken OUTER, never nested inside `planner_recovery_locks`. This path
    /// holds it across a `planner-harness-start` operation exactly as the Today
    /// bootstrap does — that operation takes no per-card lock of its own
    /// outside itself, so no cycle closes.
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

/// The operation key of the attempt immediately before `chosen` in the
/// `base`, `base#2`, `base#3`, … chain `retryable_operation_key` walks.
///
/// `None` means `chosen` IS the base, so there is no earlier attempt in the
/// chain at all. Returning the predecessor rather than re-walking the chain
/// keeps `retryable_operation_key` the single authority on which key is chosen.
///
/// Only [`select_prior`]'s vacant-`#N` row calls this, and only to name the
/// attempt whose track a genuine retry adopts. It is **not** the replay-vs-retry
/// criterion; see `PriorSelection` for why a `#N` name proves nothing about
/// this request.
fn predecessor_operation_key(base: &str, chosen: &str) -> Option<String> {
    let suffix = chosen.strip_prefix(base)?.strip_prefix('#')?;
    let n: u32 = suffix.parse().ok()?;
    match n {
        0..=2 => Some(base.to_string()),
        n => Some(format!("{base}#{}", n - 1)),
    }
}

/// Parse and validate the first-message half of the request, and pick the arm —
/// **before** `create_track` validates, let alone mints, anything.
///
/// Returns [`CreatePlan::Legacy`] when the body carried no `first_message`,
/// which is the unchanged legacy path: the header is not read, no key is
/// derived, no operation lookup happens, and the create proceeds exactly as it
/// did before this slice.
pub(super) async fn plan_first_message(
    s: &RouteState,
    headers: &HeaderMap,
    first_message: Option<String>,
    area_id: &str,
    as_template: bool,
) -> Result<CreatePlan> {
    let Some(text) = first_message else {
        return Ok(CreatePlan::Legacy);
    };
    // A template track never starts a planner harness (`as_template` skips
    // `start_planner_harness` entirely), so there is nothing that could carry the
    // message. Refusing is the honest answer; accepting it would silently drop
    // the user's sentence.
    if as_template {
        return Err(CalmError::BadRequest(
            "`first_message` is not accepted together with `as_template`: a template track starts no planner harness to deliver it to".into(),
        ));
    }
    let idempotency_key = parse_idempotency_key_header(headers)?.ok_or_else(|| {
        CalmError::BadRequest(
            "Idempotency-Key header is required when `first_message` is present, so a retried create cannot mint a second track or deliver the message twice"
                .into(),
        )
    })?;
    // Byte-identical to `POST /api/cards/{id}/planner/input`'s rules, and run
    // here — before the folder claim, the track row, the planner/report cards, the
    // overlays and `materialize_workspace` — so a rejected message leaves no
    // track behind.
    validate_first_message(&text)?;

    let base_key = derive_track_create_operation_key(area_id, &idempotency_key);
    // Taken before the chain is read, released when the plan is dropped at the
    // end of the request. See the field's doc comment.
    let same_key_claim = lock_card(&s.conversation_first_message_locks, &base_key).await;
    // May 409 `idempotency_key_exhausted`. Deliberately before any mint: a
    // used-up key must not create a track on its way to the refusal.
    let operation_key = retryable_operation_key(s, &base_key).await?;
    // What sits on the chosen key is the criterion — see `PriorSelection`. Read
    // once, under `same_key_claim`, and reused as the replay payload so the two
    // never disagree.
    let chosen_existing = s
        .operation_runtime
        .find_by_kind_and_idempotency(PLANNER_HARNESS_START, &operation_key)
        .await?;
    // The single place the replay/retry criterion is evaluated. `None` here is
    // the table's `FreshKey` row (or a `#N` predecessor whose row has since been
    // deleted): nothing to adopt, so the create mints.
    let adopted: Option<(PriorArm, String, serde_json::Value)> =
        match select_prior(&base_key, &operation_key, chosen_existing.is_some()) {
            PriorSelection::FreshKey => None,
            // `chosen_existing` is `Some` by construction of this row — it is
            // the very thing that selected it. Consumed rather than re-read so
            // the criterion and the replayed payload cannot disagree.
            PriorSelection::ReplayChosen => {
                chosen_existing.map(|op| (PriorArm::Replay, operation_key.clone(), op.payload))
            }
            PriorSelection::RetryAfter(previous) => s
                .operation_runtime
                .find_by_kind_and_idempotency(PLANNER_HARNESS_START, &previous)
                .await?
                .map(|op| (PriorArm::GenuineRetry, previous, op.payload)),
        };
    let prior = match adopted {
        None => None,
        Some((arm, prior_key, payload)) => {
            let payload: PlannerHarnessStartOperationPayload = serde_json::from_value(payload)?;
            // Every payload this route writes sets `report_card_id`; a `None`
            // here means the operation row under this key was written by
            // something else. Fail closed. Defaulting it to an empty `CardId`
            // would carry that foreign attempt's track into a 201 whose harness
            // start names a report card that does not exist — a silent wrong
            // answer where a 500 is the honest one.
            let report_card_id = payload.report_card_id.ok_or_else(|| {
                CalmError::Internal(format!(
                    "operation {prior_key} under this Idempotency-Key has no report_card_id; \
                     it was not written by POST /api/tracks"
                ))
            })?;
            Some(PriorAttempt {
                arm,
                track_id: payload.track_id,
                planner_card_id: payload.planner_card_id.to_string(),
                report_card_id,
                cwd: payload.cwd,
            })
        }
    };
    let plan = FirstMessagePlan {
        text,
        operation_key,
        _same_key_claim: same_key_claim,
    };
    Ok(match prior {
        None => CreatePlan::Mint(plan),
        Some(prior) => CreatePlan::Resume(ResumeFirstMessage { plan, prior }),
    })
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
    daemon: &SharedCodexAppServer,
    p: NewTrack,
    options: CreateTrackOptions,
    plan: FirstMessagePlan,
) -> Result<Response> {
    // #1299 S1 adjudication — the daemon-availability preflight, run BEFORE the
    // mint instead of only inside `submit`.
    //
    // `OperationRuntime::submit` calls `adapter.validate` *before*
    // `insert_operation`, so a refusal there writes no operation row at all.
    // On this arm the track, its cards and its folder claim are already
    // committed by then, and the operation row is the ONLY record of which
    // track an `Idempotency-Key` created. So a daemon outage used to leave a
    // track with nothing pointing at it, and the next request under the same
    // key read `PriorSelection::FreshKey` again and minted another one — one
    // fresh track per retry, for as long as the outage lasted. Measured on the
    // fixture: two requests, one key, two tracks, four cards, zero operations.
    // That is not the declared "this handler does not compensate" exemption
    // (one failed create MAY leave one track); it contradicts the header's own
    // contract that a retried create lands on the track the key already names.
    //
    // `require_running` is the adapter's own criterion, called — not restated:
    // a second copy would drift, and the two disagreeing is precisely the state
    // that produces the orphan.
    //
    // Residual window, stated rather than papered over: the daemon can still
    // stop between this call and `submit`'s `validate`, which re-runs it. That
    // window is one `create_track_structure` wide (one transaction plus
    // `materialize_workspace`), and inside it the old behaviour returns. What
    // changes is that a *steady-state* outage — the reachable case, where every
    // attempt fails for minutes — no longer mints anything at all.
    daemon.require_running()?;
    let (track, _created, planner_card_id, report_card_id) =
        create_track_structure(s.clone(), actor.clone(), p, options).await?;
    let cwd = track.workspace.path.clone();
    start_planner_harness_with_first_message(
        &s,
        &actor,
        &track,
        planner_card_id,
        report_card_id,
        cwd,
        plan.text,
        plan.operation_key,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(track)).into_response())
}

/// The arms where this key **already** minted a track ([`CreatePlan::Resume`]):
/// replay (a/c/d) and genuine retry (b).
///
/// Takes neither `NewTrack` nor `CreateTrackOptions`, and that absence is the
/// structural statement: nothing here can mint, so `create_track` is right to
/// have skipped the request validation that guards minting — see the module
/// docs for why re-running it was actively wrong (a deleted directory turned a
/// byte-identical replay into a permanent 400).
///
/// Everything from the payload onwards — submit, wait, outcome mapping — is
/// shared with the minting arm, which is what makes a replayed success and a
/// genuine retry produce the same response shape.
pub(super) async fn resume_prior_attempt(
    s: RouteState,
    actor: Actor,
    resume: ResumeFirstMessage,
) -> Result<Response> {
    let ResumeFirstMessage { plan, prior } = resume;
    // Fail closed. A 201 here would have to mint a replacement track under a
    // key that already means "that track", i.e. answer a byte-identical request
    // with a different track. See the module docs.
    let track = s.repo.track_get(&prior.track_id).await?.ok_or_else(|| {
        CalmError::Internal(format!(
            "track {} recorded by an earlier attempt under this Idempotency-Key no longer exists",
            prior.track_id
        ))
    })?;
    // The one place the two arms diverge. See `PriorArm`: a replay owes the
    // caller the selected operation's payload byte for byte, a genuine retry
    // owes it the world as it is now.
    let cwd = match prior.arm {
        PriorArm::Replay => prior.cwd,
        PriorArm::GenuineRetry => track.workspace.path.clone(),
    };
    start_planner_harness_with_first_message(
        &s,
        &actor,
        &track,
        prior.planner_card_id,
        prior.report_card_id,
        cwd,
        plan.text,
        plan.operation_key,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(track)).into_response())
}

/// Submit `planner-harness-start` carrying the first message.
///
/// Deliberately NOT the `tracing::warn!` + `Ok(())` best-effort shape
/// `start_planner_harness` uses for the message-less path. There the track is the
/// whole deliverable and an inert planner agent is recoverable; here the request
/// also promised to deliver a sentence, and answering 201 for an operation that
/// never enqueued it would tell the user their instruction arrived when it did
/// not. A 5xx is also what makes arm (b) of the contract usable: the client
/// retries under the same key and the retry genuinely re-executes.
#[allow(clippy::too_many_arguments)]
async fn start_planner_harness_with_first_message(
    s: &RouteState,
    actor: &Actor,
    track: &Track,
    planner_card_id: String,
    report_card_id: String,
    // `cwd` is NOT `track.workspace.path`: on a replay it is the predecessor's
    // `cwd`, so the resubmitted payload hashes to the same value even if the
    // workspace was repointed in between. See `PriorArm`.
    cwd: String,
    text: String,
    operation_key: String,
) -> Result<()> {
    let request = PlannerHarnessStartOperationPayload {
        actor: actor.to_actor_id(),
        track_id: track.id.to_string(),
        planner_card_id: CardId::from(planner_card_id),
        report_card_id: Some(report_card_id),
        sort: None,
        cwd,
        // See the module docs: the user's sentence is a `UserMessage`, and
        // `goal` stays reserved for the machine-written child-track bootstrap.
        goal: None,
        reset_harness_items: false,
        force_new_thread: false,
        profile: Default::default(),
        create_card: None,
        // Binds the body into `payload_hash` (belt to the braces of the text
        // field below, which is already part of the payload): replaying one key
        // with a different sentence is a 409 instead of a silent replay of the
        // first one.
        first_message_sha256: Some(first_message_digest(&text)),
        first_message: Some(text),
    };
    let op_payload = serde_json::to_value(&request)?;
    // Same hash shape as `start_planner_harness`, so the two paths cannot drift on
    // what a payload is.
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
    match result.outcome {
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

    /// The namespace separation, asserted where it can actually be
    /// constructed: feed ONE literal id to both derivations. A route-level test
    /// could never distinguish "separate namespaces" from "different inputs".
    #[test]
    fn the_track_create_namespace_never_collides_with_a_conversation_key() {
        let create = derive_track_create_operation_key("id-1", "key-a");
        let track = crate::conversation_keys::derive_track_conversation_keys("id-1", "key-a");
        assert_ne!(create, track.operation_key);
    }

    /// `retryable_operation_key` walks `base`, `base#2`, `base#3`, …; this is
    /// the inverse step, and getting it wrong would make a retry adopt the
    /// wrong attempt's track (or none at all, minting a second track).
    #[test]
    fn the_predecessor_of_each_retry_key_is_the_attempt_before_it() {
        let base = "track-create-abc";
        assert_eq!(predecessor_operation_key(base, base), None);
        assert_eq!(
            predecessor_operation_key(base, "track-create-abc#2").as_deref(),
            Some(base)
        );
        assert_eq!(
            predecessor_operation_key(base, "track-create-abc#3").as_deref(),
            Some("track-create-abc#2")
        );
        assert_eq!(
            predecessor_operation_key(base, "track-create-abc#64").as_deref(),
            Some("track-create-abc#63")
        );
    }

    /// `PriorSelection`'s table, row by row, over the two inputs that decide it.
    ///
    /// The load-bearing rows are the two `#2` ones: the SAME key name selects
    /// opposite arms depending only on whether an operation already sits on it.
    /// A criterion that reads the suffix cannot tell them apart, and that is
    /// exactly the bug this replaces.
    #[test]
    fn the_arm_is_decided_by_what_sits_on_the_chosen_key_not_by_its_name() {
        let base = "track-create-abc";
        let table = [
            // (chosen, occupied, expected)
            (base, false, PriorSelection::FreshKey),
            (base, true, PriorSelection::ReplayChosen),
            (
                "track-create-abc#2",
                false,
                PriorSelection::RetryAfter(base.to_string()),
            ),
            ("track-create-abc#2", true, PriorSelection::ReplayChosen),
            (
                "track-create-abc#7",
                false,
                PriorSelection::RetryAfter("track-create-abc#6".to_string()),
            ),
            ("track-create-abc#7", true, PriorSelection::ReplayChosen),
        ];
        for (chosen, occupied, want) in table {
            assert_eq!(
                select_prior(base, chosen, occupied),
                want,
                "chosen={chosen} occupied={occupied}"
            );
        }
    }
}
