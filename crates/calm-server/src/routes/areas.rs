//! `/api/areas` — Area CRUD. `GET` defaults to `kind='user'`; the system area is minted only via the idempotent `POST /api/areas/system` upsert, and `DELETE` refuses a `kind='system'` row.

use crate::actor::Actor;
use crate::db::sqlite::{
    area_create_bind_tx, area_create_replay_tx, area_create_system_tx, area_create_tx,
    area_delete_tx, area_update_tx, overlay_delete_by_entity_tx, overlay_delete_subtree_by_area_tx,
    terminal_delete_tx,
};
use crate::db::{write_with_actor_events_typed, write_with_event_typed};
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use crate::model::{Area, AreaKind, AreaPatch, NewArea, Track};
use crate::operation::workspace_lease::{
    any_track_has_active_forge_action, release_workspace_leases_for_track_tx,
    sweep_workspace_worktrees_for_tracks_repo,
};
use crate::routes::cards::quiesce_shared_card_active_turn;
use crate::state::{AppState, CodexShellState, RouteState, WorkerState};
use crate::terminal_sweeper::quiesce_terminal_artifacts_for_deletion;
use crate::workspace_materialize::validate_attached_workspace;
use crate::workspace_recycle;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use futures::FutureExt;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use utoipa::{IntoParams, ToSchema};

use super::area_folders::normalize_path;
use super::terminal_cards::{parse_idempotency_key_header, stable_payload_hash};
#[cfg(feature = "fixtures")]
use std::collections::HashMap;
#[cfg(feature = "fixtures")]
use std::sync::{Mutex as StdMutex, OnceLock};
#[cfg(feature = "fixtures")]
use tokio::sync::Notify;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/areas", get(list_areas).post(create_area))
        .route(
            "/api/areas/system",
            axum::routing::post(get_or_create_system_area),
        )
        .route(
            "/api/areas/{id}",
            axum::routing::patch(update_area).delete(delete_area),
        )
}

/// Query string accepted by `GET /api/areas`.
#[derive(Debug, Default, Deserialize, IntoParams, ToSchema)]
pub struct ListAreasQuery {
    /// When true, also include `kind='system'` areas. Opt-in for debug surfaces and integration tests.
    #[serde(default)]
    pub include_system: bool,
}

/// User-facing Area creation. Deliberately permissive about unknown JSON keys: a caller-supplied `kind` must continue to be ignored rather than gaining a path to create a system Area.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct CreateAreaRequest {
    pub name: String,
    pub color: String,
    /// If absent, server appends to end.
    pub sort: Option<f64>,
    #[serde(default)]
    pub default_template_id: Option<String>,
    #[serde(default)]
    pub default_cwd: Option<String>,
}

fn validate_default_template(
    templates: &crate::templates::TemplateRoster,
    default_template_id: Option<&str>,
) -> Result<()> {
    let Some(template_id) = default_template_id else {
        return Ok(());
    };
    if templates.get(template_id).is_none() {
        return Err(CalmError::BadRequest(format!(
            "area default: `default_template_id` must reference a known track template; got `{template_id}`"
        )));
    }
    Ok(())
}

fn validate_and_normalize_default_cwd(default_cwd: &mut Option<String>) -> Result<()> {
    let Some(cwd) = default_cwd else {
        return Ok(());
    };
    validate_attached_workspace(std::path::Path::new(cwd))?;
    *cwd = normalize_path(cwd);
    Ok(())
}

