//! Server-owned Today launchpad bootstrap (#951, Slice A).

use crate::actor::Actor;
use crate::db::rows::WAVE_SELECT_COLUMNS;
use crate::db::sqlite::{
    card_create_with_id_tx, card_update_tx, card_with_terminal_create_tx, cove_create_system_tx,
    wave_workspace_write_tx,
};
use crate::db::{write_in_tx_typed, write_with_event_typed};
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, CardId, WaveId};
use crate::model::{
    Card, CardPatch, CardRole, NewCard, RequestTheme, Terminal, Wave, WaveWorkspace,
    WaveWorkspaceKind, new_id, now_ms,
};
use crate::operation::spec_harness_start_adapter::SpecHarnessStartOperationPayload;
use crate::operation::{OperationKey, OperationOutcome};
use crate::routes::terminal_cards::stable_payload_hash;
use crate::state::{AppState, RouteState};
use crate::validation::CODEX_PAYLOAD_SCHEMA_VERSION;
use crate::wave_report::WaveReportPayload;
use axum::{
    Json, Router,
    extract::{FromRef, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::Serialize;
use sqlx::{Sqlite, Transaction};
use std::path::Path;
use utoipa::ToSchema;
// #1147 — one definition of the path digest, shared with the scheduler's
// child-wave bootstrap key.
use crate::workspace_materialize::workspace_key_digest;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/today/launchpad/ensure", post(ensure_today_launchpad))
        .route("/api/today/launchpad", get(resolve_today_launchpad))
}

#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct TodayLaunchpad {
    pub wave_id: String,
    pub spec_card_id: String,
    pub terminal_card_id: String,
    pub terminal_id: String,
}

/// #1253 §5.1 — what the Today **page load** reads.
///
/// A deliberately narrow, read-only DTO. It is not [`TodayLaunchpad`] and it
/// does not grow into it: `ensure`'s shape is the bootstrap's, this one is the
/// reader's, and the two answer different questions.
///
/// There is no `report_card_id` here on purpose. The wave detail already
/// returns the wave's cards and the frontend locates the report by
/// `kind == "wave-report"` (`fe/core/domain/report.ts::readWaveReport`), so
/// such a field would have no consumer.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct TodayLaunchpadResolved {
    pub wave_id: String,
    /// Whether this report's `summary`/`body` differ from the canonical
    /// freshly-minted report — i.e. **has anyone ever written it**.
    ///
    /// It is computed server-side by
    /// [`WaveReportPayload::report_startup_read_required`], the kernel's one
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
    /// `readWaveReport` returns non-null for the canonical initial report
    /// (its body carries the maintenance-contract comment and four H1s), so a
    /// null-check there renders four empty headings instead of an empty state.
    pub report_has_noninitial_content: bool,
}

