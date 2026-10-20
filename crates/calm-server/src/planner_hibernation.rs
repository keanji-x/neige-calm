//! Closed-track Planner lifecycle. The durable carrier stays active; only its live
//! harness and provider subscription sleep. Explicit wake callers own restoration.
use crate::error::Result;
use crate::ids::CardId;
use crate::operation::planner_start_fence::CardStartFence;
use crate::per_card_lock::lock_key;
use crate::state::AppState;
use axum::extract::FromRef;
use sqlx::Row;

/// Work that prevents both Admin close and closed-Planner hibernation. SQL closed set owned here.
pub(crate) const RUNNING_WORK_STATUSES_SQL: &str = "('dispatched','running','verifying')";

/// Cheap lifecycle candidates. The harness input owner interprets the persisted
/// checkpoint, and its original loop rechecks live eligibility before quiescence.
const ELIGIBLE_SQL_PREFIX: &str = "SELECT ws.id, ws.card_id, ws.track_id, ws.handle_state_json \
    FROM worker_sessions ws JOIN cards c ON c.session_id=ws.id AND c.track_id=ws.track_id \
    JOIN tracks t ON t.id=ws.track_id \
    WHERE c.role='planner' AND ws.contract='planner' AND ws.mode='resumable' \
      AND ws.state='idle' AND ws.completed_at_ms IS NULL \
      AND ws.terminal_run_id IS NULL AND ws.active_turn_id IS NULL AND t.closed_at IS NOT NULL \
      AND json_extract(ws.handle_state_json,'$.mode')='harness' \
      AND NOT EXISTS (SELECT 1 FROM current_tasks ct WHERE ct.track_id=t.id \
                       AND ct.status IN ";

fn eligible_sql() -> String {
    format!("{ELIGIBLE_SQL_PREFIX}{RUNNING_WORK_STATUSES_SQL})")
}

pub(crate) async fn eligible(pool: &sqlx::SqlitePool, session_id: &str) -> Result<bool> {
    let sql = format!("{} AND ws.id=?1", eligible_sql());
    let row = sqlx::query(&sql)
        .bind(session_id)
        .fetch_optional(pool)
        .await?;
    match row {
        Some(row) => snapshot_idle(&row),
        None => Ok(false),
    }
}

fn snapshot_idle(row: &sqlx::sqlite::SqliteRow) -> Result<bool> {
    let snapshot: String = row.try_get("handle_state_json")?;
    crate::harness::HarnessRuntimeSnapshot::idle_for_lifecycle(serde_json::from_str(&snapshot)?)
}

pub(crate) async fn sweep(state: &AppState) -> Result<()> {
    let Some(pool) = state.sqlite_db() else {
        return Ok(());
    };
    for row in sqlx::query(&eligible_sql())
        .fetch_all(pool.read_pool())
        .await?
    {
        let id: String = row.get("id");
        let card: String = row.get("card_id");
        let track: String = row.get("track_id");
        if let Err(error) = hibernate_one(
            state,
            pool.read_pool(),
            id.clone(),
            CardId::from(card),
            track,
        )
        .await
        {
            tracing::warn!(worker_session_id=%id, %error, "Planner hibernation or provider release failed");
        }
    }
    Ok(())
}

async fn hibernate_one(
    state: &AppState,
    pool: &sqlx::SqlitePool,
    id: String,
    card: CardId,
    track: String,
) -> Result<()> {
    let s = crate::state::RouteState::from_ref(state);
    // Already sleeping rows must not queue behind global operation activity.
    let Some(runtime) = s.repo.session_projection_by_id(&id).await? else {
        return Ok(());
    };
    let thread = crate::harness::effective_runtime_thread_id(&runtime);
    let needs_release = |thread: &Option<String>| {
        thread.as_deref().is_some_and(|thread| {
            state
                .shared_codex_appserver
                .cached_card_for_thread(thread)
                .is_some()
        })
    };
    if s.harness.get(&id).is_none() && !needs_release(&thread) {
        return Ok(());
    }
    let _card = CardStartFence::lock(
        &s.planner_recovery_locks,
        &s.repo,
        &s.operation_runtime,
        &card,
    )
    .await;
    // Lock order: card -> drive -> track -> resume_replay_serial (release).
    let operations = s.operation_runtime.lock_for_track_delete().await;
    let _track = lock_key(&s.track_delete_locks, &track).await;
    if !eligible(pool, &id).await? {
        return Ok(());
    }
    let Some(runtime) = s.repo.session_projection_by_id(&id).await? else {
        return Ok(());
    };
    let thread = crate::harness::effective_runtime_thread_id(&runtime);
    let handle = s.harness.get(&id);
    if handle.is_none() && !needs_release(&thread) {
        return Ok(());
    }
    if let Some(handle) = &handle {
        if !handle.hibernate_if_idle().await? {
            return Ok(());
        }
        s.harness.remove_if_same(&id, handle);
    }
    // Closing input ends ownership. Provider cleanup is fallible and must never
    // leave an input-closed handle installed or hold the global drive mutex.
    drop(operations);
    release_quiesced(
        handle.as_ref(),
        &runtime,
        thread,
        &state.shared_codex_appserver,
    )
    .await
}