#[utoipa::path(
    get,
    path = "/api/areas",
    tag = "areas",
    params(ListAreasQuery),
    responses(
        (status = 200, description = "List all areas (filtered to `kind='user'` unless `include_system=true` is set)", body = Vec<Area>),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_areas(
    State(s): State<RouteState>,
    Query(q): Query<ListAreasQuery>,
) -> Result<Json<Vec<Area>>> {
    // Default to the user-visible subset so the sidebar never sees the singleton system area.
    let areas = if q.include_system {
        s.repo.areas_list().await?
    } else {
        s.repo.areas_list_user_visible().await?
    };
    Ok(Json(areas))
}

#[utoipa::path(
    post,
    path = "/api/areas",
    tag = "areas",
    request_body = CreateAreaRequest,
    params(("Idempotency-Key" = Option<String>, Header, description = "Optional creation identity. The same key and typed request return the same Area with 201 without another creation event; differing inputs or a deleted Area return 409. Bindings are permanent. A replay does not repeat mutable template/folder validation. Callers without a key retain non-idempotent creation: retrying may create another Area. Separate keys may create Areas with the same name.")),
    responses(
        (status = 201, description = "Area created", body = Area),
        (status = 400, description = "Unknown default template, invalid attached default folder, or malformed Idempotency-Key", body = ErrorBody),
        (status = 409, description = "Creation key belongs to different inputs or its Area was deleted", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn create_area(
    State(s): State<RouteState>,
    actor: Actor,
    headers: HeaderMap,
    Json(mut request): Json<CreateAreaRequest>,
) -> Result<(StatusCode, Json<Area>)> {
    let key = parse_idempotency_key_header(&headers)?;
    // Versioned identity uses the original typed inputs, before mutable path
    // normalization or template validation. Null and absent options are equal.
    let fingerprint = format!("v1:{}", stable_payload_hash(&request)?);
    // Event writes refuse empty batches. A replay rolls back its read-only
    // transaction and returns the proven row through this local channel. Only
    // that branch populates it; unrelated errors cannot become successes.
    let (replay_tx, mut replay_rx) = tokio::sync::oneshot::channel();
    // `&'static`, captured by copy into the closure below.
    let templates = s.templates;
    let result =
        write_with_actor_events_typed(s.repo.as_ref(), None, &s.events, &s.write, move |tx| {
            Box::pin(async move {
                if let Some(key) = &key
                    && let Some(area) = area_create_replay_tx(tx, key, &fingerprint).await?
                {
                    let _ = replay_tx.send(area);
                    return Err(CalmError::Conflict(
                        "Area creation already committed".into(),
                    ));
                }
                validate_default_template(templates, request.default_template_id.as_deref())?;
                validate_and_normalize_default_cwd(&mut request.default_cwd)?;
                let mut area = area_create_tx(
                    tx,
                    NewArea {
                        name: request.name,
                        color: request.color,
                        sort: request.sort,
                    },
                )
                .await?;
                if request.default_template_id.is_some() || request.default_cwd.is_some() {
                    let area_id = area.id.clone();
                    area = area_update_tx(
                        tx,
                        area_id.as_str(),
                        AreaPatch {
                            default_template_id: request.default_template_id.map(Some),
                            default_cwd: request.default_cwd.map(Some),
                            ..AreaPatch::default()
                        },
                    )
                    .await?;
                }
                if let Some(key) = &key {
                    area_create_bind_tx(tx, key, &fingerprint, area.id.as_str()).await?;
                }
                // Creation reaches every replica through System scope. The
                // same transaction commits the row, optional binding and event.
                let event = (
                    actor.to_actor_id(),
                    EventScope::System,
                    Event::AreaUpdated(area.clone()),
                );
                Ok((area, vec![event]))
            })
        })
        .await;
    let area = match result {
        Ok((area, _ids)) => area,
        Err(error @ CalmError::Conflict(_)) => replay_rx.try_recv().map_err(|_| error)?,
        Err(error) => return Err(error),
    };
    Ok((StatusCode::CREATED, Json(area)))
}

#[utoipa::path(
    post,
    path = "/api/areas/system",
    tag = "areas",
    responses(
        (status = 200, description = "System area already existed; returned the existing row", body = Area),
        (status = 201, description = "System area minted", body = Area),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
/// Idempotent upsert for the singleton system area that hosts the default Today terminal. 200 with the existing row, else mint and 201.
/// The partial unique index on `areas(kind) WHERE kind = 'system'` backstops at-most-one: the loser of a write race re-reads the winner's row and returns 200.
pub(crate) async fn get_or_create_system_area(
    State(s): State<RouteState>,
    // `Actor` is extracted so the middleware validates `X-Calm-Actor`, but the event is stamped `ActorId::Kernel`: the system area is kernel-owned scaffolding and a `User` actor would be untruthful.
    _actor: Actor,
) -> Result<(StatusCode, Json<Area>)> {
    // Existence check first: the common path avoids opening a write transaction.
    if let Some(existing) = s.repo.area_get_system().await? {
        return Ok((StatusCode::OK, Json(existing)));
    }
    // Mint inside `write_with_event` so the create emits `area.updated` like the regular `POST /api/areas`.
    let mint_result = write_with_event_typed(
        s.repo.as_ref(),
        ActorId::Kernel,
        EventScope::System,
        None,
        &s.events,
        &s.write,
        move |tx| {
            Box::pin(async move {
                let area = area_create_system_tx(tx).await?;
                Ok((area.clone(), Event::AreaUpdated(area)))
            })
        },
    )
    .await;
    match mint_result {
        Ok((area, _id)) => Ok((StatusCode::CREATED, Json(area))),
        // Two cold-boot Today-page loads can both reach the mint; the partial unique index fails the loser's INSERT, so re-read and return 200. Any `Db` error retries the read (sqlx needs an `Any` boundary to downcast); if it was something else the follow-up read returns `None` and propagates.
        Err(e) => match e {
            CalmError::Db(_) => match s.repo.area_get_system().await? {
                Some(existing) => Ok((StatusCode::OK, Json(existing))),
                None => Err(e),
            },
            other => Err(other),
        },
    }
}

#[utoipa::path(
    patch,
    path = "/api/areas/{id}",
    tag = "areas",
    params(("id" = String, Path, description = "Area id")),
    request_body = AreaPatch,
    responses(
        (status = 200, description = "Area updated", body = Area),
        (status = 400, description = "Unknown default template or invalid attached default folder", body = ErrorBody),
        (status = 404, description = "Area not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn update_area(
    State(s): State<RouteState>,
    actor: Actor,
    Path(id): Path<String>,
    Json(mut p): Json<AreaPatch>,
) -> Result<Json<Area>> {
    // Resource-first: an unknown id 404s before an invalid caller-supplied path can trigger filesystem and `git` probes.
    s.repo
        .area_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("area {id}")))?;
    validate_default_template(
        s.templates,
        p.default_template_id
            .as_ref()
            .and_then(|value| value.as_deref()),
    )?;
    if let Some(value) = p.default_cwd.as_mut() {
        validate_and_normalize_default_cwd(value)?;
    }
    let scope = EventScope::Area {
        area: id.clone().into(),
    };
    let (area, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        &s.write,
        move |tx| {
            Box::pin(async move {
                let area = area_update_tx(tx, &id, p).await?;
                Ok((area.clone(), Event::AreaUpdated(area)))
            })
        },
    )
    .await?;
    Ok(Json(area))
}

struct PreparedAreaDeletion {
    id: String,
    area_kind: Option<AreaKind>,
    tracks: Vec<Track>,
    actor: ActorId,
    turn_daemon: std::sync::Arc<crate::shared_codex_appserver::SharedCodexAppServer>,
    _area_guard: crate::per_card_lock::KeyedLockGuard,
    _operation_guard: tokio::sync::OwnedMutexGuard<()>,
    _track_guards: Vec<crate::per_card_lock::KeyedLockGuard>,
}

struct QuiescedAreaDeletion {
    prepared: PreparedAreaDeletion,
    terminal_ids: Vec<String>,
    /// The Terminal cards whose generated hook settings file is removed on the committed arm only.
    terminal_card_ids: Vec<String>,
    sealed_thread_ids: Vec<String>,
    /// Every Card under every member Track, used only on the committed arm of [`RecycledAreaDeletion::commit`].
    card_ids: HashSet<String>,
}

struct RecycledAreaDeletion {
    quiesced: QuiescedAreaDeletion,
    recycle_report: workspace_recycle::AreaRecycleReport,
}

#[cfg(feature = "fixtures")]
#[derive(Clone)]
pub struct AreaDeleteCommitHook {
    pub entered: std::sync::Arc<Notify>,
    pub release: std::sync::Arc<Notify>,
    pub fail_after_release: bool,
    pub panic_after_release: bool,
}

#[cfg(feature = "fixtures")]
fn area_delete_commit_hooks() -> &'static StdMutex<HashMap<String, AreaDeleteCommitHook>> {
    static HOOKS: OnceLock<StdMutex<HashMap<String, AreaDeleteCommitHook>>> = OnceLock::new();
    HOOKS.get_or_init(|| StdMutex::new(HashMap::new()))
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn install_area_delete_commit_hook_for_test(area_id: &str, hook: AreaDeleteCommitHook) {
    area_delete_commit_hooks()
        .lock()
        .expect("area delete commit hook mutex")
        .insert(area_id.to_string(), hook);
}

async fn wait_at_area_delete_commit_hook(area_id: &str) -> (bool, bool) {
    #[cfg(feature = "fixtures")]
    {
        let hook = area_delete_commit_hooks()
            .lock()
            .expect("area delete commit hook mutex")
            .remove(area_id);
        if let Some(hook) = hook {
            hook.entered.notify_one();
            hook.release.notified().await;
            return (hook.fail_after_release, hook.panic_after_release);
        }
    }
    #[cfg(not(feature = "fixtures"))]
    let _ = area_id;
    (false, false)
}

impl PreparedAreaDeletion {
    async fn quiesce(
        self,
        route: &RouteState,
        worker: &WorkerState,
        codex: &CodexShellState,
    ) -> Result<QuiescedAreaDeletion> {
        crate::operation::terminal_disposal::require_safe(
            route.repo.as_ref(),
            crate::operation::terminal_disposal::Scope::Area(self.id.clone()),
            worker.daemon.proc_supervisor_sock.as_deref(),
        )
        .await?;
        let mut terminal_ids = Vec::new();
        let mut terminal_card_ids = Vec::new();
        let mut card_ids: HashSet<String> = HashSet::new();
        let mut seals = crate::shared_codex_appserver::DeletionThreadSeals::new(
            codex.shared_codex_appserver.clone(),
        );
        for track in &self.tracks {
            let cards = route.repo.cards_by_track(track.id.as_str()).await?;
            for card in &cards {
                card_ids.insert(card.id.to_string());
                if let Some(thread_id) =
                    quiesce_shared_card_active_turn(route.repo.as_ref(), codex, card).await?
                {
                    seals.seal(thread_id);
                }
                if let Some(terminal) = route.repo.terminal_get_by_card(card.id.as_str()).await? {
                    quiesce_terminal_artifacts_for_deletion(
                        Some(worker.terminal_renderer.as_ref()),
                        worker.daemon.proc_supervisor_sock.as_deref(),
                        &terminal,
                    )
                    .await?;
                    terminal_ids.push(terminal.id);
                    terminal_card_ids.push(terminal.card_id.to_string());
                }
            }
            // #1791 §5.1 item 4: after the track's threads are sealed and before its harness
            // shutdowns, revoke then sweep every Claude Planner id of the track in any state;
            // `Err` aborts before anything moves.
            crate::claude_planner::lifecycle::sweep_track(
                route.repo.as_ref(),
                &route.claude_planner,
                track.id.as_str(),
            )
            .await?;
            for thread_id in worker
                .harness
                .shutdown_track(&track.id, codex.shared_codex_appserver.clone())
                .await?
            {
                seals.seal(thread_id);
            }
        }
        Ok(QuiescedAreaDeletion {
            prepared: self,
            terminal_ids,
            terminal_card_ids,
            sealed_thread_ids: seals.retain(),
            card_ids,
        })
    }
}

impl QuiescedAreaDeletion {
    fn recycle(self, route: &RouteState) -> Result<RecycledAreaDeletion> {
        let Self {
            prepared,
            terminal_ids,
            terminal_card_ids,
            sealed_thread_ids,
            card_ids,
        } = self;
        let targets = prepared
            .tracks
            .iter()
            .map(|track| workspace_recycle::RecycleTarget {
                track_id: track.id.as_str(),
                workspace: &track.workspace,
            })
            .collect::<Vec<_>>();
        let now = crate::model::now_ms();
        let recycled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            workspace_recycle::recycle_area_workspaces(
                &route.workspace_root,
                &prepared.id,
                prepared.area_kind,
                &targets,
                now,
            )
        }));
        drop(targets);
        match recycled {
            Ok(Ok(recycle_report)) => {
                workspace_recycle::gc_trash_best_effort(&route.workspace_root, now);
                Ok(RecycledAreaDeletion {
                    quiesced: QuiescedAreaDeletion {
                        prepared,
                        terminal_ids,
                        terminal_card_ids,
                        sealed_thread_ids,
                        card_ids,
                    },
                    recycle_report,
                })
            }
            Ok(Err(error)) => {
                if prepared
                    .tracks
                    .iter()
                    .all(workspace_recycle::workspace_allows_runtime_recovery)
                {
                    for thread_id in &sealed_thread_ids {
                        prepared
                            .turn_daemon
                            .unseal_turn_thread_after_rollback(thread_id);
                    }
                }
                Err(error)
            }
            Err(_) => {
                if prepared
                    .tracks
                    .iter()
                    .all(workspace_recycle::workspace_allows_runtime_recovery)
                {
                    for thread_id in &sealed_thread_ids {
                        prepared
                            .turn_daemon
                            .unseal_turn_thread_after_rollback(thread_id);
                    }
                }
                Err(CalmError::Internal(format!(
                    "area deletion saga for {} panicked during workspace recycle",
                    prepared.id
                )))
            }
        }
    }
}

