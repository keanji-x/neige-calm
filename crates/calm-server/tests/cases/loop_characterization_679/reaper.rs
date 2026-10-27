use super::*;

/// Stands in for a worker process that launches and then dies without ever reporting.
struct SilentSpawnAdapter {
    spawned: Arc<tokio::sync::Notify>,
    card_id: String,
}

const SILENT_SPAWN_ADAPTER_PHASES: &[PhaseTag] = &[];

#[async_trait]
impl ProviderAdapter for SilentSpawnAdapter {
    fn kind(&self) -> &'static str {
        "terminal-worker"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        SILENT_SPAWN_ADAPTER_PHASES
    }

    async fn validate(&self, _input: &Value) -> CalmResult<()> {
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        _tx: &mut Tx<'tx>,
        _input: &Value,
        _op: &Operation,
    ) -> CalmResult<TxOutput> {
        Ok(TxOutput::new(
            "card",
            Some(self.card_id.clone()),
            json!({ "id": self.card_id }),
        ))
    }

    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<AppServerInteractOutcome> {
        Err(CalmError::Internal(
            "silent-spawn test fixture unexpected app_server_interact".into(),
        ))
    }

    async fn spawn_side_effect(
        &self,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<SpawnOutcome> {
        self.spawned.notify_one();
        Ok(SpawnOutcome::Ready(SpawnHandle::NoOp))
    }

    async fn plan_compensation(
        &self,
        _from_phase: PhaseTag,
        _reason: &str,
        _output: &TxOutput,
        _op: &Operation,
    ) -> CalmResult<CompensationStateVersioned> {
        Err(CalmError::Internal(
            "silent-spawn test fixture must not need compensation".into(),
        ))
    }

    async fn compensate_step(
        &self,
        _step: &calm_server::operation::CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<()> {
        Err(CalmError::Internal(
            "silent-spawn test fixture has no compensation steps".into(),
        ))
    }
}

#[tokio::test]
async fn dead_worker_never_reporting_reaper_converges() {
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(
            LEGACY_OWNER_USER_ID,
            NewArea {
                name: "dead-worker".into(),
                color: "#000".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "dead-worker".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let events = EventBus::new();
    let role_cache = CardRoleCache::new();
    repo.seed_card_role_cache(&role_cache).await.unwrap();
    let track_area_cache = TrackAreaCache::new();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();

    let key = "dead-worker-pin";
    let task_id = format!("{}:{key}", track.id.as_str());
    let now = now_ms();
    let task = Task {
        id: task_id.clone(),
        track_id: track.id.as_str().to_string(),
        key: key.into(),
        kind: TaskKind::Terminal,
        goal: "worker-that-never-reports".into(),
        context_json: "null".into(),
        acceptance_criteria: None,
        cwd: None,
        depends_on_json: "[]".into(),
        priority: 0,
        gate_json: None,
        status: TaskStatus::Pending,
        status_detail: None,
        worker_card_id: None,
        gate_result_json: None,
        gate_attempt: 0,
        gate_pid: None,
        gate_pid_starttime: None,
        gate_pid_boot_id: None,
        running_deadline_ms: None,
        context_stale_at_ms: None,
        declared_by: "spec".into(),
        spawn: "in-wave".into(),
        access: calm_server::model::TaskAccess::ReadWrite,
        start: calm_server::model::TaskStart::Checkout,
        result_contract: None,
        child_track_json: None,
        created_at_ms: now,
        updated_at_ms: now,
        finished_at_ms: None,
    };
    let pool = repo.sqlite_db().expect("sqlite-backed repo");
    crate::support::task::project_task(pool.read_pool(), &task)
        .await
        .unwrap();

    // The projection a real spawn would have left behind.
    let worker_card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: json!({"terminal_presentation": {"source": "task", "in_workbench": false}, "idempotency_key": task_id}),
        })
        .await
        .unwrap();
    role_cache.insert(worker_card.id.clone(), CardRole::Worker, track.id.clone());

    let spawned = Arc::new(tokio::sync::Notify::new());
    let operation_repo = Arc::new(SqlxOperationRepo::new(
        repo.sqlite_db().expect("sqlite-backed repo"),
    ));
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
    let completion = OperationCompletionBus::new();
    let spawn_ctx = SpawnCtx::new(
        route_repo,
        operation_repo.clone(),
        Arc::new(DaemonClient {
            data_dir: PathBuf::from("/tmp/neige-loop-pin-dead-worker"),
            proc_supervisor_sock: None,
        }),
        terminal_renderer,
        events.clone(),
        completion.clone(),
    );
    let operation_runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo,
        vec![Arc::new(SilentSpawnAdapter {
            spawned: spawned.clone(),
            card_id: worker_card.id.to_string(),
        })],
        events.clone(),
        completion,
        spawn_ctx,
    ));
    let dispatcher = Dispatcher::spawn_with_operation_runtime(
        repo.clone(),
        events.clone(),
        WriteContext::new(role_cache.clone(), track_area_cache.clone()),
        Arc::new(CodexClient::new_stub()),
        Arc::new(DaemonClient {
            data_dir: PathBuf::from("/tmp/neige-loop-pin-dead-worker"),
            proc_supervisor_sock: None,
        }),
        None,
        SharedCodexAppServer::new_stub(repo.clone()),
        operation_runtime,
        4,
    );

    dispatcher
        .scheduler()
        .schedule_track(track.id.clone())
        .await;

    // Positive sync point: the worker "spawn" ran.
    tokio::time::timeout(Duration::from_secs(5), spawned.notified())
        .await
        .expect("silent worker spawn must run within 5s");

    let baseline_id = repo
        .events_since(0, i64::MAX)
        .await
        .unwrap()
        .last()
        .map(|(id, _version, _scope, _event)| *id)
        .unwrap_or(0);
    let op_id: String = sqlx::query_scalar(
        "SELECT id FROM operations WHERE kind = 'terminal-worker' AND idempotency_key = ?1 \
         ORDER BY created_at_ms DESC, id DESC LIMIT 1",
    )
    .bind(&task_id)
    .fetch_one(pool.read_pool())
    .await
    .expect("worker operation row");

    let session_id = WorkerSessionId::from(new_id());
    let session_now = now_ms();
    let mut tx = pool.writer_pool().begin().await.unwrap();
    session_insert_tx(
        &mut tx,
        WorkerSession {
            id: session_id.clone(),
            track_id: track.id.clone(),
            provider: WorkerProviderKind::Terminal,
            mode: SessionMode::Ephemeral,
            contract: WorkerContract::Executor,
            parent_session_id: None,
            requester_session_id: None,
            state: WorkerSessionState::Running,
            mcp_token_hash: None,
            thread_id: None,
            agent_session_id: None,
            active_turn_id: None,
            terminal_run_id: None,
            card_id: Some(worker_card.id.clone()),
            handle_state_json: None,
            liveness: LivenessTag::Unknown,
            liveness_probed_at_ms: None,
            exit_code: None,
            exit_interpretation: None,
            spawn_op_id: Some(op_id),
            last_activity_ms: None,
            last_thread_status: None,
            created_at_ms: session_now,
            updated_at_ms: session_now,
            completed_at_ms: None,
        },
    )
    .await
    .unwrap();
    sqlx::query("UPDATE cards SET session_id = ?1 WHERE id = ?2")
        .bind(session_id.as_str())
        .bind(worker_card.id.as_str())
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let fake = Arc::new(FakeProvider::new().with_probe_script([Liveness::Exited {
        evidence: ExitEvidence {
            exit_code: Some(-1),
            signal_killed: false,
            observed_at_ms: now_ms(),
            source: ExitSource::Probe,
        },
    }]));
    let registry = WorkerProviderRegistry::from_entries([(
        WorkerProviderKind::Terminal,
        fake as Arc<dyn WorkerProvider>,
    )]);
    let reaper = Reaper::new(
        repo.clone(),
        registry,
        events.clone(),
        WriteContext::new(role_cache.clone(), track_area_cache.clone()),
    );
    reaper_on_boot();
    reaper.sweep_all().await;

    // Exactly one kernel task.failed.
    let rows = repo.events_since(baseline_id, i64::MAX).await.unwrap();
    let mut failed_events = Vec::new();
    for (id, _version, scope, event) in rows {
        if scope.track_id() != Some(&track.id) {
            continue;
        }
        match event {
            Event::TaskFailed {
                ref idempotency_key,
                ..
            } if idempotency_key == &task_id => {
                let actor_text: String =
                    sqlx::query_scalar("SELECT actor FROM events WHERE id = ?1")
                        .bind(id)
                        .fetch_one(pool.read_pool())
                        .await
                        .unwrap();
                let actor: ActorId = serde_json::from_str(&actor_text).unwrap();
                assert_eq!(actor, ActorId::KernelDispatcher);
                failed_events.push(event);
            }
            Event::TaskCompleted {
                idempotency_key, ..
            } if idempotency_key == task_id => {
                panic!("dead-worker reaper must not emit task.completed")
            }
            _ => {}
        }
    }
    assert_eq!(failed_events.len(), 1, "exactly one reaper task.failed");
    match &failed_events[0] {
        Event::TaskFailed {
            idempotency_key,
            reason,
            agent_message,
            ..
        } => {
            assert_eq!(idempotency_key, &task_id);
            // The `-1` probe sentinel is hidden behind "outcome unknown", not leaked.
            assert!(
                reason.contains("outcome unknown") && reason.contains("supervisor probe"),
                "expected provider reason, got {reason:?}"
            );
            assert!(!reason.contains("exit Some(-1)"));
            assert_eq!(agent_message, &None);
        }
        other => panic!("expected task.failed, got {other:?}"),
    }

    let task_row = repo.task_get(&task_id).await.unwrap().expect("task exists");
    assert_eq!(task_row.status, TaskStatus::Failed);
    // Classifier + reason tail (the reaper path).
    let detail = task_row.status_detail.clone().unwrap_or_default();
    assert_eq!(
        calm_server::db::sqlite::status_detail_class(&detail),
        "spawn-failed"
    );
    let session = repo
        .session_get(&session_id)
        .await
        .unwrap()
        .expect("session exists");
    assert_eq!(session.state, WorkerSessionState::Failed);
    assert_eq!(session.exit_code, Some(-1));

    let cards = repo.cards_by_track(track.id.as_str()).await.unwrap();
    assert!(
        cards.iter().any(|c| c.id == worker_card.id),
        "dead-worker convergence does not reap the worker card"
    );
}
