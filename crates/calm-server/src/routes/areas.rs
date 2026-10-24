//! `/api/areas` — Area CRUD. `GET` defaults to `kind='user'`; the system area is minted only via the idempotent `POST /api/areas/system` upsert, and `DELETE` refuses a `kind='system'` row.
use workspace_runtime::sweep::sweep_workspace_worktrees_for_tracks;

use crate::actor::Actor;
use crate::auth::Principal;
use crate::db::sqlite::{
    area_create_bind_tx, area_create_replay_tx, area_create_system_tx, area_create_tx,
    area_delete_tx, area_update_tx, overlay_delete_by_entity_tx, overlay_delete_subtree_by_area_tx,
    terminal_delete_tx,
};
use crate::db::{write_with_actor_events_typed, write_with_event_typed};
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::extract::{Json, JsonBody, Path, Query};
use crate::ids::ActorId;
use crate::model::{Area, AreaKind, AreaPatch, NewArea, Track};
use crate::operation::workspace_lease::{
    any_track_has_active_forge_action, release_workspace_leases_for_track_tx,
};
use crate::organization_access::{
    require_area_access, require_area_access_tx, require_deployment_owner,
};
use crate::routes::planner_cards::quiesce_shared_codex_card_active_turn;
use crate::state::{AppState, CodexShellState, RouteState, WorkerState};
use crate::terminal_sweeper::quiesce_terminal_artifacts_for_deletion;
use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::get,
};
use calm_types::organization::AreaAccess;
use futures::FutureExt;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use utoipa::{IntoParams, ToSchema};
use workspace_runtime::recycle as workspace_recycle;
use workspace_runtime::selection::validate_attached_workspace;