async fn finish_area_deletion(
    route: &RouteState,
    id: String,
    terminal_ids: Vec<String>,
    actor: ActorId,
) -> Result<(
    Vec<crate::operation::workspace_lease::WorkspaceTrackSweep>,
    Vec<String>,
)> {
    let scope = EventScope::Area {
        area: id.clone().into(),
    };
    let ((sweeps, deleted_track_ids), _event_ids) = write_with_actor_events_typed(
        route.repo.as_ref(),
        None,
        &route.events,
        &route.write,
        move |tx| {
            Box::pin(async move {
                crate::operation::terminal_disposal::require_safe_tx(
                    tx,
                    &crate::operation::terminal_disposal::Scope::Area(id.clone()),
                )
                .await?;
                for terminal_id in &terminal_ids {
                    match terminal_delete_tx(tx, terminal_id)
                        .await
                        .map_err(CalmError::from)
                    {
                        Ok(()) | Err(CalmError::NotFound(_)) => {}
                        Err(error) => return Err(error),
                    }
                }
                overlay_delete_subtree_by_area_tx(tx, &id).await?;
                overlay_delete_by_entity_tx(tx, "area", &id).await?;
                let mut events = Vec::new();
                let mut sweeps = Vec::new();
                let deleted_track_ids: Vec<String> =
                    sqlx::query_scalar("SELECT id FROM tracks WHERE area_id = ?1 ORDER BY id")
                        .bind(&id)
                        .fetch_all(&mut **tx)
                        .await?;
                for track_id in &deleted_track_ids {
                    let release = release_workspace_leases_for_track_tx(tx, track_id).await?;
                    events.extend(release.events);
                    if let Some(sweep) = release.sweep {
                        sweeps.push(sweep);
                    }
                }
                area_delete_tx(tx, &id).await?;
                events.push((actor, scope, Event::AreaDeleted { id: id.into() }));
                Ok(((sweeps, deleted_track_ids), events))
            })
        },
    )
    .await?;
    Ok((sweeps, deleted_track_ids))
}

