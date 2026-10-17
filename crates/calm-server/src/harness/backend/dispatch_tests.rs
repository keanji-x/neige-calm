use super::*;
use crate::shared_codex_appserver::TurnStartReturnHook;
use calm_types::conversation::SubmissionDispatchProof;

async fn backend() -> (PlannerBackend, Arc<SharedCodexAppServer>) {
    let repo = Arc::new(
        crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap(),
    );
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo, None);
    (
        PlannerBackend::codex(daemon.clone(), "card", "track", "runtime").unwrap(),
        daemon,
    )
}

pub(super) fn logical_turn_id() -> calm_types::conversation::LogicalTurnId {
    calm_types::conversation::LogicalTurnId::parse("55555555555555555555555555555555").unwrap()
}

pub(super) fn proof() -> SubmissionDispatchProof {
    serde_json::from_value(serde_json::json!({
        "version":"v2",
        "attempt":{"submission_id":"11111111111111111111111111111111",
                   "attempt_id":"22222222222222222222222222222222"},
        "input_refs":[{"kind":"system","observation_id":"33333333333333333333333333333333"}],
        "render_plan":{"version":"v1","submission_id":"11111111111111111111111111111111",
                       "sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
    }))
    .unwrap()
}

fn items() -> Vec<InputItem> {
    vec![InputItem::Text {
        text: "test input".into(),
    }]
}

async fn initial(backend: &PlannerBackend, proof: &SubmissionDispatchProof) -> InputAttemptReport {
    backend
        .dispatch_input(
            &logical_turn_id(),
            proof.attempt(),
            "thread",
            proof,
            items(),
            &TurnModelSelection::inherit(),
            PlannerPermissionMode::Never,
            None,
        )
        .await
}

#[tokio::test]
async fn local_seal_proves_nondispatch_but_remote_refusal_does_not() {
    let (backend, daemon) = backend().await;
    let proof = proof();
    daemon.thread_seals().seal_for_deletion("thread");
    let report = initial(&backend, &proof).await;
    assert!(matches!(report.delivery(), InputDelivery::NotDispatched(_)));
    assert_eq!(report.attempt(), proof.attempt());
    assert_eq!(daemon.turn_start_count_for_test(), 0);
    daemon.thread_seals().unseal_after_rollback("thread");
    daemon.reject_turn_start_for_test();
    let report = initial(&backend, &proof).await;
    assert!(matches!(report.delivery(), InputDelivery::Unknown(_)));
    assert!(matches!(
        report.outcome(),
        Err(TurnStartFailure::Refused { .. })
    ));
}

#[tokio::test]
async fn accepted_start_survives_seal_and_interrupt_failure_with_exact_correlation() {
    let (backend, daemon) = backend().await;
    let proof = proof();
    let hook = TurnStartReturnHook {
        entered: Arc::default(),
        release: Arc::default(),
    };
    daemon.install_turn_start_return_hook_for_test(hook.clone());
    daemon.fail_turn_interrupt_for_test(true);
    let task = tokio::spawn(async move { initial(&backend, &proof).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), hook.entered.notified())
        .await
        .unwrap();
    daemon.thread_seals().seal_for_deletion("thread");
    hook.release.notify_one();
    let report = task.await.unwrap();
    let InputDelivery::Accepted(receipt) = report.delivery() else {
        panic!("lost native acceptance")
    };
    assert_eq!(receipt.correlation().thread_id(), "thread");
    assert_eq!(
        receipt.correlation().client_id(),
        "11111111111111111111111111111111"
    );
    assert_eq!(receipt.native_turn_id(), "fake-turn-0001");
    assert!(report.outcome().is_err());
    assert_eq!(daemon.turn_start_count_for_test(), 1);
}

#[tokio::test]
async fn supplemental_acceptance_is_scoped_to_original_attempt_and_native_turn() {
    let (backend, daemon) = backend().await;
    let proof = proof();
    daemon.set_active_turn_for_test("thread", "native-turn");
    let report = backend
        .dispatch_supplemental(
            &logical_turn_id(),
            proof.attempt(),
            "thread",
            "native-turn",
            &proof,
            items(),
        )
        .await;
    let InputDelivery::Accepted(receipt) = report.delivery() else {
        panic!("missing steer receipt")
    };
    assert_eq!(receipt.native_turn_id(), "native-turn");
    assert_eq!(receipt.correlation().attempt(), proof.attempt());
    daemon.fail_turn_steer_for_test(true);
    let report = backend
        .dispatch_supplemental(
            &logical_turn_id(),
            proof.attempt(),
            "thread",
            "native-turn",
            &proof,
            items(),
        )
        .await;
    assert!(matches!(report.delivery(), InputDelivery::Unknown(_)));
    assert!(report.outcome().is_err());
    assert_eq!(daemon.steered_turns_for_test().len(), 2);
}

#[tokio::test]
async fn missing_rewind_target_stays_unknown_and_does_not_issue_prompt() {
    let (backend, daemon) = backend().await;
    let mut plan = backend
        .plan_rewind(
            "thread",
            RewindTarget {
                turn_id: "old",
                prompt_client_id: None,
                previous_turn: None,
            },
        )
        .unwrap();
    daemon.answer_revert_turn_not_found_for_test(true);
    let report = backend.apply_rewind("thread", &mut plan).await;
    assert!(matches!(
        report.observation(),
        RewindObservation::Unknown(_)
    ));
    assert!(report.into_outcome().is_err());
    let proof = proof();
    let report = backend
        .dispatch_input(
            &logical_turn_id(),
            proof.attempt(),
            "thread",
            &proof,
            items(),
            &TurnModelSelection::inherit(),
            PlannerPermissionMode::Never,
            Some(&plan),
        )
        .await;
    assert!(matches!(report.delivery(), InputDelivery::NotDispatched(_)));
    assert!(matches!(report.rewind(), RewindObservation::Unknown(_)));
    assert_eq!(daemon.reverted_threads_for_test().len(), 1);
    assert_eq!(daemon.turn_start_count_for_test(), 0);
}

#[tokio::test]
async fn unqualified_native_revert_never_releases_a_replacement_prompt() {
    for response in [
        serde_json::Value::Null,
        serde_json::json!({}),
        serde_json::json!({"thread":{"id":"other"}}),
    ] {
        let (backend, daemon) = backend().await;
        daemon.answer_revert_result_for_test(response.clone());
        let mut plan = backend
            .plan_rewind(
                "thread",
                RewindTarget {
                    turn_id: "old",
                    prompt_client_id: None,
                    previous_turn: None,
                },
            )
            .unwrap();
        let report = backend.apply_rewind("thread", &mut plan).await;
        assert!(
            matches!(report.observation(), RewindObservation::Unknown(_)),
            "{response}"
        );
        assert!(report.into_outcome().is_err(), "{response}");
        let proof = proof();
        let report = backend
            .dispatch_input(
                &logical_turn_id(),
                proof.attempt(),
                "thread",
                &proof,
                items(),
                &TurnModelSelection::inherit(),
                PlannerPermissionMode::Never,
                Some(&plan),
            )
            .await;
        assert!(matches!(report.delivery(), InputDelivery::NotDispatched(_)));
        assert!(matches!(report.rewind(), RewindObservation::Unknown(_)));
        assert_eq!(daemon.reverted_threads_for_test().len(), 1);
        assert_eq!(daemon.turn_start_count_for_test(), 0);
    }
}

#[tokio::test]
async fn confirmed_rewind_is_not_repeated_by_prompt_dispatch() {
    let (backend, daemon) = backend().await;
    let mut plan = backend
        .plan_rewind(
            "thread",
            RewindTarget {
                turn_id: "old",
                prompt_client_id: None,
                previous_turn: None,
            },
        )
        .unwrap();
    let report = backend.apply_rewind("thread", &mut plan).await;
    assert!(matches!(
        report.observation(),
        RewindObservation::Applied(_)
    ));
    let proof = proof();
    let report = backend
        .dispatch_input(
            &logical_turn_id(),
            proof.attempt(),
            "thread",
            &proof,
            items(),
            &TurnModelSelection::inherit(),
            PlannerPermissionMode::Never,
            Some(&plan),
        )
        .await;
    assert!(matches!(report.delivery(), InputDelivery::Accepted(_)));
    assert!(matches!(report.rewind(), RewindObservation::Applied(_)));
    assert_eq!(daemon.reverted_threads_for_test().len(), 1);
}

#[tokio::test]
async fn rewind_evidence_cannot_move_to_another_thread_or_upgrade_legacy() {
    let (backend, daemon) = backend().await;
    let mut plan = backend
        .plan_rewind(
            "thread",
            RewindTarget {
                turn_id: "old",
                prompt_client_id: None,
                previous_turn: None,
            },
        )
        .unwrap();
    backend
        .apply_rewind("thread", &mut plan)
        .await
        .into_outcome()
        .unwrap();
    let mismatched = backend.apply_rewind("different", &mut plan).await;
    assert!(matches!(
        mismatched.observation(),
        RewindObservation::Unknown(_)
    ));
    assert!(mismatched.into_outcome().is_err());
    let mut legacy: BackendRewind = serde_json::from_value(serde_json::json!({
        "provider":"codex", "before_turn_id":"old", "reverted":true,
    }))
    .unwrap();
    let legacy_result = backend.apply_rewind("thread", &mut legacy).await;
    assert!(matches!(
        legacy_result.observation(),
        RewindObservation::Unknown(_)
    ));
    assert!(legacy_result.into_outcome().is_err());
    let proof = proof();
    let report = backend
        .dispatch_input(
            &logical_turn_id(),
            proof.attempt(),
            "different",
            &proof,
            items(),
            &TurnModelSelection::inherit(),
            PlannerPermissionMode::Never,
            Some(&plan),
        )
        .await;
    assert!(matches!(report.delivery(), InputDelivery::NotDispatched(_)));
    assert!(matches!(report.rewind(), RewindObservation::Unknown(_)));
    assert_eq!(daemon.reverted_threads_for_test().len(), 1);
    assert_eq!(daemon.turn_start_count_for_test(), 0);
}

#[tokio::test]
async fn budget_control_sends_only_fixed_notice_to_original_native_turn() {
    let (backend, daemon) = backend().await;
    daemon.set_active_turn_for_test("thread", "original-turn");
    let notice = crate::planner_turn_budget::BudgetControlNotice::for_turn(
        10.try_into().unwrap(),
        "thread",
        "original-turn",
        "original-start",
    )
    .unwrap();
    let report = backend.send_budget_notice(&notice).await;
    assert!(
        matches!(report.delivery(), ControlDelivery::Accepted { native_turn_id } if native_turn_id == "original-turn")
    );
    let calls = daemon.steered_turns_for_test();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, "original-turn");
    assert_eq!(calls[0].3.as_deref(), Some("original-start:budget-warning"));
    assert_eq!(
        serde_json::to_value(&calls[0].2).unwrap(),
        serde_json::json!([{
            "type":"text", "text":"[Trusted Planner turn budget reminder]\nAt most 120 seconds of execution remain. Wrap up now before the hard stop."
        }])
    );
}
