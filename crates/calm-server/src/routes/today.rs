//! Server-owned Today launchpad bootstrap (#951, Slice A).

use crate::actor::Actor;
use crate::db::rows::TRACK_SELECT_COLUMNS;
use crate::db::sqlite::{
    area_create_system_tx, card_create_with_id_tx, card_update_tx, card_with_terminal_create_tx,
    track_workspace_write_tx,
};
use crate::db::{write_in_tx_typed, write_with_event_typed};
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, CardId, TrackId};
use crate::model::{
    Card, CardPatch, CardRole, NewCard, RequestTheme, Terminal, Track, TrackWorkspace,
    TrackWorkspaceKind, new_id, now_ms,
};
use crate::operation::planner_harness_start_adapter::PlannerHarnessStartOperationPayload;
use crate::operation::{OperationKey, OperationOutcome};
use crate::routes::terminal_cards::stable_payload_hash;
use crate::state::{AppState, RouteState};
use crate::track_report::TrackReportPayload;
use crate::validation::CODEX_PAYLOAD_SCHEMA_VERSION;
use axum::{
    Json, Router,
    extract::{FromRef, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::Serialize;
use sqlx::{Sqlite, Transaction};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use utoipa::ToSchema;
// #1147 — one definition of the path digest, shared with the scheduler's
// child-track bootstrap key.
use crate::workspace_materialize::workspace_key_digest;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/today/launchpad/ensure", post(ensure_today_launchpad))
        .route("/api/today/launchpad", get(resolve_today_launchpad))
        .route(
            "/api/today/launchpad/report/reset",
            post(reset_today_launchpad_report),
        )
}

#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct TodayLaunchpad {
    pub track_id: String,
    pub planner_card_id: String,
    pub terminal_card_id: String,
    pub terminal_id: String,
}

/// #1253 §5.1 — what the Today **page load** reads.
///
/// A deliberately narrow, read-only DTO. It is not [`TodayLaunchpad`] and it
/// does not grow into it: `ensure`'s shape is the bootstrap's, this one is the
/// reader's, and the two answer different questions.
///
/// There is no `report_card_id` here on purpose. The track detail already
/// returns the track's cards and the frontend locates the report by
/// `kind == "track-report"` (`fe/core/domain/report.ts::readTrackReport`), so
/// such a field would have no consumer.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct TodayLaunchpadResolved {
    pub track_id: String,
    /// Whether this report's `summary`/`body` differ **right now** from the
    /// canonical freshly-minted pair.
    ///
    /// It is NOT "has anyone ever written it": no history is consulted, so
    /// none can be reported, and restoring the text to the canonical pair
    /// turns this back to `false`. Do not build a "the summary has run" marker
    /// on it — see the first bullet below.
    ///
    /// It is computed server-side by
    /// [`TrackReportPayload::report_startup_read_required`], the kernel's one
    /// canonical "has this been written" predicate. It is deliberately NOT
    /// named `report_started`, and the difference is not cosmetic (design D7):
    ///
    /// * **It is a statement about the report's CURRENT content, not about its
    ///   history.** The name says exactly that, and the name is the contract:
    ///   it is `has_noninitial_content`, not `has_ever_been_written`. Restoring
    ///   `summary` and `body` byte-for-byte to the canonical initial pair
    ///   flips it back to `false`, whatever happened in between — no history is
    ///   consulted, so none can be reported.
    /// * It therefore also answers "has *anyone* written it", not "has today's
    ///   summary run": a user hand-editing the document flips it exactly as a
    ///   summary agent would, and a stale document still reads as content.
    ///   Anything that really needs "did the summary run" needs a durable
    ///   marker or event, not this.
    /// * It compares `summary + body` only; `doc_rev` and `blocks` are
    ///   deliberately ignored, so a canonical placeholder that CRDT has
    ///   already materialised still reads `false` — and so does a report whose
    ///   text was reverted to canonical while those two stayed non-zero.
    ///
    /// The frontend must not re-derive this by looking at the report body:
    /// `readTrackReport` returns non-null for the canonical initial report
    /// (its body carries the maintenance-contract comment and four H1s), so a
    /// null-check there renders four empty headings instead of an empty state.
    pub report_has_noninitial_content: bool,
}

struct EnsureTxResult {
    dto: TodayLaunchpad,
    track: Track,
    report_card_id: String,
    created: bool,
    adopted_legacy: bool,
    /// #1147 — the planner harness has never successfully started at the
    /// launchpad's *current* workspace path, so its thread must be re-opened.
    ///
    /// True on a fresh mint, on the one-time migration of a pre-S2 row, and on
    /// every retry after a failure in between — because it is derived from
    /// `operations` rather than from a one-shot in-memory comparison (N3).
    /// Without that, a materialize failure or a crash between commit and
    /// operation-submit would silently strand the agent in the old directory.
    repointed: bool,
}

/// Does `error` carry SQLite's unique-violation message for `constraint`?
///
/// **`constraint` is a COLUMN list, never an index name.** SQLite words a
/// unique violation as `UNIQUE constraint failed: <table>.<column>[, …]` — it
/// names the index only for a unique index over *expressions*. Both indexes
/// this module races on (`idx_areas_one_system` on `areas(kind)`,
/// `idx_tracks_one_launchpad` on `tracks(purpose)`) are partial indexes over
/// plain columns, so their messages read `areas.kind` and `tracks.purpose` and
/// contain no index name at all. Passing the index name here matches nothing:
/// the arm becomes dead code and the race surfaces as a 500 instead of the
/// retry it was written to perform. `routes::tracks` has always used the column
/// form (`tracks.area_id`); this module did not until #1253 PR1.
/// The `constraint` argument for the system-area race, and the ONLY place that
/// string is written.
///
/// **What binds it, precisely** — two different mutations, two different
/// carriers, both measured:
///
/// * Reverting *this constant* to an index name →
///   `tests::sqlite_names_the_columns_not_the_indexes_for_both_partial_unique_violations`
///   goes red, because it asserts against the constant.
/// * Reverting the *call site* to an inline index literal →
///   `today_launchpad::concurrent_first_ensure_retries_the_system_area_race`
///   goes red **at its HTTP status assertion**, which fires on the losing
///   request's 500. NOT at its `retries == 1` assertion, which that mutation
///   never reaches. What [`SystemAreaMintCounters`] contributes there is not
///   the failing assertion but its *validity*: `attempts == 2` proves the race
///   happened at all, without which the status assertion is satisfied by a run
///   in which nothing was ever retried.
const SYSTEM_AREA_UNIQUE: &str = "areas.kind";