impl RecycledAreaDeletion {
    async fn commit(mut self, route: &RouteState) -> Result<()> {
        let area_id = self.quiesced.prepared.id.clone();
        let (fail_for_test, panic_for_test) = wait_at_area_delete_commit_hook(&area_id).await;
        if panic_for_test {
            panic!("fixture: panic area deletion after recycle");
        }
        let result = if fail_for_test {
            Err(CalmError::Internal(
                "fixture: fail area deletion after recycle".into(),
            ))
        } else {
            finish_area_deletion(
                route,
                area_id.clone(),
                self.quiesced.terminal_ids.clone(),
                self.quiesced.prepared.actor.clone(),
            )
            .await
        };
        let (sweeps, deleted_track_ids) = match result {
            Ok(committed) => committed,
            Err(error) => {
                if let Err(restore_error) =
                    workspace_recycle::restore_area_recycle_report(&self.recycle_report)
                {
                    return Err(CalmError::Internal(format!(
                        "area deletion rolled back ({error}), but workspace compensation failed: {restore_error}"
                    )));
                }
                for thread_id in &self.quiesced.sealed_thread_ids {
                    self.quiesced
                        .prepared
                        .turn_daemon
                        .unseal_turn_thread_after_rollback(thread_id);
                }
                return Err(error);
            }
        };
        // The area delete has COMMITTED. Drop the shared daemon's in-memory thread attribution for every deleted Card so a later reconnect cannot resume a thread with no database owner. Post-commit and infallible.
        self.quiesced
            .prepared
            .turn_daemon
            .forget_threads_for_deleted_cards(&self.quiesced.card_ids)
            .await;
        // Drop the deletion-time seal verdict and any active turn id for the threads this delete sealed.
        self.quiesced
            .prepared
            .turn_daemon
            .forget_turn_state_for_deleted_threads(&self.quiesced.sealed_thread_ids);
        // Post-commit, best effort: the generated hook settings file of every deleted Terminal card.
        for card_id in &self.quiesced.terminal_card_ids {
            route.terminal_renderer.remove_hook_settings(card_id);
        }
        for track_id in deleted_track_ids {
            let track_id = crate::ids::TrackId::from(track_id);
            route.mcp_context.preview.release_track(&track_id);
            route.write.forget_track(&track_id);
        }
        workspace_recycle::finalize_area_recycle(
            &route.workspace_root,
            &area_id,
            &mut self.recycle_report,
        );
        sweep_workspace_worktrees_for_tracks_repo(route.repo.as_ref(), &route.events, sweeps)
            .await?;
        Ok(())
    }
}

