//! ACP consumes the shared carrier and park owners at its actual boundaries.
use super::*;
use calm_truth::session_repo::SessionRepo;

#[tokio::test]
async fn canonical_acp_cancel_before_publication_refuses_park_until_cleanup_finishes() {
    use crate::acp_planner::test_seams::{IssueStage, pause_issue};
    for stage in [IssueStage::AfterSpawn, IssueStage::BeforeReceipt] {
        let fx = Arc::new(Fixture::new("reply").await);
        let pause = pause_issue(&fx.owner.worker, stage);
        let dispatch = tokio::spawn({
            let fx = fx.clone();
            async move { fx.dispatch().await }
        });
        tokio::time::timeout(Duration::from_secs(20), pause.entered.notified())
            .await
            .unwrap();
        // Revoke must acquire this real writer before it can stop the native process.
        let tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
        dispatch.abort();
        match dispatch.await {
            Err(error) => assert!(error.is_cancelled()),
            Ok(_) => panic!("dispatch survived cancellation"),
        }
        assert!(
            fx.session
                .active_turn_id_for_thread(&fx.owner.thread)
                .is_none()
        );
        let acquired = fx.session.carrier_idle_for_detach();
        let acquisition_refused = acquired.is_none();
        // If acquisition incorrectly succeeds, exercise the actual guard recheck too.
        let recheck_refused = match acquired {
            Some(park) => {
                !crate::harness::backend::CarrierIdleGuard::Acp(&fx.session, park).recheck()
            }
            None => !fx.session.carrier_idle(), // Same predicate used by ACP guard recheck.
        };
        tx.rollback().await.unwrap();
        // Observe completion without closing the session or joining away the retained slot.
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Some(guard) = fx
                    .backend
                    .carrier_idle_for_detach(Some(&fx.owner.thread))
                    .await
                {
                    assert!(guard.recheck());
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        fx.session.shutdown().await.unwrap();
        assert!(
            acquisition_refused,
            "unfinished {stage:?} cleanup granted a park"
        );
        assert!(
            recheck_refused,
            "unfinished {stage:?} cleanup passed recheck"
        );
    }
}

#[tokio::test]
async fn canonical_acp_parked_issue_refuses_before_sql_without_native_effects() {
    let fx = Fixture::new("reply").await;
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let identity = submission_identity_tx(&mut tx, &fx.submission)
        .await
        .unwrap();
    let stored = submission_plan_tx(&mut tx, &fx.submission).await.unwrap();
    let attempt = InputAttempt {
        submission_id: fx.submission.clone(),
        attempt_id: InputAttemptId::parse(&crate::model::new_id()).unwrap(),
    };
    let proof =
        SubmissionDispatchProof::from_stored_plan(attempt, &stored.plan, stored.original_digest)
            .unwrap();
    let park = fx.session.carrier_idle_for_detach().unwrap();
    let admission = crate::harness::native_admission::NativePromptAdmission::new(
        || Ok(()),
        || Box::pin(async { panic!("parked entry must not reach source admission") }),
    );
    let outcome = tokio::time::timeout(
        Duration::from_secs(1),
        fx.session.start_canonical(
            &fx.owner.thread,
            vec![],
            fx.submission.as_str(),
            &TurnModelSelection::inherit(),
            PlannerPermissionMode::Never,
            crate::harness::backend::ExecutionInput::canonical(&identity.logical_turn_id, &proof),
            &admission,
        ),
    )
    .await
    .expect("parked ACP must not wait on the SQL writer");
    assert!(
        matches!(outcome, crate::acp_planner::session::CanonicalStart::UncertainReceipt(error)
        if error.to_string().contains("parked"))
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM acp_submissions")
        .fetch_one(&mut **tx)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert!(!fx.dir.path().join("requests.jsonl").exists());
    tx.rollback().await.unwrap();
    drop(park);
    // Parking is temporary; the original immutable claim can still cross once.
    fx.dispatch().await.unwrap();
    fx.wait_idle().await;
    fx.wait_end().await;
    assert_eq!(fx.prompts(), 1);
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_acp_retirement_at_receipt_and_writer_boundaries_never_resends() {
    use crate::acp_planner::test_seams::{IssueStage, pause_issue};
    for after_receipt in [false, true] {
        let fx = Arc::new(Fixture::new("reply").await);
        let pause = pause_issue(
            &fx.owner.worker,
            if after_receipt {
                IssueStage::AfterReceipt
            } else {
                IssueStage::BeforeReceipt
            },
        );
        let mut dispatch = tokio::spawn({
            let fx = fx.clone();
            async move { fx.dispatch().await }
        });
        tokio::select! {
            _ = pause.entered.notified() => {},
            _ = &mut dispatch => panic!("receipt boundary missing"),
            _ = tokio::time::sleep(Duration::from_secs(20)) => panic!("receipt boundary timeout"),
        }
        // Leave the old row and checkpoint intact: only the current-card pointer
        // changes. State-only or receipt-only validation would accept this owner.
        sqlx::query("UPDATE cards SET session_id=NULL WHERE id=?1")
            .bind(&fx.owner.card)
            .execute(fx.owner.repo.pool())
            .await
            .unwrap();
        pause.release.notify_one();
        assert!(
            dispatch.await.unwrap().is_err(),
            "retired carrier cannot record a report"
        );
        fx.wait_driver_finished().await;
        assert_eq!(fx.prompts(), 0);
        let receipt = crate::db::sqlite::acp_submission_get(
            fx.owner.repo.pool(),
            &fx.owner.worker,
            fx.submission.as_str(),
        )
        .await
        .unwrap();
        if after_receipt {
            let receipt = receipt.unwrap();
            assert!(matches!(
                receipt.state,
                crate::db::sqlite::AcpSubmissionState::Unknown
            ));
            let before = receipt.input_json.clone();
            // A retained receipt stays historical evidence, never a new send grant.
            assert!(fx.dispatch().await.is_err());
            let after = crate::db::sqlite::acp_submission_get(
                fx.owner.repo.pool(),
                &fx.owner.worker,
                fx.submission.as_str(),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(after.input_json, before);
        } else {
            assert!(
                receipt.is_none(),
                "receipt insertion must resolve the current carrier"
            );
        }
        assert!(
            crate::harness::input_dispatch::complete_once(
                fx.owner.repo.as_ref(),
                &fx.backend,
                &fx.owner.card,
                &fx.owner.worker,
            )
            .await
            .is_err()
        );
        assert_eq!(fx.prompts(), 0);
        fx.session.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn canonical_acp_end_is_fact_on_failed_current_carrier_and_refuses_retirement() {
    let fx = Fixture::new("reply").await;
    fx.dispatch().await.unwrap();
    fx.wait_idle().await;
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
    tokio::time::timeout(Duration::from_secs(10), async {
        while fx
            .backend
            .qualify_execution_end(&target)
            .await
            .unwrap()
            .is_none()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let park = fx.session.carrier_idle_for_detach().unwrap();
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    crate::db::sqlite::session_fail_if_active_runtime_tx(&mut tx, &fx.owner.worker)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(
        fx.backend
            .qualify_execution_end(&target)
            .await
            .unwrap()
            .is_some(),
        "END is ingress, permitted on a parked, failed current carrier"
    );
    assert!(fx.complete().await.is_some());
    let row = fx
        .owner
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(
            fx.owner.worker.clone(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.state,
        crate::session_projection_repo::WorkerSessionState::Failed
    );
    assert!(
        fx.dispatch().await.is_err(),
        "END cannot reactivate execution"
    );
    sqlx::query("UPDATE cards SET session_id=NULL WHERE id=?1")
        .bind(&fx.owner.card)
        .execute(fx.owner.repo.pool())
        .await
        .unwrap();
    assert!(
        matches!(
            fx.backend.qualify_execution_end(&target).await,
            Ok(None) | Err(_)
        ),
        "a finished receipt and driver cannot certify a retired carrier"
    );
    drop(park);
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn c4_canonical_acp_issue_refuses_outstanding_provider_execution() {
    let fx = Fixture::new("reply").await;
    // A retained owner claim is an admission fence even if its execution is unclassified.
    sqlx::query("UPDATE worker_sessions SET provider_execution_json=? WHERE id=?")
        .bind(
            serde_json::json!({"version":1,"execution_id":crate::model::new_id(),
            "purpose":"RewindCheck","release":"Held","decision":"Observing"})
            .to_string(),
        )
        .bind(&fx.owner.worker)
        .execute(fx.owner.repo.pool())
        .await
        .unwrap();
    let report = fx.dispatch().await.unwrap();
    let error = report
        .report
        .outcome()
        .as_ref()
        .expect_err("retained execution must fence ACP issuance");
    assert!(error.to_string().contains("provider execution"), "{error}");
    assert_eq!(fx.prompts(), 0);
    assert!(!fx.dir.path().join("requests.jsonl").exists());
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM acp_submissions")
        .fetch_one(fx.owner.repo.pool())
        .await
        .unwrap();
    assert_eq!(receipts, 0);
    assert!(
        fx.session.shutdown().await.is_err(),
        "an unclassified execution remains fenced"
    );
}