/// The `constraint` argument for the launchpad-track race.
///
/// **Read this before trusting it, because its guard is weaker than the one
/// above and the difference is not obvious.** The retry arm it feeds is
/// unreachable (see `ensure_today_launchpad`), so there is no behavioural case
/// that can drive it — nothing observes the call site at run time.
///
/// * Reverting *this constant* to an index name: caught by
///   `tests::sqlite_names_the_columns_not_the_indexes_for_both_partial_unique_violations`.
/// * Reverting the *call site* to an inline literal, leaving this constant
///   correct: **the test stays green.** The only thing that fails is
///   `cargo clippy --lib -- -D warnings`, on `dead_code`, because nothing
///   outside `mod tests` would read the constant any more. Verified by running
///   both.
///
/// So the carrier for the call site is **clippy's `dead_code` on the non-test
/// target**, not a test. That is thin on purpose-built terms: one
/// `#[allow(dead_code)]`, or one second non-test reader of this constant, and
/// the guard goes silent with nothing turning red. Do not add either without
/// replacing the guard.
const LAUNCHPAD_UNIQUE: &str = "tracks.purpose";

/// Per-server observation of the system-area mint race.
///
/// **Why it exists.** Without it the concurrency case asserted only its
/// *outcome* — both requests succeeded, one area, one launchpad — and every one
/// of those assertions is equally true when the race never happens, because
/// then only one request mints and the retry arm is never needed. `attempts`
/// lets the case assert the race *occurred*, which is what makes the outcome
/// assertions mean anything.
///
/// **Why it is not `#[cfg(feature = "fixtures")]`.** A case about scheduling
/// has to execute the instructions production executes; a counter compiled only
/// into the test build makes the tested binary a different binary from the
/// shipped one, which is exactly what a timing test cannot afford. The cost is
/// two relaxed atomic adds on a path taken at most once per server.
///
/// **Why it hangs off [`AppState`] and not a `static`.** A process-global is
/// shared by every `AppState` in the process, and ~30 sibling cases in
/// `tests/cases/today_launchpad.rs` drive the same `ensure` helper. Under
/// nextest (process per test) that is invisible; under a plain
/// `cargo test --test domain_api_suite` they run as threads in one process and
/// the counters read other cases' requests. Reproduced with a process-global
/// here, the failure is a confident false RED reading "the race did not happen:
/// 19 of the 2 requests found no system area". Per-instance scoping removes the
/// sharing rather than documenting it.
///
/// [`AppState`]: crate::state::AppState
/// A rendezvous the system-area mint race can be *created* at, not merely
/// observed.
///
/// `None` in production — the mint path costs one `Option` check and never
/// waits. A test arms it with a `Barrier::new(2)`; both requests then park
/// here after their `area_get_system()` has returned `None` and before either
/// opens its write transaction, so the second request provably cannot read the
/// first one's committed row.
///
/// **Why this exists at all.** `attempts == 2` observes whether the race
/// happened; it cannot make it happen. `tokio::join!` does not order the two
/// requests, so on a scheduler where A finishes the whole mint before B reads,
/// the assertion correctly reports "the race did not happen" — and a case that
/// is red for that reason is worse than the vacuous one it replaced. Our box
/// measured 20/20 and 4/4 green and was simply not an environment that could
/// falsify it; a CI runner was. The counters stay: they are what proves the
/// rendezvous actually did its job.
///
/// **Why an `Option` on [`AppState`] rather than `#[cfg(feature = "fixtures")]`.**
/// `routes::tracks`'s `wait_at_chat_track_ensure_barrier` is the existing
/// precedent for this shape and is cfg-gated behind a process-global registry.
/// That carrier was already ruled against for the counters, for two reasons
/// that apply here unchanged: a cfg-gated path means the tested binary does not
/// execute the instructions the shipped one does, which is exactly what a
/// timing test cannot afford; and a process-global is shared by every
/// `AppState` in the process, which a threaded `cargo test` turns into
/// cross-case interference. Same shape as the precedent, per-instance carrier.
///
/// [`AppState`]: crate::state::AppState
pub type SystemAreaMintRendezvous = Option<std::sync::Arc<tokio::sync::Barrier>>;

#[derive(Debug, Default)]
pub struct SystemAreaMintCounters {
    /// Requests that found no system area and therefore tried to mint one.
    /// Two of these means both requests read `None` before either wrote — i.e.
    /// the race actually happened.
    pub attempts: AtomicU64,
    /// Mints that lost the race and took the retry arm.
    pub retries: AtomicU64,
}

fn is_unique_constraint(error: &CalmError, constraint: &str) -> bool {
    let CalmError::Db(sqlx::Error::Database(error)) = error else {
        return false;
    };
    error.is_unique_violation() && error.message().contains(constraint)
}

