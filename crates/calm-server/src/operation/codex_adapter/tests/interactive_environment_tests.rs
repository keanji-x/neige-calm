use super::*;
use crate::db::RepoRead as _;
use crate::session_projection_repo::WorkerSessionProjectionRepo as _;

mod mint_admission;

struct IdleCard {
    _workspace: WorkerLeaseHarness,
    adapter: CodexAdapter,
    shared: Arc<SharedCodexAppServer>,
    pending: Arc<PendingThreadStartRegistry>,
    commands: Arc<std::sync::Mutex<Vec<String>>>,
    operations: Arc<SqlxOperationRepo>,
    ctx: SpawnCtx,
    op: Operation,
    output: TxOutput,
}

async fn idle_card() -> IdleCard {
    let workspace = worker_lease_harness().await;
    let pending = Arc::new(PendingThreadStartRegistry::new(
        workspace.repo.clone(),
        workspace.events.clone(),
    ));
    let shared = SharedCodexAppServer::new_fake_running_with_pending(
        workspace.repo.clone(),
        Some(pending.clone()),
    );
    let commands = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = commands.clone();
    let adapter = CodexAdapter::new_with_spawn_hook(
        workspace.repo.clone(),
        Arc::new(CodexClient::new_stub()),
        shared.clone(),
        pending.clone(),
        Arc::new(Mutex::new(())),
        CardRoleCache::new(),
        TrackAreaCache::new(),
        Arc::new(move |_, command, _, _| {
            captured.lock().unwrap().push(command);
            Box::pin(async { Ok(SpawnHandle::NoOp) })
        }),
    );
    let operations = Arc::new(SqlxOperationRepo::new(workspace.repo.db().clone()));
    let payload = serde_json::to_value(CodexCreateOperationPayload {
        actor: ActorId::KernelDispatcher,
        worker_session_id: None,
        request: NormalizedCodexCreateRequest {
            track_id: workspace.track_id.clone(),
            title: None,
            sort: None,
            cwd: workspace.worktree.display().to_string(),
            prompt: None,
            icon_bg: None,
            icon_fg: None,
            theme: RequestTheme::default_dark(),
        },
    })
    .unwrap();
    let id = operations
        .insert_operation(
            "codex-create",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: None,
                payload_hash: "idle-fixture".into(),
            },
            payload,
        )
        .await
        .unwrap();
    let claim = operations
        .claim_drive_batch(1)
        .await
        .unwrap()
        .into_iter()
        .find(|op| op.id == id)
        .unwrap();
    let prepared = crate::operation::tests::collect_claimed_preparation(
        workspace.repo.clone(),
        &adapter,
        &claim,
    )
    .await
    .unwrap();
    operations
        .prepare_tx_and_advance(&claim, &adapter, prepared)
        .await
        .unwrap()
        .committed()
        .unwrap();
    let claim = operations
        .claim_drive_batch(1)
        .await
        .unwrap()
        .into_iter()
        .find(|op| op.id == id)
        .unwrap();
    let output = crate::operation::required_output(&claim).unwrap().clone();
    let kind = adapter.app_server_interact_kind(&output, &claim).unwrap();
    operations
        .set_phase(&claim, Phase::AppServerInteract { kind }, None)
        .await
        .unwrap()
        .applied()
        .unwrap();
    let op = operations
        .claim_drive_batch(1)
        .await
        .unwrap()
        .into_iter()
        .find(|op| op.id == id)
        .unwrap();
    let ctx = SpawnCtx::new(
        workspace.repo.clone(),
        operations.clone(),
        Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new(),
        workspace.events.clone(),
        OperationCompletionBus::new(),
    );
    IdleCard {
        _workspace: workspace,
        adapter,
        shared,
        pending,
        commands,
        operations,
        ctx,
        op,
        output,
    }
}

