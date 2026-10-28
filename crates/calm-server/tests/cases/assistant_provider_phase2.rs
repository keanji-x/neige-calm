//! The real driver must preserve the transaction's entire raced predecessor journal.
use super::*;
use async_trait::async_trait;
use calm_server::db::sqlite::session_set_handle_state_tx;
use calm_server::error::{CalmError, Result as CalmResult};
use calm_server::harness::QueueEntry;
use calm_server::mcp_server::{McpServer, build_default_registry};
use calm_server::operation::planner_harness_start_adapter::PlannerHarnessStartAdapter;
use calm_server::operation::{
    AppServerInteractKind, AppServerInteractOutcome, CompensationStateVersioned, CompensationStep,
    Operation, OperationCompletionBus, OperationRepo, OperationRuntime, ProviderAdapter, SpawnCtx,
    SpawnOutcome, SqlxOperationRepo, Tx,
};

struct RacedStart {
    inner: PlannerHarnessStartAdapter,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    observed: Arc<tokio::sync::Mutex<Option<TxOutput>>>,
    entered_once: std::sync::atomic::AtomicBool,
    checkpoint: Arc<tokio::sync::Notify>,
    checkpoint_pause: bool,
    checkpoint_once: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl ProviderAdapter for RacedStart {
    fn kind(&self) -> &'static str {
        self.inner.kind()
    }

    fn phases(&self) -> &'static [PhaseTag] {
        self.inner.phases()
    }

    fn app_server_interact_kind(
        &self,
        output: &TxOutput,
        op: &Operation,
    ) -> CalmResult<AppServerInteractKind> {
        self.inner.app_server_interact_kind(output, op)
    }

    async fn validate(&self, input: &Value) -> CalmResult<()> {
        self.inner.validate(input).await
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        op: &Operation,
    ) -> CalmResult<TxOutput> {
        self.inner.prepare_tx(tx, input, op).await
    }

    async fn app_server_interact(
        &self,
        output: &mut TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> CalmResult<AppServerInteractOutcome> {
        if !self
            .entered_once
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            self.entered.notify_one();
            self.release.notified().await;
        }
        let result = self.inner.app_server_interact(output, op, ctx).await;
        if self.checkpoint_pause
            && !self
                .checkpoint_once
                .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            self.checkpoint.notify_one();
            std::future::pending::<()>().await;
        }
        let committed = ctx
            .operation_repo
            .get_operation(&op.id)
            .await?
            .unwrap()
            .tx_output
            .unwrap();
        assert_eq!(
            serde_json::to_value(output).unwrap(),
            serde_json::to_value(&committed).unwrap(),
            "postcommit delivery must equal the complete durable checkpoint"
        );
        *self.observed.lock().await = Some(committed);
        result
    }

    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> CalmResult<SpawnOutcome> {
        let persisted = ctx.operation_repo.get_operation(&op.id).await?.unwrap();
        assert_eq!(persisted.phase.tag(), PhaseTag::SpawnStarted);
        assert_eq!(
            serde_json::to_value(persisted.tx_output.as_ref().unwrap()).unwrap(),
            serde_json::to_value(output).unwrap()
        );
        assert_eq!(
            serde_json::to_value(persisted.tx_output.as_ref().unwrap()).unwrap(),
            serde_json::to_value(self.observed.lock().await.as_ref().unwrap()).unwrap(),
            "SpawnStarted must retain the complete committed checkpoint"
        );
        Err(CalmError::Internal("test phase2 spawn failure".into()))
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        output: &TxOutput,
        op: &Operation,
    ) -> CalmResult<CompensationStateVersioned> {
        self.inner
            .plan_compensation(from_phase, reason, output, op)
            .await
    }

    async fn compensate_step(
        &self,
        step: &CompensationStep,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> CalmResult<()> {
        self.inner.compensate_step(step, output, op, ctx).await
    }
}