/// #1253 §5.1 — the read-only resolve the Today page load uses.
///
/// **This handler must never reach the harness.** `ensure_today_launchpad`
/// materializes a workspace and then submits `planner-harness-start` and
/// `.wait()`s on it; putting that on the page-load path would make the whole
/// Today route fail hard whenever codex is unavailable, which is strictly
/// worse than the Today page this replaces (it needed nothing to render). So
/// this endpoint reads two rows and returns. It does not call `ensure`, does
/// not materialize a workspace, and submits no operation — `ensure` hangs off
/// an explicit user action only (INV-TODAYDOC-001).
///
/// **Routine absence is data; anomalous absence is an error.** That is the
/// whole rule, and the two branches here are the two sides of it.
///
/// *No launchpad track* is the ordinary state of a fresh workspace, so it is
/// `200` with a `null` body — not a 404. It was a 404 for one revision, on the
/// grounds that 404 is "cheap and fail-closed". That reasoning did not survive
/// contact with the fact that this is the **landing route**: every session on
/// a fresh workspace hit it, and the browser reports every 404 on its console
/// error stream, so an expected state was being transported as an error. CI
/// found it — two Playwright specs assert zero console errors and both load
/// Today; one CI run logged the 404 thirty times, because the query refetches.
/// The alternative was allowlisting a 404 in those specs, which buys a
/// permanent hole in a "no console errors" gate for a transient condition:
/// once #1253 PR2's trigger lands, first use mints a launchpad and the
/// exemption outlives its reason.
///
/// *A launchpad track with no `track-report` card* stays a `404`, and that is
/// the same rule rather than an exception to it. The track and its report card
/// are created in **one transaction** (`today_launchpad_ensure_tx`), and the
/// adopt-legacy branch has not yet written `purpose = 'launchpad'` when it
/// commits, so a `purpose`-keyed read cannot observe a half-built launchpad.
/// The state is unreachable, so it produces no console noise in practice — and
/// if it ever does occur, an error is the correct signal.
///
/// Deliberately NOT reusing `GET /api/cards/{id}/terminal`'s 404-for-absence
/// idiom. That 404 is **control flow**: its consumer bootstraps on it, so the
/// status means "go create one" (INV-TODAYTERM-006 pins that chain). This one
/// would mean "render the empty state" — pure data, no action — so borrowing
/// the shape would discard the meaning.
#[utoipa::path(get, path = "/api/today/launchpad", tag = "tracks", responses(
    (status = 200, description = "The launchpad track and whether its report has been written, or `null` when no launchpad track exists yet — the ordinary state of a fresh workspace, which the page renders as an empty state.", body = Option<TodayLaunchpadResolved>),
    (status = 404, description = "The launchpad track exists but carries no `track-report` card. Not a reachable state; see the handler docs.", body = ErrorBody)
))]
pub(crate) async fn resolve_today_launchpad(
    State(app): State<AppState>,
    _actor: Actor,
) -> Result<Json<Option<TodayLaunchpadResolved>>> {
    let Some(track) = app.repo.track_get_launchpad().await? else {
        return Ok(Json(None));
    };
    let report = app
        .repo
        .cards_by_track(track.id.as_str())
        .await?
        .into_iter()
        .find(|card| card.kind == "track-report")
        .ok_or_else(|| CalmError::NotFound("today launchpad report card".into()))?;
    // A payload this build cannot parse is, by construction, not the canonical
    // initial payload, so `true` ("someone wrote something here") is the honest
    // answer and it shows the document rather than hiding it behind an empty
    // state. The alternative — treating an unreadable payload as empty — would
    // let one bad row silently swallow a real report.
    let has_noninitial_content = serde_json::from_value::<TrackReportPayload>(report.payload)
        .map(|payload| payload.report_startup_read_required())
        .unwrap_or(true);
    Ok(Json(Some(TodayLaunchpadResolved {
        track_id: track.id.to_string(),
        report_has_noninitial_content: has_noninitial_content,
    })))
}

/// Is this track Today's launchpad? (#1343)
///
/// **One criterion, and every caller uses this one.** Two places now behave
/// differently on the launchpad — the activity briefing a new conversation
/// opens with (`routes::track_conversations`) and the identity that
/// conversation's agent is started under
/// (`operation::planner_harness_start_adapter`) — and a second spelling of
/// "is this the launchpad?" would let them disagree about the same track.
///
/// Identity against `track_get_launchpad`, not a re-derivation from `purpose`
/// or from the system area: the launchpad is a single row the repository
/// already knows how to find, and the partial unique index on
/// `purpose = 'launchpad'` is what makes that row unique. Matching on the
/// column here would be a second implementation of the repository's own query.
///
/// `false` when there is no launchpad yet, which is the ordinary state of a
/// fresh workspace. Nothing is ensured from here.
pub(crate) async fn is_launchpad_track(
    repo: &(impl crate::db::ServerRepoReadExt + ?Sized),
    track_id: &str,
) -> Result<bool> {
    Ok(repo
        .track_get_launchpad()
        .await?
        .is_some_and(|launchpad| launchpad.id.as_str() == track_id))
}

/// What a reset answers with.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct TodayLaunchpadReportReset {
    /// The launchpad track whose report was restored.
    pub track_id: String,
    /// The predicate `GET /api/today/launchpad` will now report. Always
    /// `false` on success — it is returned rather than assumed so a caller can
    /// see the reset land without a second round trip.
    pub report_has_noninitial_content: bool,
}

