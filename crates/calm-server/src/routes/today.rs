//! Server-owned Today launchpad bootstrap.

use crate::actor::Actor;
use crate::db::rows::TRACK_SELECT_COLUMNS;
use crate::db::write_in_tx_typed;
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::extract::Json;
use crate::ids::{ActorId, TrackId};
use crate::model::{
    CardRole, NewCard, RequestTheme, Track, TrackWorkspace, TrackWorkspaceKind, new_id, now_ms,
};
use crate::state::{AppState, RouteState};
use crate::track_report::TrackReportPayload;
use axum::{
    Router,
    extract::{FromRef, State},
    http::StatusCode,
    routing::{get, post},
};
use calm_truth::db::sqlite::{
    area_create_system_tx, card_create_with_id_tx, card_with_terminal_create_tx,
    track_workspace_write_tx,
};
use calm_types::card_presentation::{CardCreationSource, TerminalPresentation};
use serde::Serialize;
use sqlx::{Sqlite, Transaction};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/today/launchpad/ensure",
            post(ensure_today_launchpad_route),
        )
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
    created: bool,
    adopted_legacy: bool,
}

/// The `constraint` argument for the system-area race. A COLUMN list, never an index
/// name: SQLite words a unique violation as `UNIQUE constraint failed: <table>.<column>`
/// and names the index only for a unique index over expressions.
#[cfg(test)]
const SYSTEM_AREA_UNIQUE: &str = "areas.kind";

/// SQLite names the columns for the launchpad singleton index too.
#[cfg(test)]
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
    /// Competing mint attempts that reused the winner under the writer lock.
    pub retries: AtomicU64,
}

#[cfg(test)]
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
    (status = 404, description = "The launchpad track exists but carries no `track-report` card. Not a reachable state; see the handler docs.", body = ErrorBody),
    (status = 500, description = "Internal error", body = ErrorBody)
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
    repo: &(impl calm_truth::db::RepoRead + ?Sized),
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
/// crosses the wire. Data blocks go too, each as if deleted by id: the task projection
/// drops a removed task's pending row and marks a claimed one stale, in the same write.
/// Conversations are untouched. The revision anchor is read here, so an edit landing in
/// between yields the ordinary CRDT 409 and writes nothing.
#[utoipa::path(
    post,
    path = "/api/today/launchpad/report/reset",
    tag = "tracks",
    responses(
        (status = 200, description = "Today's report is back to the canonical empty document. Conversations are untouched.", body = TodayLaunchpadReportReset),
        (status = 400, description = "A malformed `X-Calm-Actor` header, refused by the actor middleware before anything is read or written", body = ErrorBody),
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
    )
    .await?;
    let target = crate::track_report::ReportEditTarget::resolve(s.repo.as_ref(), &track_id).await?;
    crate::track_report::write::rest_user_reset(
        s.repo.as_ref(),
        &s.events,
        &s.write,
        target,
        snapshot.doc_rev,
    )
    .await?;
    Ok(Json(TodayLaunchpadReportReset {
        track_id,
        report_has_noninitial_content: false,
    }))
}

fn launchpad_workspace(workspace_root: &Path, area_id: &str, track_id: &str) -> TrackWorkspace {
    TrackWorkspace {
        kind: TrackWorkspaceKind::Managed,
        path: workspace::materialize::managed_workspace_path(workspace_root, area_id, track_id)
            .to_string_lossy()
            .into_owned(),
        frozen_at: None,
        worktree: None,
    }
}

#[derive(Clone, Copy, Debug)]
enum AdoptionRefusal {
    WorkspaceDrift,
    IdentityUnproven,
    BindingMismatch,
    NativeAssociationMismatch,
    SnapshotUnproven,
    PhaseUnproven,
}
impl AdoptionRefusal {
    fn code(self) -> &'static str {
        match self {
            Self::WorkspaceDrift => "today_adoption_workspace_drift",
            Self::IdentityUnproven => "today_adoption_identity_unproven",
            Self::BindingMismatch => "today_adoption_binding_mismatch",
            Self::NativeAssociationMismatch => "today_adoption_native_association_mismatch",
            Self::SnapshotUnproven => "today_adoption_snapshot_unproven",
            Self::PhaseUnproven => "today_adoption_phase_unproven",
        }
    }
}

// Creation provenance comes from its typed owner contract, independently of the
// Workspace and execution graph whose damage must remain visible as a candidate.
async fn terminal_creation_source_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &Track,
) -> Result<Option<CardCreationSource>> {
    let payloads: Vec<String> =
        sqlx::query_scalar("SELECT payload FROM cards WHERE track_id=?1 AND kind='terminal'")
            .bind(track.id.as_str())
            .fetch_all(&mut **tx)
            .await?;
    let [payload] = payloads.as_slice() else {
        return Ok(None);
    };
    Ok(serde_json::from_str(payload)
        .ok()
        .and_then(|value| TerminalPresentation::from_payload(&value).ok())
        .map(|presentation| presentation.source))
}