#[tokio::test]
async fn assistant_provider_phase2_raced_with_prepare_checkpoint() {
    raced_start(true, false, 0).await;
}
#[tokio::test]
async fn assistant_provider_phase2_raced_without_prepare_checkpoint() {
    raced_start(false, false, 0).await;
}
#[tokio::test]
async fn assistant_provider_phase2_postcommit_error_uses_committed_state() {
    raced_start(true, true, 0).await;
}

#[tokio::test]
async fn assistant_provider_phase2_replay_before_checkpoint() {
    raced_start(true, false, 1).await;
}
#[tokio::test]
async fn assistant_provider_phase2_replay_after_checkpoint() {
    raced_start(true, false, 2).await;
}

async fn raced_start(prepare_checkpoint: bool, postcommit_error: bool, crash: u8) {
    let _guard = ENV_LOCK.lock().await;
    let tmp = TempDir::new().unwrap();
    let capture = tmp.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture);
    }
    let _env = EnvGuard("FAKE_CODEX_CAPTURE_REQUESTS");
    let (state, repo, roles) = state_with_live_daemon(&tmp).await;
    let track = seed_track(&repo).await;
    state
        .track_area_cache
        .insert(track.id.clone(), track.area_id.clone());
    let card_id = new_id();
    seed_assistant_card(&repo, &roles, &track, &card_id).await;
    let mut tx = repo.pool().begin().await.unwrap();
    let report = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "track-report".into(),
            sort: None,
            payload: serde_json::to_value(calm_server::track_report::TrackReportPayload::initial())
                .unwrap(),
        },
        CardRole::ReportCard,
        false,
        &roles,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    crate::report_writes::announce_report(repo.as_ref(), &state.events, state.write(), report)
        .await;
    let mut payload = serde_json::to_value(terminal_approval::start_payload(
        &track,
        &card_id,
        HarnessProfile::Assistant,
        true,
        None,
    ))
    .unwrap();
    payload["version"] = json!("assistant-v1");
    payload["provider"] = json!("codex");
    payload["reset_harness_items"] = json!(postcommit_error);
    if prepare_checkpoint {
        let a = state
            .operation_runtime
            .submit("planner-harness-start", key(), payload.clone())
            .await
            .unwrap();
        assert!(matches!(
            wait_op(&state, &a).await,
            OperationOutcome::Succeeded { .. }
        ));
    }
    let wiring = state.planner_wiring();
    let inner = PlannerHarnessStartAdapter::new(
        repo.clone(),
        calm_server::agent_backend_pool::codex::CodexBackends::single(
            state.shared_codex_appserver.clone(),
        ),
        state.thread_seals().clone(),
        state.harness.clone(),
        state.plugin.clone(),
        roles.clone(),
        state.track_area_cache.clone(),
        None,
        wiring.claude,
        wiring.acp,
    );
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let observed = Arc::new(tokio::sync::Mutex::new(None));
    let checkpoint = Arc::new(tokio::sync::Notify::new());
    let operations = Arc::new(SqlxOperationRepo::new(repo.db().clone()));
    let completion = OperationCompletionBus::new();
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operations.clone(),
        vec![Arc::new(RacedStart {
            inner,
            entered: entered.clone(),
            release: release.clone(),
            observed: observed.clone(),
            entered_once: std::sync::atomic::AtomicBool::new(false),
            checkpoint: checkpoint.clone(),
            checkpoint_pause: crash == 2,
            checkpoint_once: std::sync::atomic::AtomicBool::new(false),
        })],
        state.events.clone(),
        completion.clone(),
        SpawnCtx::new(
            repo.clone(),
            operations.clone(),
            state.daemon.clone(),
            state.terminal_renderer.clone(),
            state.events.clone(),
            completion,
        ),
    ));
    let submit_runtime = runtime.clone();
    let submit_payload = payload.clone();
    let submit_key = key();
    let lookup_key = submit_key.operation_key.clone();
    let submit = tokio::spawn(async move {
        submit_runtime
            .submit("planner-harness-start", submit_key, submit_payload)
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    let op: String = sqlx::query_scalar("SELECT id FROM operations WHERE operation_key=?1")
        .bind(&lookup_key)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    let prepared = operations.get_operation(&op).await.unwrap().unwrap();
    let prepared_output = prepared.tx_output.unwrap();
    let placeholder = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .unwrap();
    assert!(placeholder.thread_id.is_none());
    assert_eq!(
        !prepared_output.data["old_codex_credential"].is_null(),
        prepare_checkpoint
    );
    // A second production operation takes the slot while the redriven first operation is paused.
    // Notifications are one-shot facts; neither operation policy nor checkpoint construction is copied here.
    let b = state
        .operation_runtime
        .submit("planner-harness-start", key(), payload.clone())
        .await
        .unwrap();
    assert!(matches!(
        wait_op(&state, &b).await,
        OperationOutcome::Succeeded { .. }
    ));
    let racer = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(racer.kind, WorkerSessionKind::ManagedAssistant);
    state.harness.shutdown_and_remove(&racer.id).await.unwrap();
    let raw = std::fs::read_to_string(&capture).unwrap();
    let rows: Vec<Value> = raw
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let token = thread_start_token(
        rows.iter()
            .rev()
            .find(|r| r["method"] == "thread/start")
            .unwrap(),
    )
    .to_string();
    let hash = auth::hash_token(&token);
    assert_eq!(
        assert_card_session_mcp_hash_parity(&repo, &card_id, &racer.id).await,
        hash
    );
    let mut snapshot = HarnessSnapshot::from_value_strict(racer.handle_state_json.clone().unwrap());
    snapshot.set_pending_entries(vec![QueueEntry::user_message(
        "racer owed sentence".into(),
        None,
        vec![],
    )]);
    let mut tx = repo.pool().begin().await.unwrap();
    session_set_handle_state_tx(
        &mut tx,
        &racer.id,
        Some(serde_json::to_value(&snapshot).unwrap()),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    if postcommit_error {
        sqlx::query(
            r#"CREATE TRIGGER fail_transcript_event BEFORE INSERT ON events
               WHEN NEW.kind='harness.transcript.cleared'
               BEGIN
                 SELECT RAISE(ABORT, 'phase2 postcommit event failure');
               END"#,
        )
        .execute(repo.pool())
        .await
        .unwrap();
    }
    if crash != 1 {
        release.notify_one();
    }
    if crash == 2 {
        tokio::time::timeout(Duration::from_secs(10), checkpoint.notified())
            .await
            .unwrap();
        let committed = operations.get_operation(&op).await.unwrap().unwrap();
        let checkpoint_output = committed.tx_output.unwrap();
        assert_eq!(
            checkpoint_output.data["old_codex_credential"]["worker_session_id"],
            json!(racer.id)
        );
    }
    if crash != 0 {
        submit.abort();
        assert!(submit.await.unwrap_err().is_cancelled());
        let plan = runtime.recover_on_boot().await.unwrap();
        runtime.apply_recovery(plan).await.unwrap();
    } else {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(15), submit)
                .await
                .unwrap()
                .unwrap(),
            op
        );
    }
    let outcome = tokio::time::timeout(Duration::from_secs(15), runtime.wait(&op))
        .await
        .unwrap()
        .unwrap()
        .outcome;
    match outcome {
        OperationOutcome::Failed {
            from_phase,
            last_error,
            ..
        } => {
            assert_eq!(
                from_phase,
                if postcommit_error {
                    PhaseTag::AppServerInteract
                } else {
                    PhaseTag::SpawnStarted
                }
            );
            assert!(
                last_error.contains(if postcommit_error {
                    "phase2 postcommit event failure"
                } else {
                    "test phase2 spawn failure"
                }),
                "{last_error}"
            );
        }
        other => panic!("{other:?}"),
    }
    let failed = operations.get_operation(&op).await.unwrap().unwrap();
    let output = if postcommit_error {
        failed.tx_output.as_ref().unwrap().clone()
    } else {
        observed
            .lock()
            .await
            .clone()
            .expect("driver reached SpawnStarted")
    };
    assert_eq!(output.data["old_runtime_id"], json!(racer.id));
    assert_eq!(
        output.data["old_runtime_status"],
        serde_json::to_value(racer.status).unwrap()
    );
    assert_eq!(
        output.data["old_codex_credential"]["worker_session_id"],
        json!(racer.id)
    );
    assert!(
        output.data["harvested_from"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["worker_session_id"] == racer.id)
    );
    let restored = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restored.id, racer.id);
    assert_eq!(restored.thread_id, racer.thread_id);
    assert_eq!(restored.status, racer.status);
    assert_eq!(
        card_session_id(&repo, &card_id).await.as_deref(),
        Some(racer.id.as_str())
    );
    assert_eq!(
        assert_card_session_mcp_hash_parity(&repo, &card_id, &racer.id).await,
        hash
    );
    let restored_snapshot = HarnessSnapshot::from_value_strict(restored.handle_state_json.unwrap());
    let entries = restored_snapshot.pending_entries();
    assert_eq!(entries.len(), 1);
    let mut actual = serde_json::to_value(&restored_snapshot).unwrap();
    let mut expected = serde_json::to_value(&snapshot).unwrap();
    for value in [&mut actual, &mut expected] {
        for meta in value["pending_entry_meta"].as_array_mut().unwrap() {
            meta.as_object_mut().unwrap().remove("queued_at_ms");
        }
    }
    for key in [
        "pending_queue",
        "pending_entry_meta",
        "pending_envelope_ids",
        "pending_message_ids",
    ] {
        assert_eq!(actual[key], expected[key], "{key}");
    }
    let stamp: Option<i64> =
        sqlx::query_scalar("SELECT queue_harvested_at_ms FROM worker_sessions WHERE id=?1")
            .bind(&racer.id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert!(
        stamp.is_none(),
        "undo must allow the restored queue to be harvested once later"
    );
    let next_id = &placeholder.id;
    let next_hash: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?1")
            .bind(next_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert!(next_hash.is_none());
    let socket_dir = calm_test_sockets::socket_dir("assistant-phase2");
    let socket = calm_test_sockets::socket_path(socket_dir.path(), "kernel.sock");
    let server = McpServer::spawn(
        repo.clone(),
        state.events.clone(),
        state.write().clone(),
        socket.clone(),
        PathBuf::from("/nonexistent-shim"),
        build_default_registry(),
        None,
        Arc::new(tokio::sync::OnceCell::new()),
        Arc::new(tokio::sync::OnceCell::new()),
        tmp.path().join("gates"),
    )
    .await
    .unwrap();
    let read = mcp::call_tool_card_bound(&socket, &token, "neige_report_read", json!({})).await;
    assert!(read.get("error").is_none(), "{read}");
    let prose = mcp::call_tool_card_bound(&socket, &token, "neige_report_commit", json!({
        "message":"racer restored", "ops":[{"op":"upsert","kind":"prose","markdown":"racer native token restored"}]
    })).await;
    assert!(prose.get("error").is_none(), "{prose}");
    // Replay an actual driver-produced undo after its effects committed. No synthesized checkpoint.
    let journal: CompensationStateVersioned =
        serde_json::from_value(failed.compensation_state.clone().unwrap()).unwrap();
    let mut restore = journal
        .steps
        .into_iter()
        .find(|step| step.op == "restore_old_runtime")
        .unwrap();
    restore.completed = false;
    let wiring = state.planner_wiring();
    let adapter = PlannerHarnessStartAdapter::new(
        repo.clone(),
        calm_server::agent_backend_pool::codex::CodexBackends::single(
            state.shared_codex_appserver.clone(),
        ),
        state.thread_seals().clone(),
        state.harness.clone(),
        state.plugin.clone(),
        roles.clone(),
        state.track_area_cache.clone(),
        None,
        wiring.claude,
        wiring.acp,
    );
    let ctx = SpawnCtx::new(
        repo.clone(),
        operations,
        state.daemon.clone(),
        state.terminal_renderer.clone(),
        state.events.clone(),
        OperationCompletionBus::new(),
    );
    adapter
        .compensate_step(&restore, &output, &failed, &ctx)
        .await
        .unwrap();
    // An explicit rotation invalidates the old journal and native token.
    let mut tx = repo.pool().begin().await.unwrap();
    let rotated = calm_server::mcp_server::wiring::mint_and_persist_managed_planner_token(
        &mut tx, &card_id, &racer.id,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(
        adapter
            .compensate_step(&restore, &output, &failed, &ctx)
            .await
            .is_err()
    );
    assert_native_token_rejected(&repo, &token).await;
    // Revocation must not restore even the rotated credential through a stale undo.
    let mut tx = repo.pool().begin().await.unwrap();
    sqlx::query("UPDATE worker_sessions SET mcp_token_hash=NULL WHERE id=?1")
        .bind(&racer.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(
        adapter
            .compensate_step(&restore, &output, &failed, &ctx)
            .await
            .is_err()
    );
    assert_native_token_rejected(&repo, &rotated).await;
    if postcommit_error {
        sqlx::query("DROP TRIGGER fail_transcript_event")
            .execute(repo.pool())
            .await
            .unwrap();
    }
    let mut successor_key = key();
    successor_key.idempotency_key = Some(new_id());
    let successor = state
        .operation_runtime
        .submit(
            "planner-harness-start",
            successor_key.clone(),
            payload.clone(),
        )
        .await
        .unwrap();
    assert!(matches!(
        wait_op(&state, &successor).await,
        OperationOutcome::Succeeded { .. }
    ));
    assert!(
        adapter
            .compensate_step(&restore, &output, &failed, &ctx)
            .await
            .is_err()
    );
    let active = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(active.id, racer.id);
    assert_ne!(
        assert_card_session_mcp_hash_parity(&repo, &card_id, &active.id).await,
        hash
    );
    assert_native_token_rejected(&repo, &token).await;
    // The owner must reject a mint after the earlier live carrier was displaced (N1).
    let active_hash = assert_card_session_mcp_hash_parity(&repo, &card_id, &active.id).await;
    let mut tx = repo.pool().begin().await.unwrap();
    assert!(
        calm_server::mcp_server::wiring::mint_and_persist_managed_planner_token(
            &mut tx, &card_id, &racer.id
        )
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    assert_eq!(
        assert_card_session_mcp_hash_parity(&repo, &card_id, &active.id).await,
        active_hash
    );
    assert_eq!(
        state
            .operation_runtime
            .submit("planner-harness-start", successor_key, payload)
            .await
            .unwrap(),
        successor
    );
    // The restored queue is harvested by the successful successor exactly once, even on replay.
    let mut delivered = 0;
    for _ in 0..100 {
        let raw = std::fs::read_to_string(&capture).unwrap();
        delivered = raw
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|row| {
                row["method"] == "turn/start" && row.to_string().contains("racer owed sentence")
            })
            .count();
        if delivered > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        delivered, 1,
        "harvest must neither lose nor duplicate the racer's sentence"
    );
    server.stop_listener_for_test().await;
    state
        .harness
        .shutdown_track(&track.id, state.thread_seals().clone())
        .await
        .unwrap();
}

async fn assert_native_token_rejected(repo: &SqlxRepo, token: &str) {
    assert!(
        calm_server::mcp_server::handshake::handle_initialize(
            repo,
            None,
            &json!({"_meta":{"dev.neige/auth":{"token":token}}}),
            "2024-11-05",
        )
        .await
        .is_err()
    );
}