/// `POST /api/today/launchpad/report/reset` — put today's report back to the
/// canonical empty document (#1343).
///
/// **Why this is a server action and not a client-supplied write.** The
/// existing route `POST /api/tracks/{id}/report` can express a reset: send the
/// canonical `summary` and `body` and `report_startup_read_required` flips back
/// to false. But that predicate is a **byte-for-byte** comparison against
/// [`TrackReportPayload::initial`], whose body is two `include_str!`-ed
/// markdown files plus a closing `-->` and four empty H1s — around 2.6 kB that
/// no client can reproduce without copying kernel-owned text. One byte out and
/// the predicate stays `true`, so the reset fails *silently*: a 200, an edited
/// report, and an empty state that never appears. Worse, the two contract
/// fragments are private and **unclosed** on purpose (`track_report.rs`), so a
/// client reassembling them wrongly ships an unterminated HTML comment that
/// swallows the whole document with no diagnostic.
///
/// So the kernel calls `TrackReportPayload::initial()` itself. Nothing about
/// the canonical content crosses the wire in either direction.
///
/// **It touches the report and nothing else.** No conversation is created,
/// none is reset, no harness is started or stopped, and the launchpad is read
/// rather than ensured — a workspace with no launchpad has no report to reset
/// and gets a 404.
///
/// **Attribution is `EditAuthor::User`**, because `rest_user_replace` is the
/// entry used and its signature admits nothing else. That is the right record:
/// a person pressed a button, and the resulting `track.report_edited` is a
/// human edit. It is also why the same `X-Calm-Actor: user` gate the wholesale
/// replace uses is applied here — the two write the same thing through the
/// same door.
///
/// **The revision anchor is read here, not supplied.** `if_doc_rev` comes from
/// the current snapshot, so this is last-write-wins against a concurrent edit
/// rather than a 409. That is deliberate for a destructive action the user has
/// already confirmed: "reset it" means the report as it stands is being
/// discarded, so racing with an edit that is also being discarded has no
/// outcome worth reporting. It is not a claim that no edit can interleave —
/// one can, between the read and the write, and it would be overwritten.
#[utoipa::path(
    post,
    path = "/api/today/launchpad/report/reset",
    tag = "tracks",
    responses(
        (status = 200, description = "Today's report is back to the canonical empty document. Conversations are untouched.", body = TodayLaunchpadReportReset),
        (status = 401, description = "Missing or invalid session", body = ErrorBody),
        (status = 403, description = "Non-user actor (worker / plugin / planner) rejected, exactly as on `POST /api/tracks/{id}/report`", body = ErrorBody),
        (status = 404, description = "There is no launchpad track yet, so there is no report to reset", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn reset_today_launchpad_report(
    State(s): State<RouteState>,
    // Extraction asserts the session middleware ran; a missing cookie is a 401
    // long before this handler. Nothing is read off it — same single-owner
    // model as `update_track_report`.
    _principal: crate::auth::Principal,
    actor: Actor,
) -> Result<Json<TodayLaunchpadReportReset>> {
    // The same raw-string gate the wholesale replace uses, and for the same
    // reason: `Actor::to_actor_id`'s defensive fallback maps unknown `ai:*`
    // values to `User`, which is right for attribution and wrong for gating.
    crate::routes::track_report_blocks::require_rest_user_actor(&actor)?;

    let track = s
        .repo
        .track_get_launchpad()
        .await?
        .ok_or_else(|| CalmError::NotFound("today launchpad".into()))?;
    let track_id = track.id.to_string();
    let (_, report_card, _) =
        crate::track_report::resolve_report_for_track(s.repo.as_ref(), &track_id).await?;
    let snapshot = crate::track_report_read::load_report_read_snapshot(
        s.repo.as_ref(),
        report_card.id.as_str(),
    )
    .await?;
    let target = crate::track_report::ReportEditTarget::resolve(s.repo.as_ref(), &track_id).await?;
    crate::track_report::write::rest_user_replace(
        s.repo.as_ref(),
        &s.events,
        &s.write,
        target,
        // The kernel's own canonical document. Calling it is the point of this
        // endpoint; a literal here would be the same mirror-code hazard one
        // layer down.
        TrackReportPayload::initial(),
        snapshot.doc_rev,
    )
    .await?;
    Ok(Json(TodayLaunchpadReportReset {
        track_id,
        report_has_noninitial_content: false,
    }))
}

/// #1147 — the launchpad track's workspace. `Managed`, under the workspace
/// root like every other managed workspace, and **never frozen**.
///
/// **Never frozen** is the design D9 exception, and it is the *only* thing
/// that is exceptional here. The launchpad is the one track whose path the
/// kernel may legally re-point: the adopt-legacy branch below repurposes an
/// existing `Today` track, and `ensure` is idempotent, so that branch runs
/// against a row that has already been through here. Freezing it would make
/// design D1's "`frozen_at` is one-shot and monotonic" false — re-point +
/// re-stamp is exactly the sequence the latch forbids. The alternatives were
/// worse: refusing to re-point breaks `ensure`, and re-pointing while leaving
/// a stale stamp makes the stamp lie about which path was frozen.
///
/// **`Managed`, not `Attached` (S2 review ruling).** S1 wrote `Attached` here
/// because managed roots did not exist yet, and S2's first cut kept it,
/// materializing the kernel-minted `<data_dir>/../launchpad` directory in
/// place. That made the row's label disagree with the fact on disk:
/// `Attached` means "a repository the *user* pointed at, never created or
/// `git init`-ed by the server", and this directory is created by the server.
/// A label that disagrees with the fact is the class of defect the previous
/// two slices spent three review rounds removing.
///
/// Making it managed also buys an invariant with **no exceptions**:
/// `kind = Managed ⇒ path is under <workspace-root>`.
/// `every_managed_track_lives_under_the_workspace_root` in
/// `tests/cases/today_launchpad.rs` asserts it over the whole table, so S5's
/// recycle-path prefix assertion needs no launchpad carve-out.
///
/// The old `<data_dir>/../launchpad` directory is deliberately **left on
/// disk**: nothing outside the workspace root is ours to delete.
fn launchpad_workspace(workspace_root: &Path, area_id: &str, track_id: &str) -> TrackWorkspace {
    TrackWorkspace {
        kind: TrackWorkspaceKind::Managed,
        path: crate::workspace_materialize::managed_workspace_path(
            workspace_root,
            area_id,
            track_id,
        )
        .to_string_lossy()
        .into_owned(),
        // Never `Some(..)`. See the doc comment: writing a stamp here is what
        // would break monotonicity on re-adoption.
        frozen_at: None,
    }
}

fn planner_payload() -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": CODEX_PAYLOAD_SCHEMA_VERSION,
        "harness": { "snapshotVersion": 0, "pendingQueue": [] }
    })
}