#[tokio::test]
async fn interactive_workspace_idle_thread_retry_reuses_identity_without_starting_turn() {
    let mut fixture = idle_card().await;
    assert!(matches!(
        fixture
            .adapter
            .app_server_interact_kind(&fixture.output, &fixture.op)
            .unwrap(),
        AppServerInteractKind::MintAndAwait { .. }
    ));
    let first = fixture
        .adapter
        .app_server_interact(&mut fixture.output, &fixture.op, &fixture.ctx)
        .await
        .unwrap();
    let AppServerInteractOutcome::MintedAndAwaited { thread_id } = first else {
        panic!("idle workspace thread must be minted");
    };
    fixture
        .adapter
        .app_server_interact(&mut fixture.output, &fixture.op, &fixture.ctx)
        .await
        .unwrap();
    assert_eq!(fixture.shared.started_thread_params_for_test().len(), 1);
    assert!(fixture.shared.started_turns_for_test().is_empty());
    fixture
        .adapter
        .spawn_side_effect(&fixture.output, &fixture.op, &fixture.ctx)
        .await
        .unwrap();
    assert!(
        fixture.commands.lock().unwrap()[0]
            .contains(&format!("codex resume '{}' --remote", thread_id))
    );
    assert_eq!(fixture.pending.pending_count().await, 0);
    let cleanup = fixture
        .adapter
        .plan_compensation(
            PhaseTag::SpawnStarted,
            "viewer failed",
            &fixture.output,
            &fixture.op,
        )
        .await
        .unwrap();
    assert!(cleanup.steps.iter().any(
        |step| step.op == "interrupt_shared_codex_turn" && step.args["thread_id"] == thread_id
    ));
    assert!(
        cleanup
            .steps
            .iter()
            .all(|step| step.op != "pending_codex_threads_remove_by_card")
    );
    assert!(
        cleanup
            .steps
            .iter()
            .any(|step| step.op == "session_projection_set_status_failed_for_card")
    );
}

#[tokio::test]
async fn interactive_workspace_metadata_refuses_malformed_downgrades() {
    let fixture = idle_card().await;
    for value in [
        serde_json::json!(false),
        serde_json::json!(null),
        serde_json::json!("true"),
        serde_json::json!(1),
        serde_json::json!({}),
    ] {
        let mut output = fixture.output.clone();
        output.data["workspace_environment_thread"] = value;
        assert!(matches!(
            fixture
                .adapter
                .app_server_interact_kind(&output, &fixture.op),
            Err(CalmError::Internal(_))
        ));
        assert!(matches!(
            fixture
                .adapter
                .plan_compensation(PhaseTag::TxCommitted, "invalid", &output, &fixture.op)
                .await,
            Err(CalmError::Internal(_))
        ));
        assert!(matches!(
            fixture
                .adapter
                .spawn_side_effect(&output, &fixture.op, &fixture.ctx)
                .await,
            Err(CalmError::Internal(_))
        ));
    }
    assert!(fixture.shared.started_thread_params_for_test().is_empty());
}

