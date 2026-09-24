//! Server-owned Today launchpad bootstrap.

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
use crate::workspace_materialize::workspace_key_digest;
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

/// What the Today page load reads: a narrow, read-only DTO, distinct from [`TodayLaunchpad`].
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct TodayLaunchpadResolved {
    pub track_id: String,
    /// Whether this report holds content right now beyond the empty skeleton — a statement
    /// about CURRENT content, not history: restoring the canonical text flips it back to
    /// `false`, and a user hand-edit flips it exactly as a summary agent would. The frontend
    /// must not re-derive this from the report body.
    pub report_has_noninitial_content: bool,
}

struct EnsureTxResult {
    dto: TodayLaunchpad,
    track: Track,
    report_card_id: String,
    created: bool,
    adopted_legacy: bool,
    /// The planner harness has never successfully started at the launchpad's current
    /// workspace path, so its thread must be re-opened. Derived from `operations`, not an
    /// in-memory comparison, so it survives a crash between commit and operation-submit.
    repointed: bool,
}

/// The `constraint` argument for the system-area race. A COLUMN list, never an index
/// name: SQLite words a unique violation as `UNIQUE constraint failed: <table>.<column>`
/// and names the index only for a unique index over expressions.
const SYSTEM_AREA_UNIQUE: &str = "areas.kind";

/// The `constraint` argument for the launchpad-track race. Its retry arm is unreachable
/// today, so clippy's `dead_code` on the non-test target is the only guard on the call site.
const LAUNCHPAD_UNIQUE: &str = "tracks.purpose";

/// A rendezvous the system-area mint race can be created at. `None` in production; a
/// test arms it with a `Barrier::new(2)` so both requests park after `area_get_system()`
/// returned `None` and before either opens its write transaction. Per-instance on
/// `AppState`, not cfg-gated or process-global: the tested binary must execute the
/// shipped instructions, and a threaded `cargo test` shares process-globals across cases.
pub type SystemAreaMintRendezvous = Option<std::sync::Arc<tokio::sync::Barrier>>;

#[derive(Debug, Default)]
pub struct SystemAreaMintCounters {
    /// Requests that found no system area and therefore tried to mint one. Two means both
    /// read `None` before either wrote — the race actually happened.
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

/// The read-only resolve the Today page load uses. Must never reach the harness: it
/// reads two rows and returns. No launchpad track is the ordinary state of a fresh
/// workspace, so it is `200` with `null`, not a 404 — this is the landing route and
/// browsers log every 404 as a console error.
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
    // A payload this build cannot parse is, by construction, not the canonical initial
    // payload, so `true` is the honest answer; treating it as empty would let one bad row
    // silently swallow a real report.
    let has_noninitial_content = serde_json::from_value::<TrackReportPayload>(report.payload)
        .map(|payload| payload.report_startup_read_required())
        .unwrap_or(true);
    Ok(Json(Some(TodayLaunchpadResolved {
        track_id: track.id.to_string(),
        report_has_noninitial_content: has_noninitial_content,
    })))
}

/// Is this track Today's launchpad? Identity against `track_get_launchpad`, not a
/// re-derivation from `purpose`. `false` when there is no launchpad yet; nothing is
/// ensured from here.
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
    /// Always `false` on success; returned rather than assumed so a caller sees the reset land.
    pub report_has_noninitial_content: bool,
}

/// `POST /api/today/launchpad/report/reset` — put today's report back to the canonical
/// empty document. The kernel calls `TrackReportPayload::initial()` itself; nothing
/// crosses the wire. Touches the report and nothing else. The revision anchor is read
/// here, so an edit landing in between yields the ordinary CRDT 409 and writes nothing.
#[utoipa::path(
    post,
    path = "/api/today/launchpad/report/reset",
    tag = "tracks",
    responses(
        (status = 200, description = "Today's report is back to the canonical empty document. Conversations are untouched.", body = TodayLaunchpadReportReset),
        (status = 401, description = "Missing or invalid session", body = ErrorBody),
        (status = 403, description = "Non-user actor (worker / plugin / planner) rejected, exactly as on `POST /api/tracks/{id}/report`", body = ErrorBody),
        (status = 404, description = "There is no launchpad track yet, so there is no report to reset", body = ErrorBody),
        (status = 409, description = "The report changed after Reset read its revision; nothing was reset and the action may be retried", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn reset_today_launchpad_report(
    State(s): State<RouteState>,
    // Extraction asserts the session middleware ran; nothing is read off it.
    _principal: crate::auth::Principal,
    actor: Actor,
) -> Result<Json<TodayLaunchpadReportReset>> {
    // The same raw-string gate the wholesale replace uses: `Actor::to_actor_id`'s fallback
    // maps unknown `ai:*` values to `User`, right for attribution and wrong for gating.
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
        s.task_budget_default,
    )
    .await?;
    let target = crate::track_report::ReportEditTarget::resolve(s.repo.as_ref(), &track_id).await?;
    crate::track_report::write::rest_user_replace(
        s.repo.as_ref(),
        &s.events,
        &s.write,
        target,
        TrackReportPayload::initial(),
        snapshot.doc_rev,
    )
    .await?;
    Ok(Json(TodayLaunchpadReportReset {
        track_id,
        report_has_noninitial_content: false,
    }))
}

/// The launchpad track's workspace: `Managed`, under the workspace root, and never
/// frozen — the launchpad is the one track whose path the kernel may legally re-point
/// (the adopt-legacy branch), and re-point + re-stamp is what the `frozen_at` latch
/// forbids. An old `<data_dir>/../launchpad` directory is deliberately left on disk.
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
        // Never `Some(..)`: a stamp here would break monotonicity on re-adoption.
        frozen_at: None,
    }
}