async fn run_recycled_area_deletion(
    route: &RouteState,
    deletion: RecycledAreaDeletion,
) -> Result<()> {
    let recovery_report = deletion.recycle_report.clone();
    let recovery_tracks = deletion.quiesced.prepared.tracks.clone();
    let recovery_thread_ids = deletion.quiesced.sealed_thread_ids.clone();
    let recovery_turn_daemon = deletion.quiesced.prepared.turn_daemon.clone();
    let recovery_area_id = deletion.quiesced.prepared.id.clone();
    match std::panic::AssertUnwindSafe(deletion.commit(route))
        .catch_unwind()
        .await
    {
        Ok(result) => result,
        Err(_) => {
            if route.repo.area_get(&recovery_area_id).await?.is_some() {
                workspace_recycle::restore_area_recycle_report(&recovery_report)?;
                for thread_id in &recovery_thread_ids {
                    recovery_turn_daemon.unseal_turn_thread_after_rollback(thread_id);
                }
            } else {
                for track in &recovery_tracks {
                    route.write.forget_track(&track.id);
                }
                let mut committed_report = recovery_report;
                workspace_recycle::finalize_area_recycle(
                    &route.workspace_root,
                    &recovery_area_id,
                    &mut committed_report,
                );
            }
            Err(CalmError::Internal(format!(
                "area deletion saga for {recovery_area_id} panicked"
            )))
        }
    }
}

