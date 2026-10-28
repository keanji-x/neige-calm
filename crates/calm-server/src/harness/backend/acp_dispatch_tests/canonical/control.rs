//! Canonical control consumes the original driver and the actual caller's writer gate.
#![cfg(test)]
use super::*;

#[tokio::test]
async fn canonical_acp_stop_rechecks_source_at_writer_and_signals_one_exact_driver() {
    let fx = Fixture::new("hold").await;
    fx.dispatch().await.unwrap();
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let target = crate::harness::input_dispatch::execution_target_tx(
        &mut tx,
        &fx.owner.card,
        &fx.owner.worker,
        &AgentProvider::OpenCode,
    )
    .await
    .unwrap()
    .unwrap();
    tx.commit().await.unwrap();
    let admission = crate::harness::native_admission::NativePromptAdmission::new(
        || Ok(()),
        || Box::pin(async { Ok(()) }),
    );
    let mut wrong = target.clone();
    wrong.logical_turn_id = LogicalTurnId::parse(&crate::model::new_id()).unwrap();
    assert!(
        fx.backend
            .interrupt_execution(&wrong, &admission)
            .await
            .is_err()
    );
    let checks = std::sync::atomic::AtomicUsize::new(0);
    let refused = crate::harness::native_admission::NativePromptAdmission::new(
        || Ok(()),
        || {
            Box::pin(async {
                if checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    Ok(())
                } else {
                    Err(CalmError::Forbidden(
                        "source changed behind cancel writer".into(),
                    ))
                }
            })
        },
    );
    assert!(
        fx.backend
            .interrupt_execution(&target, &refused)
            .await
            .unwrap_err()
            .to_string()
            .contains("source changed")
    );
    assert_eq!(checks.load(std::sync::atomic::Ordering::SeqCst), 2);
    let cancels = || {
        std::fs::read_to_string(fx.dir.path().join("requests.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["method"]
                    == "session/cancel"
            })
            .count()
    };
    assert_eq!(cancels(), 0);
    assert!(fx.complete().await.is_none());
    fx.backend
        .interrupt_execution(&target, &admission)
        .await
        .unwrap();
    fx.wait_idle().await;
    assert_eq!(cancels(), 1);
    fx.wait_end().await;
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_acp_refused_stop_without_original_driver_preserves_held_execution() {
    refused_stop_preserves_held_execution(false).await;
}

#[tokio::test]
async fn canonical_acp_refused_stop_after_restart_preserves_unknown_keep_and_claims() {
    refused_stop_preserves_held_execution(true).await;
}

async fn refused_stop_preserves_held_execution(restarted: bool) {
    use calm_truth::session_repo::SessionRepo as _;
    let mut fx = Fixture::new(if restarted { "lost" } else { "hold" }).await;
    let mut harness = start_control_loop(&fx).await;
    if restarted {
        fx.wait_idle().await;
        harness.shutdown().await.unwrap();
    } else {
        fx.session.abort_driver_for_test().await;
        fx.wait_driver_finished().await;
    }
    fx.keep_unknown(&fx.submission, "keep-before-stop").await;
    let receipt = fx.receipt_bytes(&fx.submission).await;
    if restarted {
        let (backend, session, _held) = session_for(&fx.owner, &fx.dir);
        fx.backend = backend;
        fx.session = session;
        harness = start_control_loop(&fx).await;
    }
    let frame = || async {
        let row = fx
            .owner
            .repo
            .session_get(&calm_types::worker::WorkerSessionId(
                fx.owner.worker.clone(),
            ))
            .await
            .unwrap()
            .unwrap();
        serde_json::from_value::<crate::harness::snapshot::CanonicalHarnessSnapshot>(
            row.handle_state_json.unwrap(),
        )
        .unwrap()
    };
    let original = frame().await;
    assert_eq!(
        original.header.phase,
        calm_types::harness::HarnessPhaseTag::TurnRunning
    );
    assert!(original.canonical_inputs.active.is_some());
    assert!(matches!(
        harness.stop_canonical(fx.source.clone()).await,
        Err(CalmError::Conflict(_))
    ));
    // Observe beyond the configured Stop deadline: a refused local control must
    // never manufacture an interrupt_timeout failure on the held execution.
    tokio::time::sleep(Duration::from_millis(250)).await;
    let after = frame().await;
    assert_eq!(after.header.phase, original.header.phase);
    assert!(after.header.interruption_intent.is_none());
    assert_eq!(after.header.wedged_reason, original.header.wedged_reason);
    assert_eq!(after.canonical_inputs, original.canonical_inputs);
    assert_eq!(fx.receipt_bytes(&fx.submission).await, receipt);
    assert_eq!(fx.prompts(), 1);
    assert_eq!(cancel_count(&fx), 0);
    // Aborting the original driver deliberately leaves an unjoinable owner;
    // ordinary shutdown remains best-effort for that existing failure.
    let _ = harness.shutdown().await;
}

