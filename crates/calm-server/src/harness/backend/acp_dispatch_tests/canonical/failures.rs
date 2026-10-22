//! Native terminal frames cannot substitute for required cleanup or receipt persistence.
#![cfg(test)]
use super::*;

#[tokio::test]
async fn canonical_acp_revoke_stop_and_receipt_failures_never_qualify_end() {
    #[derive(Clone, Copy)]
    enum Failure {
        Revoke,
        Stop,
        Receipt,
    }
    for failure in [Failure::Revoke, Failure::Stop, Failure::Receipt] {
        let fx = Fixture::new("reply").await;
        let pause = crate::acp_planner::test_seams::pause_after_fence(&fx.owner.worker);
        fx.dispatch().await.unwrap();
        tokio::time::timeout(Duration::from_secs(20), pause.entered.notified())
            .await
            .unwrap();
        match failure {
            Failure::Revoke => {
                sqlx::query(concat!(
                    "CREATE TRIGGER refuse_revoke BEFORE UPDATE OF mcp_token_hash ON worker_sessions ",
                    "WHEN NEW.mcp_token_hash IS NULL BEGIN SELECT RAISE(ABORT,'refuse revoke'); END"
                ))
                    .execute(fx.owner.repo.pool()).await.unwrap();
            }
            Failure::Stop => {
                crate::planner_process::fail_claude_planner_stop_for_test(&fx.owner.worker)
            }
            Failure::Receipt => {
                sqlx::query("CREATE TRIGGER refuse_receipt BEFORE UPDATE ON acp_submissions BEGIN SELECT RAISE(ABORT,'refuse receipt'); END")
                    .execute(fx.owner.repo.pool()).await.unwrap();
            }
        }
        pause.release.notify_one();
        fx.wait_idle().await;
        assert!(fx.complete().await.is_none());
        let state: String = sqlx::query_scalar("SELECT state FROM acp_submissions")
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
        assert_ne!(state, "completed");
        fx.keep_unknown(&fx.submission, "keep-failed-cleanup").await;
        assert!(fx.backend.settle_kept_execution().await.unwrap().is_none());
        assert_eq!(fx.prompts(), 1);
        match failure {
            Failure::Revoke => {
                sqlx::query("DROP TRIGGER refuse_revoke")
                    .execute(fx.owner.repo.pool())
                    .await
                    .unwrap();
            }
            Failure::Stop => {
                crate::planner_process::clear_claude_planner_stop_failure_for_test(&fx.owner.worker)
            }
            Failure::Receipt => {
                sqlx::query("DROP TRIGGER refuse_receipt")
                    .execute(fx.owner.repo.pool())
                    .await
                    .unwrap();
            }
        }
        fx.session.shutdown().await.unwrap();
        assert!(fx.complete().await.is_none());
    }
}

#[tokio::test]
async fn canonical_acp_uncertain_receipt_commit_is_unknown_even_when_sqlite_rolled_back() {
    let fx = Fixture::new("reply").await;
    sqlx::raw_sql("CREATE TABLE commit_fault(worker TEXT REFERENCES worker_sessions(id) DEFERRABLE INITIALLY DEFERRED);
        CREATE TRIGGER defer_receipt_failure AFTER INSERT ON acp_submissions
        BEGIN INSERT INTO commit_fault VALUES('missing-worker'); END;")
        .execute(fx.owner.repo.pool()).await.unwrap();
    let report = fx.dispatch().await.unwrap();
    assert!(matches!(
        report.report.delivery(),
        InputDelivery::Unknown(_)
    ));
    assert!(report.report.outcome().is_err());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM acp_submissions")
        .fetch_one(fx.owner.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "failed COMMIT rolled back, but is never factual nondispatch"
    );
    assert_eq!(fx.prompts(), 0);
    assert!(fx.complete().await.is_none());
    assert!(fx.dispatch().await.is_err());
    assert_eq!(fx.prompts(), 0);
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_acp_completed_receipt_cannot_bypass_failed_canonical_report_commit() {
    let fx = Fixture::new("reply").await;
    sqlx::query("CREATE TRIGGER refuse_canonical_report BEFORE UPDATE OF handle_state_json ON worker_sessions
        WHEN json_extract(NEW.handle_state_json,'$.canonical_inputs.attempts[0].delivery.kind')='unknown'
        BEGIN SELECT RAISE(ABORT,'canonical report failed'); END")
        .execute(fx.owner.repo.pool()).await.unwrap();
    assert!(fx.dispatch().await.is_err());
    fx.wait_idle().await;
    let receipt = crate::db::sqlite::acp_submission_get(
        fx.owner.repo.pool(),
        &fx.owner.worker,
        fx.submission.as_str(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        receipt.state,
        crate::db::sqlite::AcpSubmissionState::Completed
    ));
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let carrier = current_input_fact_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
        .await
        .unwrap();
    let before = carrier.checkpoint().clone();
    assert!(matches!(
        before.attempt(&fx.submission).unwrap().delivery,
        InputDeliveryFact::CrossingUnknown
    ));
    tx.commit().await.unwrap();
    assert!(
        fx.complete().await.is_none(),
        "END must not bypass the canonical fact owner"
    );
    assert!(fx.dispatch().await.is_err());
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let carrier = current_input_fact_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
        .await
        .unwrap();
    assert_eq!(carrier.checkpoint(), &before);
    tx.commit().await.unwrap();
    assert_eq!(fx.prompts(), 1);
    fx.session.shutdown().await.unwrap();
}