#[allow(deprecated)]
async fn today_launchpad_ensure_tx(
    tx: &mut Transaction<'_, Sqlite>,
    s: &RouteState,
    area_id: &str,
    workspace_root: &Path,
) -> Result<EnsureTxResult> {
    let existing = sqlx::query_as::<_, crate::db::rows::TrackRow>(&format!(
        "SELECT {TRACK_SELECT_COLUMNS} FROM tracks WHERE purpose='launchpad' LIMIT 1"
    ))
    .fetch_optional(&mut **tx)
    .await?
    .map(Track::from);

    let (mut track, created, adopted_legacy) = if let Some(track) = existing {
        (track, false, false)
    } else if let Some(mut track) = sqlx::query_as::<_, crate::db::rows::TrackRow>(&format!(
        "SELECT {TRACK_SELECT_COLUMNS} FROM tracks WHERE area_id=?1 AND purpose IS NULL AND title='Today' ORDER BY created_at,id LIMIT 1"
    )).bind(area_id).fetch_optional(&mut **tx).await?.map(Track::from) {
        // #1147 S1 — this UPDATE used to carry `cwd=?2`, which made it a
        // second writer of a column that design D1 demotes to a projection of
        // `workspace.path`. It now writes everything *except* the workspace
        // and hands the workspace to the single writer below, in the same tx.
        sqlx::query("UPDATE tracks SET purpose='launchpad', template_id=NULL, plugin_scope=NULL, template_input=NULL, updated_at=?2 WHERE id=?1")
            .bind(track.id.as_str()).bind(now_ms()).execute(&mut **tx).await?;
        track.purpose = Some("launchpad".into());
        track.template_id = None; track.plugin_scope = None; track.template_input = None;
        (track, false, true)
    } else {
        let id = new_id(); let now = now_ms();
        let sort: f64 = sqlx::query_scalar("SELECT CAST(COALESCE(MAX(sort),-1)+1 AS REAL) FROM tracks WHERE area_id=?1")
            .bind(area_id).fetch_one(&mut **tx).await?;
        // #1147 S1 — `cwd` is off this INSERT's column list (it falls to
        // migration 0018's `DEFAULT ''` for the remainder of this tx) and is
        // written together with the workspace columns by the single workspace
        // writer below, shared by all three branches.
        sqlx::query("INSERT INTO tracks(id,area_id,title,sort,lifecycle,template_id,purpose,template_input,created_at,updated_at) VALUES(?1,?2,'Today',?3,'draft',NULL,'launchpad',NULL,?4,?4)")
            .bind(&id).bind(area_id).bind(sort).bind(now).execute(&mut **tx).await?;
        s.write.area_cache().insert(TrackId::from(id.clone()), area_id.to_string().into());
        (Track { id:id.into(), area_id:area_id.to_string().into(), title:"Today".into(), sort,
            archived_at:None, pinned_at:None, lifecycle:Default::default(), cwd_wire_alias:String::new(),
            template_id:None, plugin_scope:None, purpose:Some("launchpad".into()), template_input:None,
            terminal_at:None, recipe_id:None, recipe_revision:None, workspace: TrackWorkspace::default(), created_at:now, updated_at:now }, true, false)
    };

    // #1147 — ONE workspace writer for all three branches, so the launchpad's
    // row cannot differ by which branch minted it. The desired workspace is a
    // pure function of the track id, so this is a no-op on the steady state and
    // a one-time re-point for a row created before S2 (whose path was the
    // kernel-minted `<data_dir>/../launchpad`). Re-pointing is legal precisely
    // because this track is never frozen — see `launchpad_workspace`.
    let desired = launchpad_workspace(workspace_root, area_id, track.id.as_str());
    if track.workspace != desired {
        track_workspace_write_tx(tx, track.id.as_str(), &desired).await?;
        track.cwd_wire_alias = desired.path.clone();
        track.workspace = desired;
    }
    let cwd = track.workspace.path.clone();
    let cwd = cwd.as_str();

    let cards: Vec<Card> = sqlx::query_as::<_, crate::db::rows::CardRow>(
        "SELECT id,track_id,kind,title,sort,payload,deletable,created_at,updated_at FROM cards WHERE track_id=?1 ORDER BY created_at,id"
    ).bind(track.id.as_str()).fetch_all(&mut **tx).await?.into_iter().map(Card::from).collect();
    let planner = if let Some(card) = cards
        .iter()
        .find(|c| c.kind == "codex" && s.write.role_cache().get(&c.id) == Some(CardRole::Planner))
        .cloned()
    {
        if adopted_legacy {
            // Only repurposing a legacy Today track invalidates its old planner thread.
            sqlx::query("DELETE FROM harness_items WHERE card_id=?1")
                .bind(card.id.as_str())
                .execute(&mut **tx)
                .await?;
            card_update_tx(
                tx,
                card.id.as_str(),
                CardPatch {
                    payload: Some(planner_payload()),
                    ..Default::default()
                },
            )
            .await?
        } else {
            card
        }
    } else {
        card_create_with_id_tx(
            tx,
            new_id(),
            NewCard {
                title: None,
                track_id: track.id.clone(),
                kind: "codex".into(),
                sort: None,
                payload: planner_payload(),
            },
            CardRole::Planner,
            false,
            s.write.role_cache(),
        )
        .await?
    };
    let report = if let Some(card) = cards.iter().find(|c| c.kind == "track-report").cloned() {
        card
    } else {
        card_create_with_id_tx(
            tx,
            new_id(),
            NewCard {
                title: None,
                track_id: track.id.clone(),
                kind: "track-report".into(),
                sort: Some(-1.0),
                payload: serde_json::to_value(TrackReportPayload::initial())?,
            },
            CardRole::ReportCard,
            false,
            s.write.role_cache(),
        )
        .await?
    };
    let valid_terminal_card = sqlx::query_as::<_, crate::db::rows::CardRow>(
        "SELECT c.id,c.track_id,c.kind,c.title,c.sort,c.payload,c.deletable,c.created_at,c.updated_at FROM cards c JOIN terminals t ON t.card_id=c.id WHERE c.track_id=?1 AND c.kind='terminal' ORDER BY c.created_at,c.id LIMIT 1"
    ).bind(track.id.as_str()).fetch_optional(&mut **tx).await?.map(Card::from);
    let valid_terminal: Option<(Card, Terminal)> = if let Some(card) = valid_terminal_card {
        let term = crate::db::sqlite::terminal_get_by_card_tx(tx, card.id.as_str()).await?;
        term.map(|term| (card, term))
    } else {
        None
    };
    let (terminal_card, terminal) = if let Some(pair) = valid_terminal {
        pair
    } else {
        card_with_terminal_create_tx(
            tx,
            new_id(),
            &new_id(),
            None,
            track.id.clone(),
            None,
            None,
            String::new(),
            cwd.into(),
            serde_json::json!({}),
            CardRole::Worker,
            false,
            s.write.role_cache(),
            RequestTheme::default_dark(),
        )
        .await?
    };
    // #1147 N3 — "does the planner harness need re-anchoring?" must be derived
    // from DURABLE state, not from the in-memory comparison above.
    //
    // That comparison is true for exactly one `ensure`: the one whose
    // transaction moves the path. Materialization runs after that transaction
    // commits, so if it fails (500), or the process dies before the
    // `planner-harness-start` operation is recorded, the intent is gone. The next
    // `ensure` sees `stored == desired`, concludes "steady state", and starts
    // the harness with `force_new_thread: false` — leaving the planner agent's
    // codex thread pinned to the OLD cwd forever while every worker uses the
    // new one. Reproduced end to end by the reviewer.
    //
    // The durable question is instead: *has a harness start ever succeeded at
    // THIS path?* The path digest is already in the idempotency key, so the
    // `operations` table answers it directly, and the answer survives every
    // crash window because it is only written once the start actually
    // succeeded.
    let started_at_this_path: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM operations \
         WHERE kind='planner-harness-start' AND phase='succeeded' AND idempotency_key LIKE ?1)",
    )
    .bind(format!(
        "today-launchpad:{}:%:{}",
        planner.id.as_str(),
        workspace_key_digest(&track.workspace.path)
    ))
    .fetch_one(&mut **tx)
    .await?;
    let repointed = !started_at_this_path;

    Ok(EnsureTxResult {
        dto: TodayLaunchpad {
            track_id: track.id.to_string(),
            planner_card_id: planner.id.to_string(),
            terminal_card_id: terminal_card.id.to_string(),
            terminal_id: terminal.id,
        },
        track,
        report_card_id: report.id.to_string(),
        created,
        adopted_legacy,
        repointed,
    })
}

