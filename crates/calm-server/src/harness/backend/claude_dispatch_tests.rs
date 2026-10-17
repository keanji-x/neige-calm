use super::*;
use crate::claude_planner::config::{ClaudePlannerConfig, ClaudePlannerHost};
use crate::claude_planner::session::ClaudePlannerSessionParams;
use uuid::Uuid;

async fn fixture(
    scenario: &str,
) -> (
    PlannerBackend,
    Arc<ClaudePlannerSession>,
    tempfile::TempDir,
    String,
) {
    let (backend, session, dir, owner, _held) = fixture_full(scenario).await;
    (backend, session, dir, owner.thread)
}

async fn fixture_full(
    scenario: &str,
) -> (
    PlannerBackend,
    Arc<ClaudePlannerSession>,
    tempfile::TempDir,
    super::dispatch_fixture::Owner,
    crate::harness::held_requests::HeldRequestReceiver,
) {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("claude");
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/claude_planner_fake/claude.sh"
        ),
        &bin,
    )
    .unwrap();
    std::fs::write(dir.path().join("scenario"), scenario).unwrap();
    let owner = super::dispatch_fixture::seed(dir.path(), AgentProvider::Claude).await;
    let host = Arc::new(
        ClaudePlannerHost::new(
            Some(ClaudePlannerConfig {
                claude_binary: bin,
                claude_version: "2.1.280".into(),
                config_dir: dir.path().join("config"),
            }),
            &dir.path().join("data"),
            dir.path().join("unused-shim"),
            dir.path().join("mcp.sock"),
        )
        .unwrap(),
    );
    let session = Arc::new(
        ClaudePlannerSession::open(ClaudePlannerSessionParams {
            host,
            worker_session_id: owner.worker.clone(),
            card_id: owner.card.clone(),
            track_id: owner.track.clone(),
            cwd: dir.path().into(),
            instructions: "fixture".into(),
            calm_tools: crate::claude_planner::translate::ToolNames::new(
                "neige",
                Vec::<String>::new(),
            ),
            proxy: vec![],
            prior_total_tokens: 0,
            repo: owner.repo.clone(),
            seals: Arc::new(ThreadSeals::new()),
        })
        .await
        .unwrap(),
    );
    let (held, rx) = tokio::sync::mpsc::unbounded_channel();
    session.mark_installed(held);
    (
        PlannerBackend::claude_for_test(session.clone()),
        session,
        dir,
        owner,
        rx,
    )
}

async fn dispatch(backend: &PlannerBackend, thread: &str, text: String) -> InputAttemptReport {
    let proof = super::dispatch_tests::proof();
    backend
        .dispatch_input(
            &super::dispatch_tests::logical_turn_id(),
            proof.attempt(),
            thread,
            &proof,
            vec![InputItem::Text { text }],
            &TurnModelSelection::inherit(),
            PlannerPermissionMode::Never,
            None,
            &|| Box::pin(async { Ok(()) }),
        )
        .await
}