#[tokio::test]
async fn interactive_empty_viewer_failure_keeps_native_identity_and_emits_once() {
    use crate::db::sqlite::session_set_handle_state_tx;
    use crate::session_projection_lookup::project_runtime_into_cards_payload;

    let mut fixture = idle_card().await;
    fixture
        .adapter
        .app_server_interact(&mut fixture.output, &fixture.op, &fixture.ctx)
        .await
        .unwrap();
    let card_id = fixture.output.output_string("card_id", "test").unwrap();
    let worker_session_id = fixture
        .output
        .target_id
        .clone()
        .expect("worker session target");
    let thread = fixture
        .output
        .output_string("codex_thread_id", "test")
        .unwrap();
    let worker_session_to_mark = worker_session_id.clone();
    crate::db::write_in_tx_typed(fixture._workspace.repo.as_ref(), move |tx| {
        Box::pin(async move {
            session_set_handle_state_tx(
                tx,
                &worker_session_to_mark,
                Some(json!({"timeout_cleanup":true})),
            )
            .await?;
            Ok::<_, CalmError>(())
        })
    })
    .await
    .unwrap();
    let mut events = fixture._workspace.events.subscribe();
    let cleanup = fixture
        .adapter
        .plan_compensation(
            PhaseTag::SpawnStarted,
            "viewer failed",
            &fixture.output,
            &fixture.op,
        )
        .await
        .unwrap();
    for _ in 0..2 {
        for step in &cleanup.steps {
            fixture
                .adapter
                .compensate_step(step, &fixture.output, &fixture.op, &fixture.ctx)
                .await
                .unwrap();
        }
    }
    let repo = &fixture._workspace.repo;
    let runtime = repo
        .session_projection_by_id(&worker_session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(runtime.status, WorkerSessionState::Failed);
    assert_eq!(runtime.thread_id.as_deref(), Some(thread.as_str()));
    assert_eq!(
        runtime.handle_state_json.as_ref().unwrap()["timeout_cleanup"],
        true
    );
    let mut card = repo.card_get(&card_id).await.unwrap().unwrap();
    crate::session_projection_lookup::project_runtime_into_card_payload(repo.as_ref(), &mut card)
        .await
        .unwrap();
    assert_eq!(card.payload["codex_thread_status"], "failed_to_spawn");
    assert_eq!(card.payload["codex_thread_id"], thread);
    let mut cards = vec![repo.card_get(&card_id).await.unwrap().unwrap()];
    project_runtime_into_cards_payload(repo.as_ref(), &mut cards)
        .await
        .unwrap();
    let batch_runtime = cards[0].runtime.as_ref().unwrap();
    assert_eq!(batch_runtime.worker_session_id, worker_session_id);
    assert_eq!(batch_runtime.thread_id.as_deref(), Some(thread.as_str()));
    assert_eq!(
        batch_runtime.thread_status.as_deref(),
        Some("failed_to_spawn")
    );
    let updates = std::iter::from_fn(|| events.try_recv().ok())
        .filter(|event| matches!(event.event, Event::CardUpdated(_)))
        .collect::<Vec<_>>();
    assert_eq!(
        updates.len(),
        1,
        "replayed completion emits no duplicate update"
    );
    assert_eq!(updates[0].actor, ActorId::Kernel);
    assert_eq!(fixture.shared.minted_thread_cwds_for_test().len(), 1);
}

#[tokio::test]
async fn interactive_empty_viewer_failure_does_not_complete_a_successor() {
    use crate::db::sqlite::{session_complete_tx, session_start_runtime_tx};
    use crate::session_projection_repo::WorkerSessionInit;

    let mut fixture = idle_card().await;
    fixture
        .adapter
        .app_server_interact(&mut fixture.output, &fixture.op, &fixture.ctx)
        .await
        .unwrap();
    let card_id = fixture.output.output_string("card_id", "test").unwrap();
    let cleanup = fixture
        .adapter
        .plan_compensation(
            PhaseTag::SpawnStarted,
            "viewer failed",
            &fixture.output,
            &fixture.op,
        )
        .await
        .unwrap();
    let successor = new_id();
    let original_session = fixture
        .output
        .target_id
        .clone()
        .expect("worker session target");
    let successor_to_start = successor.clone();
    let card_to_start = card_id.clone();
    crate::db::write_in_tx_typed(fixture._workspace.repo.as_ref(), move |tx| {
        Box::pin(async move {
            session_complete_tx(tx, &original_session, WorkerSessionState::Failed).await?;
            session_start_runtime_tx(
                tx,
                WorkerSessionInit {
                    id: successor_to_start,
                    card_id: card_to_start,
                    kind: WorkerSessionKind::CodexCard,
                    agent_provider: Some(AgentProvider::Codex),
                    status: WorkerSessionState::Running,
                    terminal_run_id: None,
                    thread_id: Some("successor-native-thread".into()),
                    session_id: None,
                    active_turn_id: None,
                    handle_state_json: Some(json!({"successor":true})),
                    spawn_op_id: None,
                    now_ms: now_ms(),
                },
            )
            .await?;
            Ok::<_, CalmError>(())
        })
    })
    .await
    .unwrap();
    let before = fixture
        ._workspace
        .repo
        .session_projection_by_id(&successor)
        .await
        .unwrap();
    let mut events = fixture._workspace.events.subscribe();
    for step in &cleanup.steps {
        fixture
            .adapter
            .compensate_step(step, &fixture.output, &fixture.op, &fixture.ctx)
            .await
            .unwrap();
    }
    assert_eq!(
        fixture
            ._workspace
            .repo
            .session_projection_by_id(&successor)
            .await
            .unwrap(),
        before
    );
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event.event, Event::CardUpdated(_)))
    );
    assert_eq!(fixture.shared.minted_thread_cwds_for_test().len(), 1);
}

