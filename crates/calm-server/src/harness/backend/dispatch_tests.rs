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
            &crate::shared_codex_appserver::NativePromptAdmission::new(
                || Ok(()),
                || Box::pin(async { Ok(()) }),
            ),
        )
        .await
}

#[tokio::test]
async fn local_seal_and_remote_refusal_prove_nondelivery() {
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
    assert!(matches!(report.delivery(), InputDelivery::NotDispatched(_)));
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
    let report = backend
        .send_budget_notice(
            &notice,
            BudgetControlScope::Historical,
            &crate::shared_codex_appserver::NativePromptAdmission::new(
                || Ok(()),
                || Box::pin(async { Ok(()) }),
            ),
        )
        .await;
    assert!(
        matches!(report.delivery(), ControlDelivery::Accepted { native_turn_id } if native_turn_id == "original-turn")
    );
    let calls = daemon.steered_turns_for_test();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, "original-turn");
    assert_eq!(calls[0].3.as_deref(), Some("original-start:budget-warning"));
    assert!(matches!(calls[0].2.as_slice(), [InputItem::Text { text }]
        if text == "[Trusted Planner turn budget reminder]\nAt most 120 seconds of execution remain. Wrap up now before the hard stop."));
}
