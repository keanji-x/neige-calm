//! Canonical cleanup requires evidence from the original shared attachment.
use super::*;

#[tokio::test]
async fn canonical_antigravity_normal_end_has_body_free_receipt_and_exact_end() {
    let fx = Fixture::with_provider(AgentProvider::Antigravity, "reply", false).await;
    fx.dispatch().await.unwrap();
    fx.wait_idle().await;
    assert!(matches!(
        crate::db::sqlite::AcpSubmissionInput::decode(&fx.receipt_bytes(&fx.submission).await.1)
            .unwrap(),
        crate::db::sqlite::AcpSubmissionInput::Canonical(_)
    ));
    assert!(fx.complete().await.is_some());
    assert!(fx.complete().await.is_none());
    assert_eq!(fx.prompts(), 1);
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_antigravity_confirmed_close_allows_keep_and_same_process_continuation() {
    let fx = Fixture::with_provider(AgentProvider::Antigravity, "shared-unknown", false).await;
    fx.dispatch().await.unwrap();
    fx.wait_idle().await;
    let before = fx.receipt_bytes(&fx.submission).await;
    assert!(fx.complete().await.is_none());
    fx.keep_unknown(&fx.submission, "keep-original").await;
    assert!(fx.backend.settle_kept_execution().await.unwrap().is_some());
    assert!(fx.backend.settle_kept_execution().await.unwrap().is_none());
    std::fs::write(fx.dir.path().join("scenario"), "reply").unwrap();
    let successor = fx.claim_successor().await;
    fx.dispatch_submission(&successor).await;
    fx.wait_idle().await;
    assert!(fx.complete().await.is_some());
    assert_eq!(fx.receipt_bytes(&fx.submission).await, before);
    let requests = fx.requests();
    let prompts: Vec<_> = requests
        .iter()
        .filter(|r| r["method"] == "session/prompt")
        .collect();
    assert_eq!(prompts.len(), 2);
    assert_eq!(prompts[0]["_pid"], prompts[1]["_pid"]);
    assert_eq!(
        prompts[0]["params"]["sessionId"],
        prompts[1]["params"]["sessionId"]
    );
    assert!(requests.iter().any(|r| r["method"] == "session/close"
        && r["params"]["sessionId"] == prompts[0]["params"]["sessionId"]));
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_antigravity_unconfirmed_revocation_cannot_manufacture_keep_cleanup() {
    let fx = Fixture::with_provider(AgentProvider::Antigravity, "shared-unknown", false).await;
    fx.owner.repo.db().writer().execute(sqlx::query(
        "CREATE TRIGGER block_shared_revocation BEFORE UPDATE OF mcp_token_hash ON worker_sessions
         WHEN OLD.mcp_token_hash IS NOT NULL AND NEW.mcp_token_hash IS NULL
         BEGIN SELECT RAISE(ABORT, 'revocation unavailable'); END"
    )).await.unwrap();
    fx.dispatch().await.unwrap();
    fx.wait_idle().await;
    fx.keep_unknown(&fx.submission, "keep-without-close").await;
    assert!(fx.backend.settle_kept_execution().await.unwrap().is_none());
    let joins: i64 =
        sqlx::query_scalar("SELECT count(*) FROM acp_normal_joins WHERE worker_session_id=?")
            .bind(&fx.owner.worker)
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
    assert_eq!(joins, 0);
    assert!(fx.complete().await.is_none());
    assert_eq!(fx.prompts(), 1);
    fx.owner
        .repo
        .db()
        .writer()
        .execute(sqlx::query("DROP TRIGGER block_shared_revocation"))
        .await
        .unwrap();
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_antigravity_unconfirmed_process_stop_cannot_release_kept_input() {
    let fx = Fixture::with_provider(AgentProvider::Antigravity, "lost", false).await;
    let fired = crate::test_seams::install_operation_fault_for_test(
        "acp-control-reap",
        &fx.session.stop_fault_scope_for_test(),
    );
    fx.dispatch().await.unwrap();
    tokio::time::timeout(Duration::from_secs(20), fired.notified())
        .await
        .unwrap();
    fx.wait_idle().await;
    fx.keep_unknown(&fx.submission, "keep-without-reap").await;
    assert!(fx.backend.settle_kept_execution().await.unwrap().is_none());
    let joins: i64 =
        sqlx::query_scalar("SELECT count(*) FROM acp_normal_joins WHERE worker_session_id=?")
            .bind(&fx.owner.worker)
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
    assert_eq!(joins, 0);
    assert!(fx.complete().await.is_none());
    assert_eq!(fx.prompts(), 1);
    fx.session.shutdown().await.unwrap();
}