pub(super) fn cancel_count(fx: &Fixture) -> usize {
    fx.requests()
        .iter()
        .filter(|request| request["method"] == "session/cancel")
        .count()
}

#[tokio::test]
async fn canonical_acp_prepared_stop_never_cancels_a_successor_reusing_native_session() {
    let fx = Fixture::new("settlement").await;
    fx.dispatch().await.unwrap();
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let target = crate::harness::input_dispatch::execution_target_tx(
        &mut tx,
        &fx.owner.card,
        &fx.owner.worker,
        &fx.provider,
    )
    .await
    .unwrap()
    .unwrap();
    tx.commit().await.unwrap();
    let prepared = fx.backend.prepare_interrupt(&target).await.unwrap();
    std::fs::write(fx.dir.path().join("release-settlement"), "").unwrap();
    fx.wait_idle().await;
    fx.wait_end().await;
    std::fs::write(fx.dir.path().join("scenario"), "hold").unwrap();
    let successor = fx.claim_successor().await;
    fx.dispatch_submission(&successor).await;
    let requests = fx.requests();
    let prompts: Vec<_> = requests
        .iter()
        .filter(|request| request["method"] == "session/prompt")
        .collect();
    assert_eq!(prompts.len(), 2);
    assert_eq!(
        prompts[0]["params"]["sessionId"],
        prompts[1]["params"]["sessionId"]
    );
    assert_ne!(prompts[0]["_pid"], prompts[1]["_pid"]);
    let admission = crate::harness::native_admission::NativePromptAdmission::new(
        || Ok(()),
        || Box::pin(async { Ok(()) }),
    );
    assert!(matches!(
        prepared.send(&admission).await,
        Err(CalmError::Conflict(_))
    ));
    assert_eq!(cancel_count(&fx), 0);
    assert!(fx.complete().await.is_none());
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_acp_prepared_stop_allows_concurrent_driver_observation() {
    let fx = Fixture::new("hold").await;
    fx.dispatch().await.unwrap();
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let target = crate::harness::input_dispatch::execution_target_tx(
        &mut tx,
        &fx.owner.card,
        &fx.owner.worker,
        &fx.provider,
    )
    .await
    .unwrap()
    .unwrap();
    tx.commit().await.unwrap();
    let prepared = fx.backend.prepare_interrupt(&target).await.unwrap();
    let observing = fx.session.lock_driver_for_test().await;
    let admission = crate::harness::native_admission::NativePromptAdmission::new(
        || Ok(()),
        || Box::pin(async { Ok(()) }),
    );
    let result = tokio::time::timeout(Duration::from_secs(2), prepared.send(&admission)).await;
    drop(observing);
    result
        .expect("a concurrent END observer must not block the cancel writer")
        .unwrap();
    fx.wait_idle().await;
    assert_eq!(cancel_count(&fx), 1);
    fx.wait_end().await;
    fx.session.shutdown().await.unwrap();
}

async fn start_control_loop(fx: &Fixture) -> crate::harness::PlannerHarness {
    started(fx, super::compact::control_harness(fx).await).await
}

/// Run the real canonical loop until it publishes the fixture's running original.
pub(super) async fn started(
    fx: &Fixture,
    (harness, registry): (
        crate::harness::PlannerHarness,
        crate::harness::HarnessRegistry,
    ),
) -> crate::harness::PlannerHarness {
    let application = crate::auth::ValidatedApplicationAuth::from_state(fx.auth.clone()).unwrap();
    registry
        .activate_maintenance_execution(application.execution)
        .unwrap();
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let refs = submission_plan_tx(&mut tx, &fx.submission)
        .await
        .unwrap()
        .plan
        .input_refs()
        .unwrap();
    let [AcceptedInputRef::Human { message }] = refs.as_slice() else {
        panic!("human");
    };
    tx.commit().await.unwrap();
    harness
        .observe_canonical_source(message.clone(), fx.source.clone())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
            let carrier = current_input_fact_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
                .await
                .unwrap();
            let running =
                carrier.phase().unwrap() == calm_types::harness::HarnessPhaseTag::TurnRunning;
            tx.commit().await.unwrap();
            if running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("real canonical loop must dispatch and publish the running execution");
    harness
}
