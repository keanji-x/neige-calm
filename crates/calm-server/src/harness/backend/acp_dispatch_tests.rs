use super::*;
use crate::acp_planner::config::{AcpAgentConfig, AcpPlannerConfig, AcpPlannerHost};
use crate::acp_planner::session::{AcpPlannerSession, SessionParams};
use crate::db::sqlite::acp_submission_get;
use crate::harness::planner_event::PlannerEventKind;
use serde_json::{Value, json};

async fn fixture() -> (
    PlannerBackend,
    Arc<AcpPlannerSession>,
    super::dispatch_fixture::Owner,
    tempfile::TempDir,
) {
    let (backend, session, owner, dir, _held) = fixture_full().await;
    (backend, session, owner, dir)
}

async fn fixture_full() -> (
    PlannerBackend,
    Arc<AcpPlannerSession>,
    super::dispatch_fixture::Owner,
    tempfile::TempDir,
    crate::harness::held_requests::HeldRequestReceiver,
) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("scenario"), "reply").unwrap();
    let owner = super::dispatch_fixture::seed(dir.path(), AgentProvider::OpenCode).await;
    let host = Arc::new(
        AcpPlannerHost::new(
            Some(AcpPlannerConfig {
                agents: vec![AcpAgentConfig {
                    provider: AgentProvider::OpenCode,
                    command: "/usr/bin/python3".into(),
                    args: vec![
                        "-u".into(),
                        concat!(
                            env!("CARGO_MANIFEST_DIR"),
                            "/tests/fixtures/acp_planner/agent.py"
                        )
                        .into(),
                    ],
                    env: [(
                        "ACP_FIXTURE_ROOT".into(),
                        dir.path().to_string_lossy().into_owned(),
                    )]
                    .into(),
                    expected_agent_name: "Fixture ACP".into(),
                    expected_agent_version: "1".into(),
                }],
            }),
            &dir.path().join("data"),
            dir.path().join("unused-shim"),
            dir.path().join("mcp.sock"),
        )
        .unwrap(),
    );
    let session = Arc::new(AcpPlannerSession::open(SessionParams {
        host,
        provider: AgentProvider::OpenCode,
        repo: owner.repo.clone(),
        seals: Arc::new(ThreadSeals::new()),
        worker_session_id: owner.worker.clone(),
        card_id: owner.card.clone(),
        track_id: owner.track.clone(),
        cwd: dir.path().into(),
        instructions: "fixture".into(),
    }));
    let (held, rx) = tokio::sync::mpsc::unbounded_channel();
    session.mark_installed(held);
    (
        PlannerBackend(Arm::Acp(session.clone())),
        session,
        owner,
        dir,
        rx,
    )
}

async fn completed(events: &mut broadcast::Receiver<PlannerEvent>) {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while !matches!(
            events.recv().await.unwrap().kind,
            PlannerEventKind::TurnCompleted { .. }
        ) {}
    })
    .await
    .unwrap();
}