#[tokio::test]
async fn interactive_released_compensation_keeps_its_native_failure_semantics() {
    let mut fixture = idle_card().await;
    fixture
        .adapter
        .app_server_interact(&mut fixture.output, &fixture.op, &fixture.ctx)
        .await
        .unwrap();
    let card_id = fixture.output.output_string("card_id", "test").unwrap();
    let worker_session_id = fixture
        .output
        .target_id
        .clone()
        .expect("worker session target");
    let thread = fixture
        .output
        .output_string("codex_thread_id", "test")
        .unwrap();
    let step = crate::operation::CompensationStep::new(
        "runtime_set_status_failed_for_card",
        json!({"card_id":card_id}),
    );
    fixture
        .adapter
        .compensate_step(&step, &fixture.output, &fixture.op, &fixture.ctx)
        .await
        .unwrap();
    let runtime = fixture
        ._workspace
        .repo
        .session_projection_by_id(&worker_session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(runtime.status, WorkerSessionState::Failed);
    assert_eq!(runtime.thread_id.as_deref(), Some(thread.as_str()));
    assert!(runtime.handle_state_json.is_none());
    let mut card = fixture
        ._workspace
        .repo
        .card_get(&card_id)
        .await
        .unwrap()
        .unwrap();
    crate::session_projection_lookup::project_runtime_into_card_payload(
        fixture._workspace.repo.as_ref(),
        &mut card,
    )
    .await
    .unwrap();
    assert!(card.runtime.unwrap().thread_status.is_none());
    // If an older binary already completed the state step, a marker-aware
    // viewer failure replay can still commit its explicit presentation fact.
    let cleanup = fixture
        .adapter
        .plan_compensation(
            PhaseTag::SpawnStarted,
            "viewer failed",
            &fixture.output,
            &fixture.op,
        )
        .await
        .unwrap();
    for step in &cleanup.steps {
        fixture
            .adapter
            .compensate_step(step, &fixture.output, &fixture.op, &fixture.ctx)
            .await
            .unwrap();
    }
    let mut card = fixture
        ._workspace
        .repo
        .card_get(&card_id)
        .await
        .unwrap()
        .unwrap();
    crate::session_projection_lookup::project_runtime_into_card_payload(
        fixture._workspace.repo.as_ref(),
        &mut card,
    )
    .await
    .unwrap();
    assert_eq!(card.payload["codex_thread_status"], "failed_to_spawn");
    assert_eq!(card.payload["codex_thread_id"], thread);
}

#[tokio::test]
async fn interactive_empty_viewer_failure_after_card_deletion_drains_without_update() {
    use crate::db::sqlite::card_with_terminal_rollback_tx;

    let mut fixture = idle_card().await;
    fixture
        .adapter
        .app_server_interact(&mut fixture.output, &fixture.op, &fixture.ctx)
        .await
        .unwrap();
    let card_id = fixture.output.output_string("card_id", "test").unwrap();
    let cleanup = fixture
        .adapter
        .plan_compensation(
            PhaseTag::SpawnStarted,
            "viewer failed",
            &fixture.output,
            &fixture.op,
        )
        .await
        .unwrap();
    let terminal_id = fixture.output.output_string("terminal_id", "test").unwrap();
    let card_to_delete = card_id.clone();
    let roles = fixture._workspace.repo.card_role_cache().clone();
    let deleted = crate::db::write_in_tx_typed(fixture._workspace.repo.as_ref(), move |tx| {
        Box::pin(async move {
            Ok::<_, CalmError>(
                card_with_terminal_rollback_tx(tx, &card_to_delete, &terminal_id, &roles).await?,
            )
        })
    })
    .await
    .unwrap();
    assert!(deleted.retire.is_none(), "the viewer was never admitted");
    let mut events = fixture._workspace.events.subscribe();
    for _ in 0..2 {
        for step in &cleanup.steps {
            fixture
                .adapter
                .compensate_step(step, &fixture.output, &fixture.op, &fixture.ctx)
                .await
                .unwrap();
        }
    }
    assert!(
        fixture
            ._workspace
            .repo
            .card_get(&card_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event.event, Event::CardUpdated(_)))
    );
    assert_eq!(fixture.shared.minted_thread_cwds_for_test().len(), 1);
}