async fn prove_existing_today_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &Track,
    area: Option<&str>,
    root: &Path,
) -> Result<std::result::Result<TodayLaunchpad, AdoptionRefusal>> {
    use crate::db::sqlite::conversation;
    use crate::harness::profile::{HarnessProfile, PlannerBinding};
    use crate::harness::snapshot::{CanonicalHarnessSnapshot, HarnessPhaseTag, HarnessSnapshot};
    use AdoptionRefusal::*;
    if area != Some(track.area_id.as_str())
        || track.closed_at.is_some()
        || terminal_creation_source_tx(tx, track).await? != Some(CardCreationSource::System)
    {
        return Ok(Err(IdentityUnproven));
    }
    if track.workspace != launchpad_workspace(root, track.area_id.as_str(), track.id.as_str()) {
        return Ok(Err(WorkspaceDrift));
    }
    let planners: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT id,session_id FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(track.id.as_str())
            .fetch_all(&mut **tx)
            .await?;
    let [(card, Some(worker))] = planners.as_slice() else {
        return Ok(Err(IdentityUnproven));
    };
    if let Err(error) = conversation::current_native_fact_tx(tx, card, worker, true).await {
        return match error {
            calm_truth::error::CalmError::Db(_) => Err(error.into()),
            _ => Ok(Err(IdentityUnproven)),
        };
    }
    let shape = match crate::db::sqlite::card_execution_shape_tx(tx, card).await {
        Ok(shape) => shape,
        Err(calm_truth::error::TruthError::Serde(_)) => return Ok(Err(BindingMismatch)),
        Err(error) => return Err(error.into()),
    };
    let Some(binding) = PlannerBinding::from_shape(&shape.kind, shape.role, &shape.payload)
        .filter(|b| b.profile == HarnessProfile::Planner)
    else {
        return Ok(Err(BindingMismatch));
    };
    let row = match crate::db::sqlite::session_get_tx(
        tx,
        &calm_types::worker::WorkerSessionId(worker.clone()),
    )
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return Ok(Err(IdentityUnproven)),
        Err(calm_truth::error::TruthError::Serde(_)) => return Ok(Err(SnapshotUnproven)),
        Err(error) => return Err(error.into()),
    };
    if !binding.matches_session_contract(&row) {
        return Ok(Err(BindingMismatch));
    }
    let Some(snapshot) = row.handle_state_json else {
        return Ok(Err(SnapshotUnproven));
    };
    // Decode for proof only. Neither codec's serialization or recovery runs here.
    let thread = if snapshot.get("canonical_inputs").is_some() {
        let Ok(frame) = serde_json::from_value::<CanonicalHarnessSnapshot>(snapshot) else {
            return Ok(Err(SnapshotUnproven));
        };
        if frame.lifecycle_refusal().is_some() {
            return Ok(Err(PhaseUnproven));
        }
        frame.header.last_thread_id
    } else {
        let Ok(frame) = serde_json::from_value::<HarnessSnapshot>(snapshot.clone()) else {
            return Ok(Err(SnapshotUnproven));
        };
        if frame.schema_version != crate::harness::snapshot::LEGACY_HARNESS_SNAPSHOT_SCHEMA_VERSION
            || frame.mode != crate::harness::HARNESS_MODE
        {
            return Ok(Err(SnapshotUnproven));
        }
        if !crate::harness::HarnessRuntimeSnapshot::idle_for_lifecycle(snapshot).unwrap_or(false)
            || !matches!(
                frame.phase,
                HarnessPhaseTag::Idle | HarnessPhaseTag::TurnCompleted | HarnessPhaseTag::Resumed
            )
            || frame.pending_rewind.is_some()
            || frame.interruption_intent.is_some()
            || frame.wedged_reason.is_some()
        {
            return Ok(Err(PhaseUnproven));
        }
        frame.last_thread_id
    };
    if thread != row.thread_id {
        return Ok(Err(NativeAssociationMismatch));
    }
    let reports: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM cards WHERE track_id=?1 AND kind='track-report' AND role='reportcard'",
    ).bind(track.id.as_str()).fetch_one(&mut **tx).await?;
    let terminals: Vec<(String, String)> = sqlx::query_as(
        "SELECT c.id,t.id FROM cards c JOIN terminals t ON t.card_id=c.id WHERE c.track_id=?1 AND c.kind='terminal'",
    ).bind(track.id.as_str()).fetch_all(&mut **tx).await?;
    let [(terminal_card, terminal)] = terminals.as_slice() else {
        return Ok(Err(IdentityUnproven));
    };
    if reports != 1 {
        return Ok(Err(IdentityUnproven));
    }
    Ok(Ok(TodayLaunchpad {
        track_id: track.id.to_string(),
        planner_card_id: card.clone(),
        terminal_card_id: terminal_card.clone(),
        terminal_id: terminal.clone(),
    }))
}