use super::area_folders::normalize_path;
use super::idempotency_key::{parse_idempotency_key_header, stable_payload_hash};
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
    /// Deployment owner only: also include the kernel-owned system Area.
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
        (status = 200, description = "List the authenticated user's member Areas; include_system additionally requires deployment-owner authority", body = Vec<Area>),
        (status = 403, description = "Registered user required; include_system requires deployment-owner authority", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_areas(
    State(s): State<RouteState>,
    principal: Principal,
    Query(q): Query<ListAreasQuery>,
) -> Result<Json<Vec<Area>>> {
    if q.include_system {
        require_deployment_owner(&principal)?;
    }
    let mut areas = s.repo.areas_list_for_member(&principal.user_id).await?;
    if q.include_system
        && let Some(system) = s.repo.area_get_system().await?
    {
        areas.push(system);
        areas.sort_by(|a, b| {
            a.sort
                .total_cmp(&b.sort)
                .then(a.id.as_str().cmp(b.id.as_str()))
        });
    }
    Ok(Json(areas))
}

#[utoipa::path(
    post,
    path = "/api/areas",
    tag = "areas",
    request_body = CreateAreaRequest,
    params(("Idempotency-Key" = Option<String>, Header, description = "Optional permanent creation identity scoped to the authenticated creator. The same creator, key and typed request return the same Area with 201 without another creation event; differing inputs return 409 `idempotency_key_reused` and a deleted Area 409 `idempotency_key_exhausted`. Bindings are permanent. A replay does not repeat mutable template/folder validation. Callers without a key retain non-idempotent creation: retrying may create another Area. Separate keys may create Areas with the same name. A blank, non-ASCII or over-128-byte key is 400.")),
    responses(
        (status = 201, description = "Area created", body = Area),
        (status = 403, description = "Registered creator required; replay requires current Area membership; cross-origin writes are forbidden", body = ErrorBody),
        (status = 400, description = "Unknown default template, invalid default folder, or an invalid Idempotency-Key (`idempotency_key_invalid`)", body = ErrorBody),
        (status = 409, description = "The key belongs to different inputs (`idempotency_key_reused`) or its Area was deleted (`idempotency_key_exhausted`)", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn create_area(
    State(s): State<RouteState>,
    actor: Actor,
    principal: Principal,
    headers: HeaderMap,
    JsonBody(mut request): JsonBody<CreateAreaRequest>,
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
                crate::db::sqlite::area_membership::application_user_require_registered(
                    tx,
                    &principal.user_id,
                )
                .await?;
                if let Some(key) = &key
                    && let Some(area) =
                        area_create_replay_tx(tx, &principal.user_id, key, &fingerprint).await?
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
                    &principal.user_id,
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
                    area_create_bind_tx(
                        tx,
                        &principal.user_id,
                        key,
                        &fingerprint,
                        area.id.as_str(),
                    )
                    .await?;
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
        (status = 403, description = "Registered deployment owner required; cross-origin writes are forbidden", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
/// Idempotent upsert for the singleton system area that hosts the default Today terminal. 200 with the existing row, else mint and 201.
/// The partial unique index on `areas(kind) WHERE kind = 'system'` backstops at-most-one: the loser of a write race re-reads the winner's row and returns 200.
pub(crate) async fn get_or_create_system_area(
    State(s): State<RouteState>,
    // `Actor` is extracted so the middleware validates `X-Calm-Actor`, but the event is stamped `ActorId::Kernel`: the system area is kernel-owned scaffolding and a `User` actor would be untruthful.
    _actor: Actor,
    principal: Principal,
) -> Result<(StatusCode, Json<Area>)> {
    require_deployment_owner(&principal)?;
    s.repo
        .application_user_require_registered(&principal.user_id)
        .await?;
    let (status, area) = ensure_system_area(&s).await?;
    Ok((status, Json(area)))
}

pub(crate) async fn ensure_system_area(s: &RouteState) -> Result<(StatusCode, Area)> {
    // Existence check first: the common path avoids opening a write transaction.
    if let Some(existing) = s.repo.area_get_system().await? {
        return Ok((StatusCode::OK, existing));
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
        Ok((area, _id)) => Ok((StatusCode::CREATED, area)),
        // Two cold-boot Today-page loads can both reach the mint; the partial unique index fails the loser's INSERT, so re-read and return 200. Any `Db` error retries the read (sqlx needs an `Any` boundary to downcast); if it was something else the follow-up read returns `None` and propagates.
        Err(e) => match e {
            CalmError::Db(_) => match s.repo.area_get_system().await? {
                Some(existing) => Ok((StatusCode::OK, existing)),
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
        (status = 403, description = "Current Area membership required, or registered deployment owner for system Areas; cross-origin writes are forbidden", body = ErrorBody),
        (status = 400, description = "Unknown default template or invalid attached default folder", body = ErrorBody),
        (status = 404, description = "Area not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn update_area(
    State(s): State<RouteState>,
    actor: Actor,
    principal: Principal,
    Path(id): Path<String>,
    JsonBody(mut p): JsonBody<AreaPatch>,
) -> Result<Json<Area>> {
    // Resource-first: an unknown id 404s before an invalid caller-supplied path can trigger filesystem and `git` probes.
    require_area_access(s.repo.as_ref(), &id, &principal, AreaAccess::Collaborate).await?;
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
                require_area_access_tx(tx, &id, &principal, AreaAccess::Collaborate).await?;
                let area = area_update_tx(tx, &id, p).await?;
                Ok((area.clone(), Event::AreaUpdated(area)))
            })
        },
    )
    .await?;
    Ok(Json(area))
}

struct PreparedAreaDeletion {
    reclaim_ports: crate::workspace_service::reclaim::ServerReclaimPorts,
    id: String,
    area_kind: Option<AreaKind>,
    tracks: Vec<Track>,
    actor: ActorId,
    principal: Principal,
    turn_daemon: std::sync::Arc<crate::shared_codex_appserver::SharedCodexAppServer>,
    thread_seals: std::sync::Arc<crate::thread_seals::ThreadSeals>,
    _area_guard: crate::per_card_lock::KeyedLockGuard,
    _operation_guard: crate::operation::OperationDriveGuard,
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
        // Provider ownership records revoke and record Stop before generic execution
        // quiescence. All area tracks are already fenced against destructive races.
        for track in &self.tracks {
            crate::claude_planner::lifecycle::sweep_track(
                route.repo.as_ref(),
                &route.claude_planner,
                track.id.as_str(),
            )
            .await?;
        }
        crate::execution_quiesce::quiesce(
            route.repo.as_ref(),
            route.operation_runtime.execution().engine(),
            crate::operation::terminal_disposal::Scope::Area(self.id.clone()),
        )
        .await?;
        let mut terminal_ids = Vec::new();
        let mut terminal_card_ids = Vec::new();
        let mut card_ids: HashSet<String> = HashSet::new();
        let mut seals = crate::thread_seals::DeletionThreadSeals::new(route.thread_seals.clone());
        for track in &self.tracks {
            let cards = route.repo.cards_by_track(track.id.as_str()).await?;
            for card in &cards {
                card_ids.insert(card.id.to_string());
                if let Some(thread_id) =
                    quiesce_shared_codex_card_active_turn(route, codex, card).await?
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
            crate::acp_planner::config::sweep_track(
                route.repo.as_ref(),
                &route.acp_planner,
                track.id.as_str(),
            )
            .await?;
            for thread_id in worker
                .harness
                .shutdown_track(&track.id, route.thread_seals.clone())
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
            Ok(Ok(recycle_report)) => Ok(RecycledAreaDeletion {
                quiesced: QuiescedAreaDeletion {
                    prepared,
                    terminal_ids,
                    terminal_card_ids,
                    sealed_thread_ids,
                    card_ids,
                },
                recycle_report,
            }),
            Ok(Err(error)) => {
                if prepared
                    .tracks
                    .iter()
                    .all(workspace_recycle::workspace_allows_runtime_recovery)
                {
                    for thread_id in &sealed_thread_ids {
                        prepared.thread_seals.unseal_after_rollback(thread_id);
                    }
                }
                Err(CalmError::from(error))
            }
            Err(_) => {
                if prepared
                    .tracks
                    .iter()
                    .all(workspace_recycle::workspace_allows_runtime_recovery)
                {
                    for thread_id in &sealed_thread_ids {
                        prepared.thread_seals.unseal_after_rollback(thread_id);
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
    principal: Principal,
    registration: workspace_runtime::sweep::TeardownRegistration,
) -> Result<(
    Vec<workspace_runtime::sweep::WorkspaceTrackSweep>,
    Vec<String>,
)> {
    let scope = EventScope::Area {
        area: id.clone().into(),
    };
    let (((sweeps, deleted_track_ids), retire), _event_ids) = write_with_actor_events_typed(
        route.repo.as_ref(),
        None,
        &route.events,
        &route.write,
        move |tx| {
            Box::pin(async move {
                require_area_access_tx(tx, &id, &principal, AreaAccess::Delete).await?;
                crate::operation::terminal_disposal::require_safe_tx(
                    tx,
                    &crate::operation::terminal_disposal::Scope::Area(id.clone()),
                )
                .await?;
                let mut retire = Vec::new();
                for terminal_id in &terminal_ids {
                    match terminal_delete_tx(tx, terminal_id)
                        .await
                        .map_err(CalmError::from)
                    {
                        Ok(deleted) => retire.push(deleted),
                        Err(CalmError::NotFound(_)) => {}
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
                        registration.register(&sweep);
                        sweeps.push(sweep);
                    }
                }
                area_delete_tx(tx, &id).await?;
                events.push((actor, scope, Event::AreaDeleted { id: id.into() }));
                Ok((((sweeps, deleted_track_ids), retire), events))
            })
        },
    )
    .await?;
    route
        .operation_runtime
        .execution()
        .drive(retire.into_iter().filter_map(|deleted| deleted.retire));
    Ok((sweeps, deleted_track_ids))
}

impl RecycledAreaDeletion {
    async fn commit(
        mut self,
        route: &RouteState,
        registration: workspace_runtime::sweep::TeardownRegistration,
    ) -> Result<()> {
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
                self.quiesced.prepared.principal.clone(),
                registration.clone(),
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
                        .thread_seals
                        .unseal_after_rollback(thread_id);
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
            .thread_seals
            .forget_deleted(&self.quiesced.sealed_thread_ids);
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
        // Membership is committed and recycling finalized; the registry owns the
        // remaining worktree effects, including creates in a recreated area.
        drop(self.quiesced.prepared._area_guard);
        crate::workspace_service::sweep::before_sweep(&sweeps).await;
        sweep_workspace_worktrees_for_tracks(
            registration.sweeps(sweeps),
            self.quiesced.prepared.reclaim_ports.clone(),
            crate::workspace_service::ports::observe,
        )
        .await;
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
    let recovery_thread_seals = deletion.quiesced.prepared.thread_seals.clone();
    let recovery_area_id = deletion.quiesced.prepared.id.clone();
    let registration = route.teardown_targets.registration();
    match std::panic::AssertUnwindSafe(deletion.commit(route, registration.clone()))
        .catch_unwind()
        .await
    {
        Ok(result) => result,
        Err(_) => {
            if route.repo.area_get(&recovery_area_id).await?.is_some() {
                workspace_recycle::restore_area_recycle_report(&recovery_report)?;
                for thread_id in &recovery_thread_ids {
                    recovery_thread_seals.unseal_after_rollback(thread_id);
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
            route.thread_seals.clone(),
            route.planner_wiring(),
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
        (status = 403, description = "Registered Area owner required; system Areas cannot be deleted via REST; cross-origin writes are forbidden", body = ErrorBody),
        (status = 404, description = "Area not found", body = ErrorBody),
        (status = 409, description = "A child track has an active forge action, unresolved terminal launch, or pending provider Stop; retry after settlement", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn delete_area(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    State(cs): State<CodexShellState>,
    actor: Actor,
    principal: Principal,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    // Resolve current ownership under the Area fence before any quiesce/recycle.
    // The final write transaction checks the same policy again.
    // Lock order is area delete → operation drive → sorted track delete; the track-create route takes this area lock before entering the operation driver, so neither side can invert the pair.
    let area_delete_guard = crate::per_card_lock::lock_key(&s.area_delete_locks, &id).await;
    let area = require_area_access(s.repo.as_ref(), &id, &principal, AreaAccess::Delete).await?;
    let area_kind = Some(area.kind);

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
    let pool = w.repo.sqlite_db().ok_or_else(|| {
        CalmError::Internal("delete_area forge-action fence requires sqlite-backed repo".into())
    })?;
    if any_track_has_active_forge_action(pool.read_pool(), &track_ids).await? {
        return Err(CalmError::Conflict(format!(
            "area {id} has a child track with an in-flight forge-action; retry after it settles"
        )));
    }

    for track in &tracks {
        crate::workspace_service::reclaim::protect_delete(
            &mut *pool.read_pool().acquire().await?,
            &track.workspace,
        )
        .await?;
    }

    let prepared = PreparedAreaDeletion {
        reclaim_ports: crate::workspace_service::reclaim::ServerReclaimPorts {
            db: pool.clone(),
            events: s.events.clone(),
        },
        id,
        area_kind,
        tracks,
        actor: actor.to_actor_id(),
        principal,
        turn_daemon: cs.shared_codex_appserver.clone(),
        thread_seals: s.thread_seals.clone(),
        _area_guard: area_delete_guard,
        _operation_guard: operation_guard,
        _track_guards: track_delete_guards,
    };
    finish_prepared_area_deletion_owned(s, w, cs, prepared).await?;
    Ok(StatusCode::NO_CONTENT)
}