/// The launchpad's Planner is kernel-minted and runs on Codex.
fn planner_payload() -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": CODEX_PAYLOAD_SCHEMA_VERSION,
        "harness": { "snapshotVersion": 0, "pendingQueue": [] },
        crate::validation::PLANNER_PROVIDER_PAYLOAD_KEY: "codex",
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
        // Writes everything except the workspace; the single workspace writer below handles
        // that in the same tx.
        sqlx::query("UPDATE tracks SET purpose='launchpad', template_id=NULL, plugin_scope=NULL, template_input=NULL, updated_at=?2 WHERE id=?1")
            .bind(track.id.as_str()).bind(now_ms()).execute(&mut **tx).await?;
        track.purpose = Some("launchpad".into());
        track.template_id = None; track.plugin_scope = None; track.template_input = None;
        (track, false, true)
    } else {
        let id = new_id(); let now = now_ms();
        let sort: f64 = sqlx::query_scalar("SELECT CAST(COALESCE(MAX(sort),-1)+1 AS REAL) FROM tracks WHERE area_id=?1")
            .bind(area_id).fetch_one(&mut **tx).await?;
        // `cwd` is off this INSERT's column list; the single workspace writer below writes it.
        sqlx::query("INSERT INTO tracks(id,area_id,title,sort,lifecycle,template_id,purpose,template_input,created_at,updated_at) VALUES(?1,?2,'Today',?3,'draft',NULL,'launchpad',NULL,?4,?4)")
            .bind(&id).bind(area_id).bind(sort).bind(now).execute(&mut **tx).await?;
        s.write.area_cache().insert(TrackId::from(id.clone()), area_id.to_string().into());
        (Track { id:id.into(), area_id:area_id.to_string().into(), title:"Today".into(), sort,
            archived_at:None, pinned_at:None, lifecycle:Default::default(), cwd_wire_alias:String::new(),
            template_id:None, plugin_scope:None, purpose:Some("launchpad".into()), template_input:None,
            terminal_at:None, recipe_id:None, recipe_revision:None, workspace: TrackWorkspace::default(), claude_permissions_policy:None, created_at:now, updated_at:now }, true, false)
    };

    // ONE workspace writer for all three branches. The desired workspace is a pure function
    // of the track id, so this is a no-op on the steady state and a one-time re-point for
    // an older row.
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
            false,
        )
        .await?
    };
    // "Does the planner harness need re-anchoring?" must be derived from DURABLE state:
    // materialization runs after this tx commits, so if it fails or the process dies before
    // the operation is recorded, an in-memory comparison would read "steady state" next
    // time and pin the planner's codex thread to the OLD cwd forever.
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
        // Counted before the mint so a test can tell "both requests raced" from "the second
        // read the first's row".
        app.system_area_mint
            .attempts
            .fetch_add(1, Ordering::Relaxed);
        // Armed only by the concurrency case; `None` everywhere else.
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
            // The COLUMN form, not the index name. Reachable: `area_get_system()` runs OUTSIDE any
            // transaction, so two concurrent ensures can both read `None` and both reach the mint.
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
        // The COLUMN form, not the index name. This arm is unreachable today: `write_in_tx`
        // opens with BEGIN IMMEDIATE, so the SELECT and the INSERT sit in one writer-lock hold.
        // Kept as a fail-safe against moving that SELECT out of the transaction or a second
        // writer of `purpose='launchpad'` appearing.
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
    // The launchpad does not go through `create_track_structure` (raw `INSERT INTO tracks`),
    // so it carries its own materialize call; skipping it leaves every codex task on the
    // Today panel dying with `spawn-failed`.
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
        // A re-point also forces a new thread: the codex thread holds the cwd it was minted
        // with. The transcript is NOT reset — harness items are persisted per card, not per thread.
        force_new_thread: out.created || out.adopted_legacy || out.repointed,
        profile: Default::default(),
        create_card: None,
        first_message: None,
        create_request_sha256: None,
        opening_briefing: None,
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
                // The workspace path is part of the key, not just of the payload: the runtime refuses a
                // key already used with a different payload hash, and the payload carries `cwd`, so
                // after a re-point every `ensure` would be a 409 forever (nothing deletes `operations`
                // rows). Keying on the path mints a new key instead.
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

    /// How SQLite words a violation of the two partial unique indexes this module races on —
    /// a claim about SQLite, so it runs real migrations on a real database. Both index names
    /// are asserted NOT to match.
    #[tokio::test]
    async fn sqlite_names_the_columns_not_the_indexes_for_both_partial_unique_violations() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let pool = repo.pool();

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

        // SQLite names the index only for a unique index over *expressions*; pin the observed behaviour.
        assert!(
            !message.contains("idx_"),
            "no index name appears anywhere in the message: {message}"
        );
    }
}