#[allow(deprecated)]
async fn finish_prepared_area_deletion_owned(
    route: RouteState,
    worker: WorkerState,
    codex: CodexShellState,
    prepared: PreparedAreaDeletion,
) -> Result<()> {
    let area_id = prepared.id.clone();
    let recovery_track_ids: HashSet<_> = prepared
        .tracks
        .iter()
        .map(|track| track.id.clone())
        .collect();
    let task_area_id = area_id.clone();
    tokio::spawn(async move {
        let workflow = std::panic::AssertUnwindSafe(async {
            let recycled = prepared
                .quiesce(&route, &worker, &codex)
                .await?
                .recycle(&route)?;
            run_recycled_area_deletion(&route, recycled).await
        })
        .catch_unwind()
        .await;
        let result = match workflow {
            Ok(result) => result,
            Err(_) => Err(CalmError::Internal(format!(
                "area deletion saga for {task_area_id} panicked before recycle"
            ))),
        };
        let recovery = crate::harness::HarnessRecoveryContext::new(
            worker.repo.clone(),
            route.events.clone(),
            route.write.role_cache().clone(),
            route.write.area_cache().clone(),
            codex.shared_codex_appserver.clone(),
            route.claude_planner_wiring(),
            worker.harness.clone(),
            route.track_delete_locks.clone(),
        );
        if result.is_err()
            && let Err(error) =
                crate::harness::recover_harnesses_for_tracks(&recovery, &recovery_track_ids).await
        {
            tracing::error!(
                area_id = %task_area_id,
                error = %error,
                "aborted area deletion could not recover every planner harness"
            );
        }
        result
    })
    .await
    .map_err(|error| {
        CalmError::Internal(format!(
            "owned deletion task for area {area_id} failed: {error}"
        ))
    })?
}