/// Release what a quiesced owner held, once its input is closed, it has left the registry and
/// the drive mutex is dropped. The Planner is then an ordinary sleeping one, which the shared
/// wake path restores on the next send or observation.
pub(crate) async fn release_quiesced(
    handle: Option<&crate::harness::PlannerHarness>,
    runtime: &crate::session_projection_repo::WorkerSessionProjection,
    thread: Option<String>,
    daemon: &crate::shared_codex_appserver::SharedCodexAppServer,
) -> Result<()> {
    if let Some(handle) = handle {
        handle.release_hibernated_resources().await?;
    }
    if runtime.agent_provider == Some(crate::session_projection_repo::AgentProvider::Codex)
        && let Some(thread) = thread
    {
        daemon.release_hibernating_thread(&thread).await;
    }
    Ok(())
}

pub(crate) async fn sleeping_thread(pool: &sqlx::SqlitePool, thread: &str) -> Result<bool> {
    let sql = format!("{} AND ws.thread_id=?1", eligible_sql());
    for row in sqlx::query(&sql).bind(thread).fetch_all(pool).await? {
        if snapshot_idle(&row)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Shared restoration owner for sends, Dispatcher observations and reopen.
/// Dispatcher tasks can outlive AppState: runtime/provider owners are weak so
/// this context cannot retain their resources after the application is dropped.
#[derive(Clone)]
pub(crate) struct WakeContext {
    canonical_adapter: std::sync::Weak<
        crate::operation::planner_harness_start_adapter::PlannerHarnessStartAdapter,
    >,
    workspace_root: std::path::PathBuf,
    historical_source: Option<crate::conversation_inputs::HistoricalHumanSource>,
    repo: std::sync::Arc<dyn crate::db::Repo>,
    events: crate::event::EventBus,
    write: crate::state::WriteContext,
    operation_runtime: std::sync::Weak<crate::operation::OperationRuntime>,
    planner_recovery_locks: crate::per_card_lock::PerCardLocks,
    track_delete_locks: crate::per_card_lock::KeyedLocks,
    harness: crate::harness::HarnessRegistry,
    daemon: std::sync::Weak<crate::shared_codex_appserver::SharedCodexAppServer>,
    thread_seals: std::sync::Arc<crate::thread_seals::ThreadSeals>,
    providers: crate::harness::wiring::PlannerWiring,
}

impl WakeContext {
    pub(crate) fn new(
        route: crate::state::RouteState,
        repo: std::sync::Arc<dyn crate::db::Repo>,
        shell: crate::state::CodexShellState,
    ) -> Self {
        Self {
            canonical_adapter: std::sync::Arc::downgrade(&route.planner_start_adapter),
            workspace_root: route.workspace_root.clone(),
            historical_source: None,
            repo,
            events: route.events.clone(),
            write: route.write.clone(),
            operation_runtime: std::sync::Arc::downgrade(&route.operation_runtime),
            planner_recovery_locks: route.planner_recovery_locks.clone(),
            track_delete_locks: route.track_delete_locks.clone(),
            harness: route.harness.clone(),
            daemon: std::sync::Arc::downgrade(&shell.shared_codex_appserver),
            thread_seals: route.thread_seals.clone(),
            providers: route.planner_wiring(),
        }
    }

    pub(crate) async fn wake_canonical(
        &self,
        card: &CardId,
        source: crate::conversation_inputs::InputSource,
    ) -> Result<()> {
        source.check_current()?;
        let runtime = self.operation_runtime.upgrade().ok_or_else(|| {
            crate::error::CalmError::ServiceUnavailable("application runtime stopped".into())
        })?;
        let adapter = self.canonical_adapter.upgrade().ok_or_else(|| {
            crate::error::CalmError::ServiceUnavailable("application start owner stopped".into())
        })?;
        let repo: std::sync::Arc<dyn crate::db::RouteRepo> = self.repo.clone();
        let fence = CardStartFence::lock(&self.planner_recovery_locks, &repo, &runtime, card).await;
        source.check_current()?;
        fence
            .wake_retained(
                adapter,
                source,
                self.workspace_root.clone(),
                self.events.clone(),
            )
            .await
    }

    pub(crate) fn with_historical_source(
        mut self,
        source: Option<crate::conversation_inputs::HistoricalHumanSource>,
    ) -> Self {
        self.historical_source = source;
        self
    }

    pub(crate) async fn ensure(
        &self,
        card: &CardId,
    ) -> Result<Option<(crate::harness::PlannerHarness, CardStartFence)>> {
        let s = self;
        let operation_runtime = s.operation_runtime.upgrade().ok_or_else(|| {
            crate::error::CalmError::ServiceUnavailable("application runtime stopped".into())
        })?;
        let route_repo: std::sync::Arc<dyn crate::db::RouteRepo> = self.repo.clone();
        let fence = CardStartFence::lock(
            &s.planner_recovery_locks,
            &route_repo,
            &operation_runtime,
            card,
        )
        .await;
        let Some(runtime) = s
            .repo
            .session_projection_active_for_card(&card.to_string())
            .await?
        else {
            return Ok(None);
        };
        if let Some(handle) = s.harness.get(&runtime.id) {
            return Ok(Some((handle, fence)));
        }
        if runtime.status == crate::session_projection_repo::WorkerSessionState::Starting
            || !runtime
                .handle_state_json
                .as_ref()
                .is_some_and(crate::harness::is_harness_snapshot_value)
        {
            return Ok(None);
        }
        Ok(Some((self.install(runtime).await?, fence)))
    }

    #[allow(deprecated)]
    pub(crate) async fn install(
        &self,
        runtime: crate::session_projection_repo::WorkerSessionProjection,
    ) -> Result<crate::harness::PlannerHarness> {
        use calm_types::runtime::AgentProtocol;
        crate::conversation_inputs::require_historical_storage(
            self.repo.as_ref(),
            &runtime.card_id,
        )
        .await?;
        let s = self;
        if let Some(source) = &s.historical_source {
            source
                .require_card(s.repo.as_ref(), &runtime.card_id)
                .await?;
        }
        match runtime.agent_provider.as_ref() {
            Some(provider) if provider.protocol() == AgentProtocol::Codex => {
                let card =
                    s.repo.card_get(&runtime.card_id).await?.ok_or_else(|| {
                        crate::error::CalmError::NotFound("conversation card".into())
                    })?;
                let _track = lock_key(&s.track_delete_locks, card.track_id.as_str()).await;
                let track = s
                    .repo
                    .track_get(card.track_id.as_str())
                    .await?
                    .ok_or_else(|| {
                        crate::error::CalmError::NotFound("conversation track".into())
                    })?;
                if !workspace_runtime::recycle::workspace_allows_runtime_recovery(&track) {
                    return Err(crate::error::CalmError::Conflict(
                        "Restore this conversation's workspace before resuming".into(),
                    ));
                }
                crate::harness::backend::factory::PlannerFactory::new(
                    s.daemon.clone(),
                    &s.providers.claude,
                    &s.providers.acp,
                )
                .resume_active_conversation(s.repo.as_ref(), &runtime, s.historical_source.clone())
                .await?;
            }
            Some(provider) => {
                crate::harness::backend::factory::PlannerFactory::new(
                    s.daemon.clone(),
                    &s.providers.claude,
                    &s.providers.acp,
                )
                .ready(
                    provider,
                    crate::harness::backend::factory::ReadinessFor::RestoreOrSend,
                )
                .await?;
            }
            None => {
                return Err(crate::error::CalmError::Conflict(
                    "conversation provider is missing".into(),
                ));
            }
        }
        self.install_resumed(runtime).await
    }

    #[allow(deprecated)]
    pub(crate) async fn install_resumed(
        &self,
        runtime: crate::session_projection_repo::WorkerSessionProjection,
    ) -> Result<crate::harness::PlannerHarness> {
        let s = self;
        crate::harness::spawn_recovered_harness_admitted(
            self.repo.clone(),
            s.events.clone(),
            s.write.role_cache().clone(),
            s.write.area_cache().clone(),
            s.daemon.upgrade().ok_or_else(|| {
                crate::error::CalmError::ServiceUnavailable("application daemon stopped".into())
            })?,
            s.thread_seals.clone(),
            &s.providers,
            &s.harness,
            &s.track_delete_locks,
            runtime,
            crate::harness::ClaimMode::Replace,
            s.historical_source.clone(),
        )
        .await?
        .installed()
        .ok_or_else(|| {
            crate::error::CalmError::Conflict("conversation could not be restored".into())
        })
    }
}