#[allow(deprecated)]
async fn today_launchpad_ensure_tx(
    tx: &mut Transaction<'_, Sqlite>,
    s: &RouteState,
    area_id: Option<&str>,
    workspace_root: &Path,
) -> Result<(
    std::result::Result<EnsureTxResult, AdoptionRefusal>,
    Vec<(ActorId, EventScope, Event)>,
)> {
    let candidates_rows = sqlx::query_as::<_, crate::db::rows::TrackRow>(&format!(
        "SELECT {TRACK_SELECT_COLUMNS} FROM tracks WHERE purpose='launchpad' OR (purpose IS NULL AND title='Today') ORDER BY (purpose='launchpad') DESC,created_at,id"
    )).fetch_all(&mut **tx).await?;
    let mut candidates = Vec::new();
    for row in candidates_rows {
        let track = Track::from(row);
        // Only explicit Manual provenance outside the System Area excludes an
        // ordinary namesake. Missing/invalid/Unrecorded provenance proves nothing.
        if track.purpose.is_none()
            && area_id != Some(track.area_id.as_str())
            && terminal_creation_source_tx(tx, &track).await? == Some(CardCreationSource::Manual)
        {
            continue;
        }
        candidates.push(track);
    }
    if let Some(mut track) = candidates.pop() {
        // No ordering can authorize adoption while another candidate is unresolved.
        let proof = if candidates.is_empty() {
            prove_existing_today_tx(tx, &track, area_id, workspace_root).await?
        } else {
            Err(AdoptionRefusal::IdentityUnproven)
        };
        let dto = match proof {
            Ok(dto) => dto,
            Err(reason) => {
                tracing::warn!(track_id = %track.id, candidate_count = candidates.len() + 1, code = reason.code(), "Today adoption refused before mutation");
                return Ok((Err(reason), vec![]));
            }
        };
        let adopted_legacy = track.purpose.is_none();
        if adopted_legacy {
            sqlx::query("UPDATE tracks SET purpose='launchpad',template_id=NULL,plugin_scope=NULL,template_input=NULL,updated_at=?2 WHERE id=?1")
                .bind(track.id.as_str()).bind(now_ms()).execute(&mut **tx).await?;
            track.purpose = Some("launchpad".into());
        }
        return Ok((
            Ok(EnsureTxResult {
                dto,
                track,
                created: false,
                adopted_legacy,
            }),
            vec![],
        ));
    }
    let mut events = vec![];
    let area_id = if let Some(id) = area_id {
        id.to_owned()
    } else {
        let area = area_create_system_tx(tx).await?;
        let id = area.id.to_string();
        events.push((
            ActorId::Kernel,
            EventScope::System,
            Event::AreaUpdated(area),
        ));
        id
    };
    let area_id = area_id.as_str();
    let mut track = {
        let id = new_id();
        let now = now_ms();
        let sort: f64 = sqlx::query_scalar(
            "SELECT CAST(COALESCE(MAX(sort),-1)+1 AS REAL) FROM tracks WHERE area_id=?1",
        )
        .bind(area_id)
        .fetch_one(&mut **tx)
        .await?;
        // `cwd` is off this INSERT's column list; the single workspace writer below writes it.
        sqlx::query("INSERT INTO tracks(id,area_id,title,sort,template_id,purpose,template_input,admin_granted_at,created_at,updated_at) VALUES(?1,?2,'Today',?3,NULL,'launchpad',NULL,NULL,?4,?4)")
            .bind(&id).bind(area_id).bind(sort).bind(now).execute(&mut **tx).await?;
        Track {
            id: id.into(),
            area_id: area_id.to_string().into(),
            title: "Today".into(),
            sort,
            pinned_at: None,
            closed_at: None,
            admin_granted_at: None,
            cwd_wire_alias: String::new(),
            template_id: None,
            plugin_scope: None,
            purpose: Some("launchpad".into()),
            template_input: None,
            recipe_id: None,
            recipe_revision: None,
            creator_track_id: None,
            creator_key: None,
            workspace: TrackWorkspace::default(),
            created_at: now,
            updated_at: now,
        }
    };

    let desired = launchpad_workspace(workspace_root, area_id, track.id.as_str());
    track_workspace_write_tx(tx, track.id.as_str(), &desired).await?;
    track.cwd_wire_alias = desired.path.clone();
    track.workspace = desired;
    workspace_runtime::selection::materialize_workspace(
        &track.workspace,
        workspace_root,
        track.id.as_str(),
    )?;
    let cwd = track.workspace.path.as_str();
    let planner = card_create_with_id_tx(
        tx,
        new_id(),
        NewCard {
            title: None,
            track_id: track.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: crate::track_creation::planner_harness_card_payload(
                &s.acp_planner,
                None,
                crate::session_projection_repo::AgentProvider::Codex,
            ),
        },
        CardRole::Planner,
        false,
        s.write.role_cache(),
    )
    .await?;
    crate::track_creation::allocate_empty_planner_carrier_tx(
        tx,
        planner.id.as_str(),
        crate::session_projection_repo::AgentProvider::Codex,
    )
    .await?;
    let _report = card_create_with_id_tx(
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
    .await?;
    let (terminal_card, terminal) = card_with_terminal_create_tx(
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
        calm_types::card_presentation::CardCreationSource::System,
    )
    .await?;
    Ok((
        Ok(EnsureTxResult {
            dto: TodayLaunchpad {
                track_id: track.id.to_string(),
                planner_card_id: planner.id.to_string(),
                terminal_card_id: terminal_card.id.to_string(),
                terminal_id: terminal.id,
            },
            track,
            created: true,
            adopted_legacy: false,
        }),
        events,
    ))
}

async fn ensure_today_launchpad_route(
    State(app): State<AppState>,
    actor: Actor,
) -> Result<axum::response::Response> {
    use axum::response::IntoResponse;
    Ok(match ensure_today_launchpad_decision(State(app), actor).await? {
        Ok(reply) => reply.into_response(),
        Err(reason) => (StatusCode::CONFLICT, Json(ErrorBody {
            error: "Today could not be adopted safely. Its Workspace and conversation have been preserved.".into(),
            code: reason.code().into(), field: None,
        })).into_response(),
    })
}

#[utoipa::path(post,path="/api/today/launchpad/ensure",tag="tracks",responses(
    (status=200,description="Existing launchpad retained",body=TodayLaunchpad),
    (status=201,description="Empty canonical launchpad minted or existing Today safely adopted",body=TodayLaunchpad),
    (status=409,description="Today adoption refused; code identifies Workspace, identity, binding, snapshot, phase or native association proof failure. Nothing changed.",body=ErrorBody),
    (status=500,description="Internal error",body=ErrorBody)
))]
pub(crate) async fn ensure_today_launchpad(
    app: State<AppState>,
    actor: Actor,
) -> Result<(StatusCode, Json<TodayLaunchpad>)> {
    ensure_today_launchpad_decision(app, actor)
        .await?
        .map_err(|reason| CalmError::Conflict(reason.code().into()))
}