fn prompt_count(dir: &std::path::Path) -> usize {
    std::fs::read_to_string(dir.join("requests.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|frame| frame["method"] == "session/prompt")
        .count()
}

#[tokio::test]
async fn acp_v2_journal_contains_only_original_proof_and_replay_never_resends() {
    let (backend, session, owner, dir) = fixture().await;
    let proof = super::dispatch_tests::proof();
    let mut events = session.subscribe_events();
    let selection = TurnModelSelection::inherit();
    let logical_turn_id = super::dispatch_tests::logical_turn_id();
    let send = || {
        backend.dispatch_input(
            &logical_turn_id,
            proof.attempt(),
            &owner.thread,
            &proof,
            vec![InputItem::Text {
                text: "private body must not enter the journal".into(),
            }],
            &selection,
            PlannerPermissionMode::Never,
            None,
            &|| Box::pin(async { Ok(()) }),
        )
    };
    let report = send().await;
    let InputDelivery::Unknown(correlation) = report.delivery() else {
        panic!("ACP local ownership is not acceptance")
    };
    assert!(report.outcome().is_ok());
    assert!(correlation.native_session_id().is_some());
    assert!(correlation.native_turn_id().is_none());
    completed(&mut events).await;
    let receipt = acp_submission_get(
        owner.repo.pool(),
        &owner.worker,
        proof.attempt().submission_id.as_str(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&receipt.input_json).unwrap(),
        serde_json::to_value(&proof).unwrap()
    );
    assert!(!receipt.input_json.contains("private body"));
    let replay = send().await;
    assert!(matches!(replay.delivery(), InputDelivery::Unknown(_)));
    assert_eq!(
        replay.outcome().as_ref().unwrap(),
        report.outcome().as_ref().unwrap()
    );
    assert_eq!(prompt_count(dir.path()), 1);
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn acp_v2_receipt_rejects_another_attempt_without_new_prompt() {
    let (backend, session, owner, dir) = fixture().await;
    let proof = super::dispatch_tests::proof();
    let mut events = session.subscribe_events();
    let report = backend
        .dispatch_input(
            &super::dispatch_tests::logical_turn_id(),
            proof.attempt(),
            &owner.thread,
            &proof,
            vec![InputItem::Text {
                text: "first".into(),
            }],
            &TurnModelSelection::inherit(),
            PlannerPermissionMode::Never,
            None,
            &|| Box::pin(async { Ok(()) }),
        )
        .await;
    assert!(report.outcome().is_ok());
    completed(&mut events).await;
    let mut changed = serde_json::to_value(&proof).unwrap();
    changed["attempt"]["attempt_id"] = serde_json::json!("44444444444444444444444444444444");
    let changed: calm_types::conversation::SubmissionDispatchProof =
        serde_json::from_value(changed).unwrap();
    let report = backend
        .dispatch_input(
            &super::dispatch_tests::logical_turn_id(),
            changed.attempt(),
            &owner.thread,
            &changed,
            vec![InputItem::Text {
                text: "different".into(),
            }],
            &TurnModelSelection::inherit(),
            PlannerPermissionMode::Never,
            None,
            &|| Box::pin(async { Ok(()) }),
        )
        .await;
    assert!(matches!(report.delivery(), InputDelivery::Unknown(_)));
    assert!(report.outcome().is_err());
    assert_eq!(prompt_count(dir.path()), 1);
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn acp_legacy_proof_is_preserved_and_cannot_authorize_v2_replay() {
    let (backend, session, owner, dir) = fixture().await;
    let proof = super::dispatch_tests::proof();
    let mut events = session.subscribe_events();
    session
        .turn_start(
            &owner.thread,
            vec![InputItem::Text {
                text: "legacy".into(),
            }],
            &TurnModelSelection::inherit(),
            PlannerPermissionMode::Never,
            proof.attempt().submission_id.as_str(),
            &[],
        )
        .await
        .unwrap();
    completed(&mut events).await;
    let before = acp_submission_get(
        owner.repo.pool(),
        &owner.worker,
        proof.attempt().submission_id.as_str(),
    )
    .await
    .unwrap()
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&before.input_json).unwrap();
    assert!(value.get("version").is_none());
    assert!(value.get("input").is_some());
    assert!(value.get("prompt").is_some());
    assert_eq!(value["queue"], serde_json::json!([]));
    let report = backend
        .dispatch_input(
            &super::dispatch_tests::logical_turn_id(),
            proof.attempt(),
            &owner.thread,
            &proof,
            vec![],
            &TurnModelSelection::inherit(),
            PlannerPermissionMode::Never,
            None,
            &|| Box::pin(async { Ok(()) }),
        )
        .await;
    assert!(matches!(report.delivery(), InputDelivery::Unknown(_)));
    assert!(report.outcome().is_err());
    let after = acp_submission_get(
        owner.repo.pool(),
        &owner.worker,
        proof.attempt().submission_id.as_str(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(before.input_json, after.input_json);
    assert_eq!(prompt_count(dir.path()), 1);
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn acp_new_rewind_is_typed_unsupported_during_pure_planning() {
    let (backend, session, owner, dir) = fixture().await;
    assert!(!backend.history_replacement_supported());
    let result = backend.plan_rewind(
        &owner.thread,
        RewindTarget {
            turn_id: "old",
            prompt_client_id: None,
            previous_turn: None,
        },
    );
    assert!(matches!(result, Err(RewindPlanFailure::Unsupported(_))));
    assert!(!dir.path().join("requests.jsonl").exists());
    session.shutdown().await.unwrap();
}

fn end_target(
    owner: &super::dispatch_fixture::Owner,
    report: &InputAttemptReport,
) -> ExecutionEndTarget {
    let InputDelivery::Unknown(correlation) = report.delivery() else {
        panic!("ACP delivery stays unknown")
    };
    ExecutionEndTarget {
        card_id: owner.card.clone(),
        track_id: owner.track.clone(),
        worker_session_id: owner.worker.clone(),
        logical_turn_id: super::dispatch_tests::logical_turn_id(),
        proof: super::dispatch_tests::proof(),
        scope: ExecutionEndScope::LocalExecution {
            thread_id: owner.thread.clone(),
            native_session_id: correlation.native_session_id().unwrap().into(),
            local_turn_id: report.outcome().as_ref().unwrap().clone(),
        },
    }
}

async fn qualify(backend: &PlannerBackend, target: &ExecutionEndTarget) {
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

async fn dispatch_report(
    backend: &PlannerBackend,
    owner: &super::dispatch_fixture::Owner,
    mode: PlannerPermissionMode,
) -> InputAttemptReport {
    let proof = super::dispatch_tests::proof();
    backend
        .dispatch_input(
            &super::dispatch_tests::logical_turn_id(),
            proof.attempt(),
            &owner.thread,
            &proof,
            vec![InputItem::Text {
                text: "input".into(),
            }],
            &TurnModelSelection::inherit(),
            mode,
            None,
            &|| Box::pin(async { Ok(()) }),
        )
        .await
}

#[tokio::test]
async fn acp_completed_receipt_qualifies_exact_execution_including_known_refusal() {
    for scenario in ["reply", "prompt-error"] {
        let (backend, session, owner, dir, _held) = fixture_full().await;
        std::fs::write(dir.path().join("scenario"), scenario).unwrap();
        let mut events = session.subscribe_events();
        let report = dispatch_report(&backend, &owner, PlannerPermissionMode::Never).await;
        let target = end_target(&owner, &report);
        completed(&mut events).await;
        qualify(&backend, &target).await;
        qualify(&backend, &target).await;
        let mut wrong_logical = target.clone();
        wrong_logical.logical_turn_id =
            calm_types::conversation::LogicalTurnId::parse("77777777777777777777777777777777")
                .unwrap();
        assert!(
            backend
                .qualify_execution_end(&wrong_logical)
                .await
                .unwrap()
                .is_none(),
            "a caller's LogicalTurnId cannot replace the dispatch owner's association"
        );
        if scenario == "prompt-error" {
            let receipt = acp_submission_get(
                owner.repo.pool(),
                &owner.worker,
                target.proof.attempt().submission_id.as_str(),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(
                !receipt
                    .outcome_json
                    .unwrap()
                    .contains("Insufficient balance")
            );
            let projected: String = sqlx::query_scalar("SELECT params FROM harness_items WHERE worker_session_id=?1 AND method='turn/completed'")
                .bind(&owner.worker).fetch_one(owner.repo.pool()).await.unwrap();
            assert!(!projected.contains("Insufficient balance"));
        }
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
        qualify(&backend, &target).await;
        let mut foreign = target.clone();
        foreign.track_id = "another-track".into();
        assert!(
            backend
                .qualify_execution_end(&foreign)
                .await
                .unwrap()
                .is_none()
        );
        let mut changed = serde_json::to_value(&target.proof).unwrap();
        changed["attempt"]["attempt_id"] = serde_json::json!("66666666666666666666666666666666");
        foreign = target;
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
}

#[tokio::test]
async fn acp_permission_held_driver_allows_same_loop_to_answer_before_qualification() {
    let (backend, session, owner, dir, mut held) = fixture_full().await;
    std::fs::write(dir.path().join("scenario"), "ask").unwrap();
    let mut events = session.subscribe_events();
    let report = dispatch_report(&backend, &owner, PlannerPermissionMode::Ask).await;
    let target = end_target(&owner, &report);
    let open = tokio::time::timeout(std::time::Duration::from_secs(5), held.recv())
        .await
        .unwrap()
        .unwrap();
    let crate::harness::held_requests::HeldRequestMessage::Open { responder, .. } = open else {
        panic!("actual native approval")
    };
    let mut occupied = Vec::new();
    for _ in 0..owner.repo.pool().options().get_max_connections() {
        occupied.push(owner.repo.pool().acquire().await.unwrap());
    }
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
    drop(occupied);
    responder.respond(0);
    completed(&mut events).await;
    qualify(&backend, &target).await;
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn acp_synthetic_unknown_completion_never_qualifies_execution_end() {
    let (backend, session, owner, dir, _held) = fixture_full().await;
    std::fs::write(dir.path().join("scenario"), "lost").unwrap();
    let mut events = session.subscribe_events();
    let report = dispatch_report(&backend, &owner, PlannerPermissionMode::Never).await;
    let target = end_target(&owner, &report);
    completed(&mut events).await;
    assert!(
        backend
            .qualify_execution_end(&target)
            .await
            .unwrap()
            .is_none()
    );
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
async fn acp_finished_driver_requires_the_actual_durable_receipt_to_match() {
    let (backend, session, owner, _dir, _held) = fixture_full().await;
    let mut events = session.subscribe_events();
    let report = dispatch_report(&backend, &owner, PlannerPermissionMode::Never).await;
    let target = end_target(&owner, &report);
    completed(&mut events).await;
    qualify(&backend, &target).await;
    let client = target.proof.attempt().submission_id.as_str();
    let receipt = acp_submission_get(owner.repo.pool(), &owner.worker, client)
        .await
        .unwrap()
        .unwrap();
    let mut changed: Value = serde_json::from_str(&receipt.input_json).unwrap();
    changed["attempt"]["attempt_id"] = json!("66666666666666666666666666666666");
    // The target and actual finished driver stay correct. Change the persisted
    // row independently, so the transient association cannot mask this check.
    for (proof, thread, session_id, turn, outcome) in [
        (
            changed.to_string(),
            receipt.thread_id.clone(),
            receipt.native_session_id.clone(),
            receipt.turn_id.clone(),
            receipt.outcome_json.clone(),
        ),
        (
            receipt.input_json.clone(),
            "foreign-thread".into(),
            receipt.native_session_id.clone(),
            receipt.turn_id.clone(),
            receipt.outcome_json.clone(),
        ),
        (
            receipt.input_json.clone(),
            receipt.thread_id.clone(),
            "foreign-session".into(),
            receipt.turn_id.clone(),
            receipt.outcome_json.clone(),
        ),
        (
            receipt.input_json.clone(),
            receipt.thread_id.clone(),
            receipt.native_session_id.clone(),
            "foreign-turn".into(),
            receipt.outcome_json.clone(),
        ),
        (
            receipt.input_json.clone(),
            receipt.thread_id.clone(),
            receipt.native_session_id.clone(),
            receipt.turn_id.clone(),
            Some(json!({"id":"foreign-outcome"}).to_string()),
        ),
    ] {
        sqlx::query(
            "UPDATE acp_submissions
             SET input_json=?3, thread_id=?4, native_session_id=?5, turn_id=?6, outcome_json=?7
             WHERE worker_session_id=?1 AND client_id=?2",
        )
        .bind(&owner.worker)
        .bind(client)
        .bind(proof)
        .bind(thread)
        .bind(session_id)
        .bind(turn)
        .bind(outcome)
        .execute(owner.repo.pool())
        .await
        .unwrap();
        assert!(
            backend
                .qualify_execution_end(&target)
                .await
                .unwrap()
                .is_none()
        );
    }
    sqlx::query(
        "UPDATE acp_submissions
             SET input_json=?3, thread_id=?4, native_session_id=?5, turn_id=?6, outcome_json=?7
             WHERE worker_session_id=?1 AND client_id=?2",
    )
    .bind(&owner.worker)
    .bind(client)
    .bind(&receipt.input_json)
    .bind(&receipt.thread_id)
    .bind(&receipt.native_session_id)
    .bind(&receipt.turn_id)
    .bind(&receipt.outcome_json)
    .execute(owner.repo.pool())
    .await
    .unwrap();
    qualify(&backend, &target).await;
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn acp_journal_wait_rechecks_the_actual_slot_without_holding_native_gates() {
    let (backend, session, owner, _dir, _held) = fixture_full().await;
    let mut events = session.subscribe_events();
    let report = dispatch_report(&backend, &owner, PlannerPermissionMode::Never).await;
    let target = end_target(&owner, &report);
    completed(&mut events).await;
    qualify(&backend, &target).await;
    let observed = backend
        .qualify_execution_end(&target)
        .await
        .unwrap()
        .unwrap();
    let mut occupied = Vec::new();
    for _ in 0..owner.repo.pool().options().get_max_connections() {
        occupied.push(owner.repo.pool().acquire().await.unwrap());
    }
    let mut observing = Box::pin(backend.qualify_execution_end(&target));
    assert!(futures_util::poll!(&mut observing).is_pending());
    // Both actual native gates must be available while the journal read waits.
    let issue = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        session.hold_execution_issue_for_test(),
    )
    .await
    .unwrap();
    let driver = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        session.hold_execution_driver_for_test(),
    )
    .await
    .unwrap();
    drop(occupied);
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), observing)
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
    assert!(observed.matches(&target));
    drop(driver);
    drop(issue);
    qualify(&backend, &target).await;
    session.shutdown().await.unwrap();
}