struct EnsureTxResult {
    dto: TodayLaunchpad,
    wave: Wave,
    report_card_id: String,
    created: bool,
    adopted_legacy: bool,
    /// #1147 — the spec harness has never successfully started at the
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
/// this module races on (`idx_coves_one_system` on `coves(kind)`,
/// `idx_waves_one_launchpad` on `waves(purpose)`) are partial indexes over
/// plain columns, so their messages read `coves.kind` and `waves.purpose` and
/// contain no index name at all. Passing the index name here matches nothing:
/// the arm becomes dead code and the race surfaces as a 500 instead of the
/// retry it was written to perform. `routes::waves` has always used the column
/// form (`waves.cove_id`); this module did not until #1253 PR1.
/// The `constraint` argument for the system-cove race, and the ONLY place that
/// string is written. The retry arm passes this, and the test that provokes a
/// real violation asserts against this — so reverting it to an index name makes
/// that test red instead of silently making the arm dead again.
const SYSTEM_COVE_UNIQUE: &str = "coves.kind";

/// The `constraint` argument for the launchpad-wave race. Same single-sourcing
/// as [`SYSTEM_COVE_UNIQUE`], and it matters more here: this arm is currently
/// unreachable (see `ensure_today_launchpad`), so this constant plus the test
/// that reads it are the *only* thing standing between the fix and a silent
/// return to dead code.
const LAUNCHPAD_UNIQUE: &str = "waves.purpose";

fn is_unique_constraint(error: &CalmError, constraint: &str) -> bool {
    let CalmError::Db(sqlx::Error::Database(error)) = error else {
        return false;
    };
    error.is_unique_violation() && error.message().contains(constraint)
}

/// #1253 §5.1 — the read-only resolve the Today page load uses.
///
/// **This handler must never reach the harness.** `ensure_today_launchpad`
/// materializes a workspace and then submits `spec-harness-start` and
/// `.wait()`s on it; putting that on the page-load path would make the whole
/// Today route fail hard whenever codex is unavailable, which is strictly
/// worse than the Today page this replaces (it needed nothing to render). So
/// this endpoint reads two rows and returns. It does not call `ensure`, does
/// not materialize a workspace, and submits no operation — `ensure` hangs off
/// an explicit user action only (INV-TODAYDOC-001).
///
/// **404 twice, and the second one needs its reason stated correctly.** No
/// launchpad wave is a 404, and the frontend renders that as an empty state
/// rather than an error. A launchpad wave with no `wave-report` card is
/// *also* a 404 — but not because that state is reachable: the wave and its
/// report card are created in **one transaction**
/// (`today_launchpad_ensure_tx`), and the adopt-legacy branch has not yet
/// written `purpose = 'launchpad'` when it commits, so a `purpose`-keyed read
/// cannot observe a half-built launchpad. 404 is chosen because it is cheap
/// and fail-closed, **not** because the intermediate state occurs.
#[utoipa::path(get, path = "/api/today/launchpad", tag = "waves", responses(
    (status = 200, description = "The launchpad wave and whether its report has been written", body = TodayLaunchpadResolved),
    (status = 404, description = "No launchpad wave yet (page renders an empty state), or it carries no report card", body = ErrorBody)
))]
pub(crate) async fn resolve_today_launchpad(
    State(app): State<AppState>,
    _actor: Actor,
) -> Result<Json<TodayLaunchpadResolved>> {
    let wave = app
        .repo
        .wave_get_launchpad()
        .await?
        .ok_or_else(|| CalmError::NotFound("today launchpad".into()))?;
    let report = app
        .repo
        .cards_by_wave(wave.id.as_str())
        .await?
        .into_iter()
        .find(|card| card.kind == "wave-report")
        .ok_or_else(|| CalmError::NotFound("today launchpad report card".into()))?;
    // A payload this build cannot parse is, by construction, not the canonical
    // initial payload, so `true` ("someone wrote something here") is the honest
    // answer and it shows the document rather than hiding it behind an empty
    // state. The alternative — treating an unreadable payload as empty — would
    // let one bad row silently swallow a real report.
    let has_noninitial_content = serde_json::from_value::<WaveReportPayload>(report.payload)
        .map(|payload| payload.report_startup_read_required())
        .unwrap_or(true);
    Ok(Json(TodayLaunchpadResolved {
        wave_id: wave.id.to_string(),
        report_has_noninitial_content: has_noninitial_content,
    }))
}

/// #1147 — the launchpad wave's workspace. `Managed`, under the workspace
/// root like every other managed workspace, and **never frozen**.
///
/// **Never frozen** is the design D9 exception, and it is the *only* thing
/// that is exceptional here. The launchpad is the one wave whose path the
/// kernel may legally re-point: the adopt-legacy branch below repurposes an
/// existing `Today` wave, and `ensure` is idempotent, so that branch runs
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
/// `every_managed_wave_lives_under_the_workspace_root` in
/// `tests/cases/today_launchpad.rs` asserts it over the whole table, so S5's
/// recycle-path prefix assertion needs no launchpad carve-out.
///
/// The old `<data_dir>/../launchpad` directory is deliberately **left on
/// disk**: nothing outside the workspace root is ours to delete.
fn launchpad_workspace(workspace_root: &Path, cove_id: &str, wave_id: &str) -> WaveWorkspace {
    WaveWorkspace {
        kind: WaveWorkspaceKind::Managed,
        path: crate::workspace_materialize::managed_workspace_path(
            workspace_root,
            cove_id,
            wave_id,
        )
        .to_string_lossy()
        .into_owned(),
        // Never `Some(..)`. See the doc comment: writing a stamp here is what
        // would break monotonicity on re-adoption.
        frozen_at: None,
    }
}