async fn ensure_today_launchpad_decision(
    State(app): State<AppState>,
    _actor: Actor,
) -> Result<std::result::Result<(StatusCode, Json<TodayLaunchpad>), AdoptionRefusal>> {
    let area_absent = app.repo.area_get_system().await?.is_none();
    if area_absent {
        app.system_area_mint
            .attempts
            .fetch_add(1, Ordering::Relaxed);
        if let Some(barrier) = &app.system_area_mint_rendezvous {
            barrier.wait().await;
        }
    }
    let route = RouteState::from_ref(&app);
    let workspace_root = app.workspace_root().to_path_buf();
    let root = workspace_root.clone();
    let counters = app.system_area_mint.clone();
    let (decision, ids, pending) = write_in_tx_typed(app.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let area: Option<String> =
                sqlx::query_scalar("SELECT id FROM areas WHERE kind='system'")
                    .fetch_optional(&mut **tx)
                    .await?;
            if area_absent && area.is_some() {
                counters.retries.fetch_add(1, Ordering::Relaxed);
            }
            let (decision, pending) =
                today_launchpad_ensure_tx(tx, &route, area.as_deref(), &root).await?;
            let ids = if pending.is_empty() {
                vec![]
            } else {
                crate::db::sqlite::append_actor_events_in_tx(tx, None, &route.write, &pending)
                    .await?
                    .0
            };
            Ok((decision, ids, pending))
        })
    })
    .await?;
    crate::db::sqlite::broadcast_actor_events(&app.events, &ids, pending);
    let out = match decision {
        Ok(out) => out,
        Err(reason) => return Ok(Err(reason)),
    };
    if out.created {
        app.write().remember_track(
            TrackId::from(out.track.id.to_string()),
            out.track.area_id.clone(),
        );
    }
    let status = if out.created || out.adopted_legacy {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok(Ok((status, Json(out.dto))))
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
                "INSERT INTO tracks(id,area_id,title,sort,purpose,created_at,updated_at) \
                 VALUES(?1,'area-winner','Today',1,'launchpad',1,1)",
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