#[tokio::test]
async fn interactive_prompted_failure_does_not_record_empty_viewer_failure() {
    let mut fixture = idle_card().await;
    // The prompt start path already owns native failure semantics. The empty
    // viewer marker must not replace that released contract.
    fixture.output.data["prompt"] = json!("first input");
    let cleanup = fixture
        .adapter
        .plan_compensation(
            PhaseTag::SpawnStarted,
            "prompted start failed",
            &fixture.output,
            &fixture.op,
        )
        .await
        .unwrap();
    for step in &cleanup.steps {
        fixture
            .adapter
            .compensate_step(step, &fixture.output, &fixture.op, &fixture.ctx)
            .await
            .unwrap();
    }
    let worker_session_id = fixture
        .output
        .target_id
        .clone()
        .expect("worker session target");
    let runtime = fixture
        ._workspace
        .repo
        .session_projection_by_id(&worker_session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(runtime.status, WorkerSessionState::Failed);
    assert!(runtime.handle_state_json.is_none());
}

#[tokio::test]
async fn interactive_workspace_legacy_prepared_output_keeps_pending_replay() {
    let mut fixture = idle_card().await;
    fixture
        .output
        .data
        .as_object_mut()
        .unwrap()
        .remove("workspace_environment_thread");
    let kind = fixture
        .adapter
        .app_server_interact_kind(&fixture.output, &fixture.op)
        .unwrap();
    assert!(matches!(
        kind,
        AppServerInteractKind::RegisterPending { .. }
    ));
    fixture
        .operations
        .set_phase_and_tx_output(
            &fixture.op,
            Phase::AppServerInteract { kind },
            &fixture.output,
        )
        .await
        .unwrap()
        .unwrap();
    fixture.op = fixture
        .operations
        .claim_drive_batch(1)
        .await
        .unwrap()
        .into_iter()
        .find(|op| op.id == fixture.op.id)
        .unwrap();
    let outcome = fixture
        .adapter
        .app_server_interact(&mut fixture.output, &fixture.op, &fixture.ctx)
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        AppServerInteractOutcome::RegisteredPendingForLaterAttribution { .. }
    ));
    fixture
        .adapter
        .spawn_side_effect(&fixture.output, &fixture.op, &fixture.ctx)
        .await
        .unwrap();
    assert!(fixture.shared.started_thread_params_for_test().is_empty());
    assert!(fixture.commands.lock().unwrap()[0].starts_with("codex --remote"));
    assert_eq!(fixture.pending.pending_count().await, 1);
    let cleanup = fixture
        .adapter
        .plan_compensation(
            PhaseTag::SpawnStarted,
            "legacy viewer failed",
            &fixture.output,
            &fixture.op,
        )
        .await
        .unwrap();
    assert!(
        cleanup
            .steps
            .iter()
            .any(|step| step.op == "pending_codex_threads_remove_by_card")
    );
}