#[utoipa::path(
    delete,
    path = "/api/areas/{id}",
    tag = "areas",
    params(("id" = String, Path, description = "Area id")),
    responses(
        (status = 204, description = "Area deleted"),
        (status = 403, description = "Area is system-owned and cannot be deleted via REST", body = ErrorBody),
        (status = 404, description = "Area not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn delete_area(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    State(cs): State<CodexShellState>,
    actor: Actor,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    // Refuse to delete the singleton system area at the handler boundary; `area_delete_tx` trusts its caller. `None` (no such area) makes every recycle below refuse; the row delete still 404s naturally.
    // Lock order is area delete → operation drive → sorted track delete; the track-create route takes this area lock before entering the operation driver, so neither side can invert the pair.
    let area_delete_guard = crate::per_card_lock::lock_key(&s.area_delete_locks, &id).await;
    let area_kind = s.repo.area_get(&id).await?.map(|area| area.kind);
    if area_kind == Some(AreaKind::System) {
        return Err(CalmError::Forbidden(format!(
            "area {id} is system-owned and cannot be deleted via the public API"
        )));
    }

    // Track DELETE holds this same guard through commit or compensation, so an area deletion cannot erase the rows underneath a workspace restoration.
    let operation_guard = s.operation_runtime.lock_for_track_delete().await;

    let tracks = s.repo.tracks_by_area(&id).await?;
    let mut guarded_track_ids = tracks
        .iter()
        .map(|track| track.id.to_string())
        .collect::<Vec<_>>();
    guarded_track_ids.sort();
    // Direct harness recovery and websocket terminal reattach bypass the operation driver; lock every member in stable order so those paths either finish first or observe deleted rows.
    let mut track_delete_guards = Vec::with_capacity(guarded_track_ids.len());
    for track_id in &guarded_track_ids {
        track_delete_guards
            .push(crate::per_card_lock::lock_key(&s.track_delete_locks, track_id).await);
    }
    let track_ids = guarded_track_ids
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    // Defensive TOCTOU guard only: this non-transactional read happens before the teardown tx, so a forge-action can still become in-flight; durable parked recovery is the backstop.
    let pool = w.repo.sqlite_pool().ok_or_else(|| {
        CalmError::Internal("delete_area forge-action fence requires sqlite-backed repo".into())
    })?;
    let mut preflight = crate::db::sqlite::begin_immediate_tx(&pool).await?;
    for track_id in &track_ids {
        crate::db::sqlite::track_require_candidate_verification_settled_tx(
            &mut preflight,
            track_id,
        )
        .await?;
    }
    preflight.rollback().await?;
    if any_track_has_active_forge_action(&pool, &track_ids).await? {
        return Err(CalmError::Conflict(format!(
            "area {id} has a child track with an in-flight forge-action; retry after it settles"
        )));
    }

    let prepared = PreparedAreaDeletion {
        id,
        area_kind,
        tracks,
        actor: actor.to_actor_id(),
        turn_daemon: cs.shared_codex_appserver.clone(),
        _area_guard: area_delete_guard,
        _operation_guard: operation_guard,
        _track_guards: track_delete_guards,
    };
    finish_prepared_area_deletion_owned(s, w, cs, prepared).await?;
    Ok(StatusCode::NO_CONTENT)
}