fn spec_payload() -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": CODEX_PAYLOAD_SCHEMA_VERSION,
        "harness": { "snapshotVersion": 0, "pendingQueue": [] }
    })
}

#[allow(deprecated)]
async fn today_launchpad_ensure_tx(
    tx: &mut Transaction<'_, Sqlite>,
    s: &RouteState,
    cove_id: &str,
    workspace_root: &Path,
) -> Result<EnsureTxResult> {
    let existing = sqlx::query_as::<_, crate::db::rows::WaveRow>(&format!(
        "SELECT {WAVE_SELECT_COLUMNS} FROM waves WHERE purpose='launchpad' LIMIT 1"
    ))
    .fetch_optional(&mut **tx)
    .await?
    .map(Wave::from);

    let (mut wave, created, adopted_legacy) = if let Some(wave) = existing {
        (wave, false, false)
    } else if let Some(mut wave) = sqlx::query_as::<_, crate::db::rows::WaveRow>(&format!(
        "SELECT {WAVE_SELECT_COLUMNS} FROM waves WHERE cove_id=?1 AND purpose IS NULL AND title='Today' ORDER BY created_at,id LIMIT 1"
    )).bind(cove_id).fetch_optional(&mut **tx).await?.map(Wave::from) {
        // #1147 S1 — this UPDATE used to carry `cwd=?2`, which made it a
        // second writer of a column that design D1 demotes to a projection of
        // `workspace.path`. It now writes everything *except* the workspace
        // and hands the workspace to the single writer below, in the same tx.
        sqlx::query("UPDATE waves SET purpose='launchpad', template_id=NULL, plugin_scope=NULL, template_input=NULL, updated_at=?2 WHERE id=?1")
            .bind(wave.id.as_str()).bind(now_ms()).execute(&mut **tx).await?;
        wave.purpose = Some("launchpad".into());
        wave.template_id = None; wave.plugin_scope = None; wave.template_input = None;
        (wave, false, true)
    } else {
        let id = new_id(); let now = now_ms();
        let sort: f64 = sqlx::query_scalar("SELECT CAST(COALESCE(MAX(sort),-1)+1 AS REAL) FROM waves WHERE cove_id=?1")
            .bind(cove_id).fetch_one(&mut **tx).await?;
        // #1147 S1 — `cwd` is off this INSERT's column list (it falls to
        // migration 0018's `DEFAULT ''` for the remainder of this tx) and is
        // written together with the workspace columns by the single workspace
        // writer below, shared by all three branches.
        sqlx::query("INSERT INTO waves(id,cove_id,title,sort,lifecycle,template_id,purpose,template_input,created_at,updated_at) VALUES(?1,?2,'Today',?3,'draft',NULL,'launchpad',NULL,?4,?4)")
            .bind(&id).bind(cove_id).bind(sort).bind(now).execute(&mut **tx).await?;
        s.write.cove_cache().insert(WaveId::from(id.clone()), cove_id.to_string().into());
        (Wave { id:id.into(), cove_id:cove_id.to_string().into(), title:"Today".into(), sort,
            archived_at:None, pinned_at:None, lifecycle:Default::default(), cwd_wire_alias:String::new(),
            template_id:None, plugin_scope:None, purpose:Some("launchpad".into()), template_input:None,
            terminal_at:None, workspace: WaveWorkspace::default(), created_at:now, updated_at:now }, true, false)
    };

    // #1147 — ONE workspace writer for all three branches, so the launchpad's
    // row cannot differ by which branch minted it. The desired workspace is a
    // pure function of the wave id, so this is a no-op on the steady state and
    // a one-time re-point for a row created before S2 (whose path was the
    // kernel-minted `<data_dir>/../launchpad`). Re-pointing is legal precisely
    // because this wave is never frozen — see `launchpad_workspace`.
    let desired = launchpad_workspace(workspace_root, cove_id, wave.id.as_str());
    if wave.workspace != desired {
        wave_workspace_write_tx(tx, wave.id.as_str(), &desired).await?;
        wave.cwd_wire_alias = desired.path.clone();
        wave.workspace = desired;
    }
    let cwd = wave.workspace.path.clone();
    let cwd = cwd.as_str();

    let cards: Vec<Card> = sqlx::query_as::<_, crate::db::rows::CardRow>(
        "SELECT id,wave_id,kind,title,sort,payload,deletable,created_at,updated_at FROM cards WHERE wave_id=?1 ORDER BY created_at,id"
    ).bind(wave.id.as_str()).fetch_all(&mut **tx).await?.into_iter().map(Card::from).collect();
    let spec = if let Some(card) = cards
        .iter()
        .find(|c| c.kind == "codex" && s.write.role_cache().get(&c.id) == Some(CardRole::Spec))
        .cloned()
    {
        if adopted_legacy {
            // Only repurposing a legacy Today wave invalidates its old spec thread.
            sqlx::query("DELETE FROM harness_items WHERE card_id=?1")
                .bind(card.id.as_str())
                .execute(&mut **tx)
                .await?;
            card_update_tx(
                tx,
                card.id.as_str(),
                CardPatch {
                    payload: Some(spec_payload()),
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
                wave_id: wave.id.clone(),
                kind: "codex".into(),
                sort: None,
                payload: spec_payload(),
            },
            CardRole::Spec,
            false,
            s.write.role_cache(),
        )
        .await?
    };
    let report = if let Some(card) = cards.iter().find(|c| c.kind == "wave-report").cloned() {
        card
    } else {
        card_create_with_id_tx(
            tx,
            new_id(),
            NewCard {
                title: None,
                wave_id: wave.id.clone(),
                kind: "wave-report".into(),
                sort: Some(-1.0),
                payload: serde_json::to_value(WaveReportPayload::initial())?,
            },
            CardRole::ReportCard,
            false,
            s.write.role_cache(),
        )
        .await?
    };
    let valid_terminal_card = sqlx::query_as::<_, crate::db::rows::CardRow>(
        "SELECT c.id,c.wave_id,c.kind,c.title,c.sort,c.payload,c.deletable,c.created_at,c.updated_at FROM cards c JOIN terminals t ON t.card_id=c.id WHERE c.wave_id=?1 AND c.kind='terminal' ORDER BY c.created_at,c.id LIMIT 1"
    ).bind(wave.id.as_str()).fetch_optional(&mut **tx).await?.map(Card::from);
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
            wave.id.clone(),
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
    // #1147 N3 — "does the spec harness need re-anchoring?" must be derived
    // from DURABLE state, not from the in-memory comparison above.
    //
    // That comparison is true for exactly one `ensure`: the one whose
    // transaction moves the path. Materialization runs after that transaction
    // commits, so if it fails (500), or the process dies before the
    // `spec-harness-start` operation is recorded, the intent is gone. The next
    // `ensure` sees `stored == desired`, concludes "steady state", and starts
    // the harness with `force_new_thread: false` — leaving the spec agent's
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
         WHERE kind='spec-harness-start' AND phase='succeeded' AND idempotency_key LIKE ?1)",
    )
    .bind(format!(
        "today-launchpad:{}:%:{}",
        spec.id.as_str(),
        workspace_key_digest(&wave.workspace.path)
    ))
    .fetch_one(&mut **tx)
    .await?;
    let repointed = !started_at_this_path;

    Ok(EnsureTxResult {
        dto: TodayLaunchpad {
            wave_id: wave.id.to_string(),
            spec_card_id: spec.id.to_string(),
            terminal_card_id: terminal_card.id.to_string(),
            terminal_id: terminal.id,
        },
        wave,
        report_card_id: report.id.to_string(),
        created,
        adopted_legacy,
        repointed,
    })
}