#[utoipa::path(post,path="/api/today/launchpad/ensure",tag="tracks",responses(
    (status=200,description="Existing live launchpad",body=TodayLaunchpad),
    (status=201,description="Launchpad minted or adopted; harness start may still be dormant",body=TodayLaunchpad),
    (status=503,description="Launchpad exists but harness failed to start",body=ErrorBody)
))]
pub(crate) async fn ensure_today_launchpad(
    State(app): State<AppState>,
    _actor: Actor,
) -> Result<(StatusCode, Json<TodayLaunchpad>)> {
    let area = if let Some(c) = app.repo.area_get_system().await? {
        c
    } else {
        // The read said "no system area". Counted before the mint so that a
        // test can tell "both requests raced" from "the second one simply read
        // the first one's row".
        app.system_area_mint
            .attempts
            .fetch_add(1, Ordering::Relaxed);
        // Armed only by the concurrency case; `None` everywhere else, so this
        // is one `Option` check on the production path. See
        // [`SystemAreaMintRendezvous`] for why the race has to be created here
        // rather than hoped for.
        if let Some(barrier) = &app.system_area_mint_rendezvous {
            barrier.wait().await;
        }
        let route = RouteState::from_ref(&app);
        let minted = write_with_event_typed(
            app.repo.as_ref(),
            ActorId::Kernel,
            EventScope::System,
            None,
            &app.events,
            &route.write,
            |tx| {
                Box::pin(async move {
                    let c = area_create_system_tx(tx).await?;
                    Ok((c.clone(), Event::AreaUpdated(c)))
                })
            },
        )
        .await;
        match minted {
            Ok((c, _)) => c,
            // The COLUMN form, not `idx_areas_one_system`: see
            // `is_unique_constraint`. Until #1253 PR1 this arm never matched,
            // so the loser of the first-concurrent-mint race got a 500.
            //
            // Unlike the launchpad arm below, this one is genuinely reachable:
            // `area_get_system()` runs OUTSIDE any transaction, so two
            // concurrent entries into this handler can both read `None` and
            // both reach the mint. NOT two page loads — a page load calls only
            // the read-only resolve and never gets here (INV-TODAYDOC-001).
            // What *does* reach here in production is either a deliberate
            // `POST /api/today/launchpad/ensure`, or `POST /api/today/summary`,
            // which calls this handler directly
            // (`routes::today_summary::write_today_summary`, the
            // `ensure_today_launchpad(State(app.clone()), synthetic_actor())`
            // call). So the race needs two concurrent such actions, not two
            // page loads.
            // `today_launchpad::concurrent_first_ensure_retries_the_system_area_race`
            // drives exactly that.
            Err(e) if is_unique_constraint(&e, SYSTEM_AREA_UNIQUE) => {
                app.system_area_mint.retries.fetch_add(1, Ordering::Relaxed);
                app.repo
                    .area_get_system()
                    .await?
                    .ok_or_else(|| CalmError::Internal("system area race had no winner".into()))?
            }
            Err(e) => return Err(e),
        }
    };
    // #1147 — the launchpad's workspace is a managed one under the workspace
    // root, derived from the track id inside the transaction. The pre-S2
    // `<data_dir>/../launchpad` directory is no longer created here, and an
    // existing one is deliberately left on disk: nothing outside the workspace
    // root is ours to remove.
    let workspace_root = app.workspace_root().to_path_buf();
    let route = RouteState::from_ref(&app);
    let area_id = area.id.to_string();
    let root_for_tx = workspace_root.clone();
    let attempt = write_in_tx_typed(app.repo.as_ref(), move |tx| {
        Box::pin(async move { today_launchpad_ensure_tx(tx, &route, &area_id, &root_for_tx).await })
    })
    .await;
    let out = match attempt {
        Ok(v) => v,
        // The COLUMN form, not `idx_tracks_one_launchpad`: see
        // `is_unique_constraint`.
        //
        // —— This arm is UNREACHABLE today, and that is worth stating plainly.
        //
        // 1. Why. `write_in_tx` opens the transaction with **BEGIN IMMEDIATE**
        //    (`calm-truth`'s `events.rs`), which takes the writer lock at
        //    transaction start. `today_launchpad_ensure_tx`'s
        //    `SELECT ... WHERE purpose='launchpad'` and the `INSERT`/`UPDATE`
        //    that follows it therefore sit inside ONE writer-lock hold: no
        //    other writer can commit between them, so the SELECT cannot miss a
        //    row that the INSERT then collides with. A concurrency probe
        //    confirmed it — 320 concurrent `ensure`s against a pre-seeded
        //    system area never entered this arm, while forcing the SELECT to
        //    miss did enter it. Contrast the `areas.kind` arm above, which IS
        //    reachable precisely because its `area_get_system()` read happens
        //    OUTSIDE any transaction.
        // 2. Why keep it. It is fail-safe against two ordinary refactors:
        //    moving that SELECT out of the transaction (or to a deferred one),
        //    and a **second writer of `purpose='launchpad'` appearing**. As of
        //    #1253, `routes/today.rs` is the sole writer of that value in the
        //    whole repository — the two statements in
        //    `today_launchpad_ensure_tx` — and that is exactly the fact a
        //    future change would silently invalidate.
        // 3. What is and is not covered. The *string* is pinned, by
        //    `tests::sqlite_names_the_columns_not_the_indexes_for_both_partial_unique_violations`,
        //    which provokes a real violation of this index on a real database.
        //    The *reachability* is not pinned by anything, and the
        //    system-area concurrency case
        //    (`today_launchpad::concurrent_first_ensure_retries_the_system_area_race`)
        //    does NOT cover this arm — it exercises the `areas.kind` one. This
        //    is a known, named gap; do not close it by asserting that test
        //    covers both, and do not add a fixtures-gated seam whose only
        //    purpose is to make an unreachable state reachable.
        Err(e) if is_unique_constraint(&e, LAUNCHPAD_UNIQUE) => {
            // A concurrent inserter won the partial unique index; retry selects it.
            let route = RouteState::from_ref(&app);
            let area_id = area.id.to_string();
            let root_for_tx = workspace_root.clone();
            write_in_tx_typed(app.repo.as_ref(), move |tx| {
                Box::pin(async move {
                    today_launchpad_ensure_tx(tx, &route, &area_id, &root_for_tx).await
                })
            })
            .await?
        }
        Err(e) => return Err(e),
    };
    // #1147 S2 (design D3) — the launchpad is one of the four track-create
    // entry points (`POST /api/tracks`, area chat, launchpad, child track;
    // template seeding was a fifth until #1300 S2 deleted it). The enumeration
    // is spelled out once, in `tests/cases/track_workspace_materialize.rs`,
    // because an ordinal repeated across files is what drifted: this comment
    // and `child_track_adapter.rs` both used to call themselves "the fifth".
    // It does **not** go through `create_track_structure` (raw
    // `INSERT INTO tracks`), so it carries its own materialize call. Skipping it
    // would leave every codex task on the Today panel dying with
    // `spawn-failed` (`git rev-parse --show-toplevel` on a non-repository),
    // which is the exact defect #1147 opened on.
    crate::workspace_materialize::materialize_workspace(
        &out.track.workspace,
        &workspace_root,
        out.track.id.as_str(),
    )
    .map_err(|error| {
        tracing::error!(
            track_id = %out.dto.track_id,
            path = %out.track.workspace.path,
            error = %error,
            "today launchpad: workspace materialization failed"
        );
        error
    })?;

    let req = PlannerHarnessStartOperationPayload {
        actor: ActorId::Kernel,
        track_id: out.dto.track_id.clone(),
        planner_card_id: CardId::from(out.dto.planner_card_id.clone()),
        report_card_id: Some(out.report_card_id),
        sort: None,
        cwd: out.track.workspace.path.clone(),
        goal: None,
        reset_harness_items: out.created || out.adopted_legacy,
        // #1147 — a re-point also forces a new thread. The codex thread holds
        // the cwd it was minted with, so resuming it after the workspace moved
        // would leave the planner agent working in the old directory while every
        // worker uses the new one. The transcript is NOT reset: harness items
        // are persisted per card, not per thread (`db/sqlite/read.rs`,
        // `WHERE card_id = ?1`), so re-opening the thread costs the agent its
        // in-thread context, not the user's history.
        force_new_thread: out.created || out.adopted_legacy || out.repointed,
        profile: Default::default(),
        create_card: None,
        first_message_sha256: None,
        first_message: None,
        create_request_sha256: None,
    };
    let start_mode = if out.created || out.adopted_legacy {
        "bootstrap"
    } else if out.repointed {
        // A distinct mode so the re-point's operation is not collapsed onto a
        // previously succeeded `reuse` by the idempotency key.
        "repoint"
    } else {
        "reuse"
    };
    let hash = stable_payload_hash(&serde_json::json!({"actor":"kernel","request":&req}))?;
    let op = app
        .operation_runtime
        .submit(
            "planner-harness-start",
            OperationKey {
                operation_key: new_id(),
                // #1147 S2 (red-team B1) — the workspace path is part of the
                // key, not just of the payload.
                //
                // The operation runtime refuses an idempotency key that was
                // already used with a *different* payload hash, and the
                // payload carries `cwd`. A pre-S2 database already holds
                // `today-launchpad:<card>:reuse` rows hashed against the old
                // `<data_dir>/../launchpad` path; after the upgrade re-points
                // the workspace, every subsequent `ensure` would submit that
                // same key with a new cwd and be rejected — 409, on every
                // request, forever, because nothing ever deletes rows from
                // `operations`. The Today panel would be dead from the first
                // request after deploy. The same mechanism fires on any
                // `CALM_WORKSPACE_ROOT` change.
                //
                // Keying on the path makes a re-point mint a *new* key instead
                // of colliding with the old one, and keeps idempotency exactly
                // as strong within one workspace.
                idempotency_key: Some(format!(
                    "today-launchpad:{}:{start_mode}:{}",
                    out.dto.planner_card_id,
                    workspace_key_digest(&out.track.workspace.path)
                )),
                payload_hash: hash,
            },
            serde_json::to_value(req)?,
        )
        .await?;
    let result = app.operation_runtime.wait(&op).await?;
    match result.outcome {
        OperationOutcome::Succeeded { .. } | OperationOutcome::SucceededViaCollision { .. } => {
            Ok((
                if out.created || out.adopted_legacy {
                    StatusCode::CREATED
                } else {
                    StatusCode::OK
                },
                Json(out.dto),
            ))
        }
        _ => Err(CalmError::Internal(format!(
            "launchpad exists but harness start failed: {op}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::SqlxRepo;

    /// #1253 PR1 — how SQLite words a violation of the two partial unique
    /// indexes this module races on.
    ///
    /// **This is a claim about SQLite, not about this crate**, which is why it
    /// is worth a test even though one of the two arms it protects is
    /// unreachable (see `ensure_today_launchpad`): the message wording is the
    /// entire content of the fix, it comes from outside this repository, and it
    /// can change under us. It runs the real migrations on a real database and
    /// provokes real violations; `is_unique_constraint` is then called
    /// **directly**, as the module's own private function. No hand-built
    /// `CalmError`, and deliberately not `tracks::is_unique_constraint_for_test`
    /// — a test that reaches for a test-only export is testing the export.
    ///
    /// **What it binds, and what it does not.** It asserts against
    /// [`SYSTEM_AREA_UNIQUE`] and [`LAUNCHPAD_UNIQUE`] rather than string
    /// literals, so it is a test of **those two constants**: reverting either
    /// to an index name turns it red, where the first version of this test
    /// (which restated the literals) stayed green.
    ///
    /// It is **not** a test of the call sites, and claiming it was is itself a
    /// review finding. A call site that passes an inline literal instead of its
    /// constant leaves this test green — measured: that mutation on the
    /// launchpad arm passes here and fails only
    /// `cargo clippy --lib -- -D warnings`, on `dead_code`. What binds each
    /// call site is written on the constant it uses: the concurrency case for
    /// [`SYSTEM_AREA_UNIQUE`], clippy alone for [`LAUNCHPAD_UNIQUE`].
    ///
    /// The negative half is load-bearing too: asserting only that the constants
    /// match would stay green if SQLite ever started naming the index as well,
    /// so both index names are asserted NOT to match.
    #[tokio::test]
    async fn sqlite_names_the_columns_not_the_indexes_for_both_partial_unique_violations() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let pool = repo.pool();

        // —— areas(kind) WHERE kind = 'system' (migration 0009) ——
        let insert_system_area = |id: &'static str| {
            sqlx::query(
                "INSERT INTO areas(id,name,color,sort,kind,created_at,updated_at) \
                 VALUES(?1,'System','#abc',1,'system',1,1)",
            )
            .bind(id)
            .execute(pool)
        };
        insert_system_area("area-winner").await.unwrap();
        let error: CalmError = insert_system_area("area-loser").await.unwrap_err().into();
        let message = error.to_string();
        assert!(
            message.contains("UNIQUE constraint failed: areas.kind"),
            "unexpected message: {message}"
        );
        // `SYSTEM_AREA_UNIQUE`, not a literal, so this assertion follows the
        // constant the retry arm reads: revert the CONSTANT and this goes red,
        // where a literal here would pin only the helper. It does NOT follow
        // the call site — swapping the arm to an inline literal leaves this
        // green (the docstring above says what catches that instead).
        assert!(
            is_unique_constraint(&error, SYSTEM_AREA_UNIQUE),
            "the system-area retry arm's constraint must match a real \
             violation, but `{SYSTEM_AREA_UNIQUE}` does not: {message}"
        );
        assert!(
            !is_unique_constraint(&error, "idx_areas_one_system"),
            "the index name must NOT match — matching it is what made the \
             system-area retry arm dead code before #1253: {message}"
        );

        // —— tracks(purpose) WHERE purpose = 'launchpad' (migration 0064) ——
        let insert_launchpad = |id: &'static str| {
            sqlx::query(
                "INSERT INTO tracks(id,area_id,title,sort,lifecycle,purpose,created_at,updated_at) \
                 VALUES(?1,'area-winner','Today',1,'draft','launchpad',1,1)",
            )
            .bind(id)
            .execute(pool)
        };
        insert_launchpad("track-winner").await.unwrap();
        let error: CalmError = insert_launchpad("track-loser").await.unwrap_err().into();
        let message = error.to_string();
        assert!(
            message.contains("UNIQUE constraint failed: tracks.purpose"),
            "unexpected message: {message}"
        );
        assert!(
            is_unique_constraint(&error, LAUNCHPAD_UNIQUE),
            "the launchpad retry arm's constraint must match a real violation, \
             but `{LAUNCHPAD_UNIQUE}` does not: {message}"
        );
        assert!(
            !is_unique_constraint(&error, "idx_tracks_one_launchpad"),
            "the index name must NOT match — see the note on the launchpad \
             retry arm in `ensure_today_launchpad`: {message}"
        );

        // Both indexes are PARTIAL, and that is the whole reason the index name
        // never appears: sqlite names the index only for a unique index over
        // *expressions*. A non-partial index on a plain column words it the
        // same way, so the fix is about partiality only incidentally — pin the
        // observed behaviour rather than the folklore.
        assert!(
            !message.contains("idx_"),
            "no index name appears anywhere in the message: {message}"
        );
    }
}