#[tokio::test]
async fn claude_version_refusal_is_not_dispatched_before_the_line_writer() {
    let (backend, session, dir, thread) = fixture("exit").await;
    std::fs::write(dir.path().join("version"), "2.1.279").unwrap();
    let report = dispatch(&backend, &thread, "input".into()).await;
    assert!(matches!(report.delivery(), InputDelivery::NotDispatched(_)));
    assert!(report.outcome().is_err());
    assert_eq!(session.user_line_writes_for_test(), 0);
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn claude_partial_pipe_write_is_unknown_even_after_cleanup() {
    let (backend, session, _dir, thread) = fixture("stall").await;
    let report = dispatch(&backend, &thread, "x".repeat(256 * 1024)).await;
    assert!(matches!(report.delivery(), InputDelivery::Unknown(_)));
    assert!(report.outcome().is_err());
    assert_eq!(session.user_line_writes_for_test(), 1);
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn claude_local_write_success_is_not_native_acceptance() {
    let (backend, session, _dir, thread) = fixture("exit").await;
    let mut events = session.subscribe_events();
    let report = dispatch(&backend, &thread, "input".into()).await;
    let InputDelivery::Unknown(correlation) = report.delivery() else {
        panic!("local write is not acceptance")
    };
    assert_eq!(correlation.native_session_id(), Some(thread.as_str()));
    assert!(correlation.native_turn_id().is_none());
    assert_eq!(
        correlation.local_turn_id(),
        report.outcome().as_ref().ok().map(String::as_str)
    );
    assert!(report.outcome().is_ok());
    let seen = collect_until_completed(&mut events).await;
    assert!(seen.iter().any(|event| matches!(&event.kind,
        crate::harness::planner_event::PlannerEventKind::Item { params, .. }
        if params["item"]["clientId"] == "11111111111111111111111111111111"
            && event.thread_id.as_deref() == Some(thread.as_str()))));
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn claude_legacy_rewind_remains_unknown_without_cli_effects() {
    let (backend, session, dir, _thread) = fixture("exit").await;
    assert!(!backend.history_replacement_supported());
    let mut old = BackendRewind(RewindArm::Claude {
        resume_at: Uuid::new_v4(),
        drops_turn: Uuid::new_v4(),
    });
    let report = backend.apply_rewind("thread", &mut old).await;
    assert!(matches!(
        report.observation(),
        RewindObservation::Unknown(_)
    ));
    assert!(report.into_outcome().is_err());
    let foreign = BackendRewind(RewindArm::CodexV2 {
        thread_id: "thread".into(),
        before_turn_id: "old".into(),
        reverted: true,
    });
    let proof = super::dispatch_tests::proof();
    let report = backend
        .dispatch_input(
            &super::dispatch_tests::logical_turn_id(),
            proof.attempt(),
            "thread",
            &proof,
            vec![],
            &TurnModelSelection::inherit(),
            PlannerPermissionMode::Never,
            Some(&foreign),
            &|| Box::pin(async { Ok(()) }),
        )
        .await;
    assert!(matches!(report.rewind(), RewindObservation::Unknown(_)));
    assert!(matches!(report.delivery(), InputDelivery::NotDispatched(_)));
    assert!(!dir.path().join("argv").exists());
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn claude_new_rewind_is_typed_unsupported_during_pure_planning() {
    let (backend, session, dir, thread) = fixture("exit").await;
    let prior = vec![TranscriptRow {
        id: 1,
        turn_id: Some("previous".into()),
        item_uuid: None,
        item_type: None,
        method: "turn/completed".into(),
        params: serde_json::json!({"lastRecordUuid":Uuid::new_v4().to_string()}).to_string(),
        input_segments: None,
    }];
    let result = backend.plan_rewind(
        &thread,
        RewindTarget {
            turn_id: "old",
            prompt_client_id: Some("11111111111111111111111111111111"),
            previous_turn: Some(&prior),
        },
    );
    assert!(matches!(result, Err(RewindPlanFailure::Unsupported(_))));
    assert!(!dir.path().join("argv").exists());
    assert_eq!(session.user_line_writes_for_test(), 0);
    session.shutdown().await.unwrap();
}

async fn collect_until_completed(
    events: &mut broadcast::Receiver<PlannerEvent>,
) -> Vec<PlannerEvent> {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let mut seen = Vec::new();
        loop {
            let event = events.recv().await.unwrap();
            let done = matches!(
                event.kind,
                crate::harness::planner_event::PlannerEventKind::TurnCompleted { .. }
            );
            seen.push(event);
            if done {
                return seen;
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn claude_unqualified_native_streams_never_emit_receipt_or_success() {
    for scenario in [
        "receipt-before-init",
        "wrong-init-session",
        "wrong-replay-session",
        "result-before-init",
    ] {
        let (backend, session, _dir, thread) = fixture(scenario).await;
        let mut events = session.subscribe_events();
        let report = dispatch(&backend, &thread, "input".into()).await;
        assert!(
            matches!(report.delivery(), InputDelivery::Unknown(_)),
            "{scenario}"
        );
        assert!(
            report.outcome().is_ok(),
            "local ownership is separate: {scenario}"
        );
        let seen = collect_until_completed(&mut events).await;
        session.shutdown().await.unwrap();
        assert!(
            !seen.iter().any(|event| matches!(&event.kind,
            crate::harness::planner_event::PlannerEventKind::Item { params, .. }
            if params["item"]["clientId"] == "11111111111111111111111111111111")),
            "unqualified receipt: {scenario}"
        );
        assert!(
            seen.iter().any(|event| matches!(&event.kind,
            crate::harness::planner_event::PlannerEventKind::TurnCompleted { turn }
            if turn["status"] == "failed")),
            "unqualified success: {scenario}"
        );
    }
}

fn end_target(owner: &super::dispatch_fixture::Owner, turn: &str) -> ExecutionEndTarget {
    ExecutionEndTarget {
        card_id: owner.card.clone(),
        track_id: owner.track.clone(),
        worker_session_id: owner.worker.clone(),
        logical_turn_id: super::dispatch_tests::logical_turn_id(),
        proof: super::dispatch_tests::proof(),
        scope: ExecutionEndScope::LocalExecution {
            thread_id: owner.thread.clone(),
            native_session_id: owner.thread.clone(),
            local_turn_id: turn.into(),
        },
    }
}

async fn qualified(backend: &PlannerBackend, target: &ExecutionEndTarget) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(end) = backend.qualify_execution_end(target).await.unwrap() {
                assert!(end.matches(target));
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn claude_real_driver_closure_is_exact_and_reusable_while_delivery_stays_unknown() {
    let (backend, session, _dir, owner, _held) = fixture_full("exit").await;
    let mut events = session.subscribe_events();
    let report = dispatch(&backend, &owner.thread, "input".into()).await;
    assert!(matches!(report.delivery(), InputDelivery::Unknown(_)));
    let target = end_target(&owner, report.outcome().as_ref().unwrap());
    collect_until_completed(&mut events).await;
    qualified(&backend, &target).await;
    qualified(&backend, &target).await; // a failed canonical CAS can retry the same proof
    let mut wrong_logical = target.clone();
    wrong_logical.logical_turn_id =
        calm_types::conversation::LogicalTurnId::parse("77777777777777777777777777777777").unwrap();
    assert!(
        backend
            .qualify_execution_end(&wrong_logical)
            .await
            .unwrap()
            .is_none(),
        "a caller's LogicalTurnId cannot replace the dispatch owner's association"
    );
    let issuance = session.hold_execution_issue_for_test().await;
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            backend.qualify_execution_end(&target),
        )
        .await
        .unwrap()
        .unwrap()
        .is_none(),
        "an unresolved native issue cannot qualify the previous slot"
    );
    drop(issuance);
    let same_slot = session.hold_execution_driver_for_test().await;
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            backend.qualify_execution_end(&target)
        )
        .await
        .unwrap()
        .unwrap()
        .is_none()
    );
    drop(same_slot);
    qualified(&backend, &target).await;
    let mut foreign = target.clone();
    foreign.worker_session_id = "another-runtime".into();
    assert!(
        backend
            .qualify_execution_end(&foreign)
            .await
            .unwrap()
            .is_none()
    );
    let mut changed = serde_json::to_value(&target.proof).unwrap();
    changed["attempt"]["attempt_id"] = serde_json::json!("66666666666666666666666666666666");
    foreign = target.clone();
    foreign.proof = serde_json::from_value(changed).unwrap();
    assert!(
        backend
            .qualify_execution_end(&foreign)
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(report.delivery(), InputDelivery::Unknown(_)));
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn claude_permission_held_driver_does_not_block_qualification_or_its_own_answer() {
    let (backend, session, _dir, owner, mut held) = fixture_full("ask").await;
    let proof = super::dispatch_tests::proof();
    let mut events = session.subscribe_events();
    let report = backend
        .dispatch_input(
            &super::dispatch_tests::logical_turn_id(),
            proof.attempt(),
            &owner.thread,
            &proof,
            vec![InputItem::Text {
                text: "input".into(),
            }],
            &TurnModelSelection::inherit(),
            PlannerPermissionMode::Ask,
            None,
            &|| Box::pin(async { Ok(()) }),
        )
        .await;
    let target = end_target(&owner, report.outcome().as_ref().unwrap());
    let open = tokio::time::timeout(std::time::Duration::from_secs(5), held.recv())
        .await
        .unwrap()
        .unwrap();
    let crate::harness::held_requests::HeldRequestMessage::Open { responder, .. } = open else {
        panic!("actual native approval")
    };
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            backend.qualify_execution_end(&target)
        )
        .await
        .unwrap()
        .unwrap()
        .is_none()
    );
    responder.respond(0);
    collect_until_completed(&mut events).await;
    qualified(&backend, &target).await;
    assert!(matches!(
        held.recv().await,
        Some(crate::harness::held_requests::HeldRequestMessage::ConnectionLost { .. })
    ));
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn claude_completed_display_without_successful_stop_never_qualifies() {
    let (backend, session, _dir, owner, _held) = fixture_full("exit").await;
    let worker = owner.worker.clone();
    session.set_after_spawn_hook_for_test(Arc::new(move || {
        crate::claude_planner::stop::fail_claude_planner_stop_for_test(&worker);
    }));
    let mut events = session.subscribe_events();
    let report = dispatch(&backend, &owner.thread, "input".into()).await;
    let target = end_target(&owner, report.outcome().as_ref().unwrap());
    collect_until_completed(&mut events).await;
    assert!(
        backend
            .qualify_execution_end(&target)
            .await
            .unwrap()
            .is_none()
    );
    crate::claude_planner::stop::clear_claude_planner_stop_failure_for_test(&owner.worker);
    session.shutdown().await.unwrap();
    assert!(
        backend
            .qualify_execution_end(&target)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn claude_completed_display_without_durable_record_never_qualifies() {
    let (backend, session, _dir, owner, _held) = fixture_full("exit").await;
    sqlx::query(
        "CREATE TRIGGER reject_terminal_record BEFORE INSERT ON harness_items
         WHEN NEW.method='turn/completed'
         BEGIN
         SELECT RAISE(FAIL,'fixture terminal recording failure');
         END",
    )
    .execute(owner.repo.pool())
    .await
    .unwrap();
    let mut events = session.subscribe_events();
    let report = dispatch(&backend, &owner.thread, "input".into()).await;
    let target = end_target(&owner, report.outcome().as_ref().unwrap());
    collect_until_completed(&mut events).await;
    session.shutdown().await.unwrap();
    assert!(
        backend
            .qualify_execution_end(&target)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn claude_canonical_terminal_keeps_native_error_text_out_of_durable_outcome() {
    let (backend, session, _dir, owner, _held) = fixture_full("result-error-no-reason").await;
    let mut events = session.subscribe_events();
    let report = dispatch(&backend, &owner.thread, "input".into()).await;
    let target = end_target(&owner, report.outcome().as_ref().unwrap());
    collect_until_completed(&mut events).await;
    qualified(&backend, &target).await;
    let projected: String = sqlx::query_scalar(
        "SELECT params FROM harness_items WHERE worker_session_id=?1 AND method='turn/completed'",
    )
    .bind(&owner.worker)
    .fetch_one(owner.repo.pool())
    .await
    .unwrap();
    assert!(!projected.contains("fake result error"));
    assert!(projected.contains("provider ended this execution"));
    session.shutdown().await.unwrap();
}