#[utoipa::path(post,path="/api/today/launchpad/ensure",tag="waves",responses(
    (status=200,description="Existing live launchpad",body=TodayLaunchpad),
    (status=201,description="Launchpad minted or adopted; harness start may still be dormant",body=TodayLaunchpad),
    (status=503,description="Launchpad exists but harness failed to start",body=ErrorBody)
))]
pub(crate) async fn ensure_today_launchpad(
    State(app): State<AppState>,
    _actor: Actor,
) -> Result<(StatusCode, Json<TodayLaunchpad>)> {
    let cove = if let Some(c) = app.repo.cove_get_system().await? {
        c
    } else {
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
                    let c = cove_create_system_tx(tx).await?;
                    Ok((c.clone(), Event::CoveUpdated(c)))
                })
            },
        )
        .await;
        match minted {
            Ok((c, _)) => c,
            // The COLUMN form, not `idx_coves_one_system`: see
            // `is_unique_constraint`. Until #1253 PR1 this arm never matched,
            // so the loser of the first-concurrent-mint race got a 500.
            //
            // Unlike the launchpad arm below, this one is genuinely reachable:
            // `cove_get_system()` runs OUTSIDE any transaction, so two cold
            // page loads can both read `None` and both reach the mint.
            // `today_launchpad::concurrent_first_ensure_retries_the_system_cove_race`
            // drives that race through the route.
            Err(e) if is_unique_constraint(&e, "coves.kind") => {
                app.repo
                    .cove_get_system()
                    .await?
                    .ok_or_else(|| CalmError::Internal("system cove race had no winner".into()))?
            }
            Err(e) => return Err(e),
        }
    };
    // #1147 — the launchpad's workspace is a managed one under the workspace
    // root, derived from the wave id inside the transaction. The pre-S2
    // `<data_dir>/../launchpad` directory is no longer created here, and an
    // existing one is deliberately left on disk: nothing outside the workspace
    // root is ours to remove.
    let workspace_root = app.workspace_root().to_path_buf();
    let route = RouteState::from_ref(&app);
    let cove_id = cove.id.to_string();
    let root_for_tx = workspace_root.clone();
    let attempt = write_in_tx_typed(app.repo.as_ref(), move |tx| {
        Box::pin(async move { today_launchpad_ensure_tx(tx, &route, &cove_id, &root_for_tx).await })
    })
    .await;
    let out = match attempt {
        Ok(v) => v,
        // The COLUMN form, not `idx_waves_one_launchpad`: see
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
        //    system cove never entered this arm, while forcing the SELECT to
        //    miss did enter it. Contrast the `coves.kind` arm above, which IS
        //    reachable precisely because its `cove_get_system()` read happens
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
        //    system-cove concurrency case
        //    (`today_launchpad::concurrent_first_ensure_retries_the_system_cove_race`)
        //    does NOT cover this arm — it exercises the `coves.kind` one. This
        //    is a known, named gap; do not close it by asserting that test
        //    covers both, and do not add a fixtures-gated seam whose only
        //    purpose is to make an unreachable state reachable.
        Err(e) if is_unique_constraint(&e, LAUNCHPAD_UNIQUE) => {
            // A concurrent inserter won the partial unique index; retry selects it.
            let route = RouteState::from_ref(&app);
            let cove_id = cove.id.to_string();
            let root_for_tx = workspace_root.clone();
            write_in_tx_typed(app.repo.as_ref(), move |tx| {
                Box::pin(async move {
                    today_launchpad_ensure_tx(tx, &route, &cove_id, &root_for_tx).await
                })
            })
            .await?
        }
        Err(e) => return Err(e),
    };
    // #1147 S2 (design D3) — the launchpad is the fifth wave-create entry
    // point and it does **not** go through `create_wave_structure` (raw
    // `INSERT INTO waves`), so it carries its own materialize call. Skipping it
    // would leave every codex task on the Today panel dying with
    // `spawn-failed` (`git rev-parse --show-toplevel` on a non-repository),
    // which is the exact defect #1147 opened on.
    crate::workspace_materialize::materialize_workspace(
        &out.wave.workspace,
        &workspace_root,
        out.wave.id.as_str(),
    )
    .map_err(|error| {
        tracing::error!(
            wave_id = %out.dto.wave_id,
            path = %out.wave.workspace.path,
            error = %error,
            "today launchpad: workspace materialization failed"
        );
        error
    })?;

    let req = SpecHarnessStartOperationPayload {
        actor: ActorId::Kernel,
        wave_id: out.dto.wave_id.clone(),
        spec_card_id: CardId::from(out.dto.spec_card_id.clone()),
        report_card_id: Some(out.report_card_id),
        sort: None,
        cwd: out.wave.workspace.path.clone(),
        goal: None,
        reset_harness_items: out.created || out.adopted_legacy,
        // #1147 — a re-point also forces a new thread. The codex thread holds
        // the cwd it was minted with, so resuming it after the workspace moved
        // would leave the spec agent working in the old directory while every
        // worker uses the new one. The transcript is NOT reset: harness items
        // are persisted per card, not per thread (`db/sqlite/read.rs`,
        // `WHERE card_id = ?1`), so re-opening the thread costs the agent its
        // in-thread context, not the user's history.
        force_new_thread: out.created || out.adopted_legacy || out.repointed,
        profile: Default::default(),
        create_card: None,
        first_message_sha256: None,
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
            "spec-harness-start",
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
                    out.dto.spec_card_id,
                    workspace_key_digest(&out.wave.workspace.path)
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
    /// `CalmError`, and deliberately not `waves::is_unique_constraint_for_test`
    /// — a test that reaches for a test-only export is testing the export.
    ///
    /// **It asserts against [`SYSTEM_COVE_UNIQUE`] and [`LAUNCHPAD_UNIQUE`],
    /// not against string literals**, and that is what makes it a test of the
    /// call sites rather than of the helper. The first version of this test
    /// used literals; reverting the launchpad call site to the index name left
    /// it green, which is the same defect in a new costume — the assertion has
    /// to read the value production passes, not restate the value production
    /// ought to pass.
    ///
    /// The negative half is the other load-bearing half: asserting only that
    /// the constants match would stay green if SQLite ever started naming the
    /// index too, so both index names are asserted NOT to match.
    #[tokio::test]
    async fn sqlite_names_the_columns_not_the_indexes_for_both_partial_unique_violations() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let pool = repo.pool();

        // —— coves(kind) WHERE kind = 'system' (migration 0009) ——
        let insert_system_cove = |id: &'static str| {
            sqlx::query(
                "INSERT INTO coves(id,name,color,sort,kind,created_at,updated_at) \
                 VALUES(?1,'System','#abc',1,'system',1,1)",
            )
            .bind(id)
            .execute(pool)
        };
        insert_system_cove("cove-winner").await.unwrap();
        let error: CalmError = insert_system_cove("cove-loser").await.unwrap_err().into();
        let message = error.to_string();
        assert!(
            message.contains("UNIQUE constraint failed: coves.kind"),
            "unexpected message: {message}"
        );
        // `SYSTEM_COVE_UNIQUE`, not a literal: this is the exact value the
        // retry arm passes, so reverting that arm reverts this assertion's
        // input too. A literal here would pin the helper and leave the call
        // site free to go back to matching nothing.
        assert!(
            is_unique_constraint(&error, SYSTEM_COVE_UNIQUE),
            "the system-cove retry arm's constraint must match a real \
             violation, but `{SYSTEM_COVE_UNIQUE}` does not: {message}"
        );
        assert!(
            !is_unique_constraint(&error, "idx_coves_one_system"),
            "the index name must NOT match — matching it is what made the \
             system-cove retry arm dead code before #1253: {message}"
        );

        // —— waves(purpose) WHERE purpose = 'launchpad' (migration 0064) ——
        let insert_launchpad = |id: &'static str| {
            sqlx::query(
                "INSERT INTO waves(id,cove_id,title,sort,lifecycle,purpose,created_at,updated_at) \
                 VALUES(?1,'cove-winner','Today',1,'draft','launchpad',1,1)",
            )
            .bind(id)
            .execute(pool)
        };
        insert_launchpad("wave-winner").await.unwrap();
        let error: CalmError = insert_launchpad("wave-loser").await.unwrap_err().into();
        let message = error.to_string();
        assert!(
            message.contains("UNIQUE constraint failed: waves.purpose"),
            "unexpected message: {message}"
        );
        assert!(
            is_unique_constraint(&error, LAUNCHPAD_UNIQUE),
            "the launchpad retry arm's constraint must match a real violation, \
             but `{LAUNCHPAD_UNIQUE}` does not: {message}"
        );
        assert!(
            !is_unique_constraint(&error, "idx_waves_one_launchpad"),
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
