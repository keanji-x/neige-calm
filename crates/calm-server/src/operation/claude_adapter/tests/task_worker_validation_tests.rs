use super::*;

#[tokio::test]
async fn task_worker_claude_new_requests_are_rejected_before_preparation() {
    let harness = claude_worker_harness().await;
    let payload = claude_worker_payload(&harness.track_id, "unsupported");
    let error = harness.adapter.validate(&payload).await.unwrap_err();
    assert!(matches!(error, CalmError::BadRequest(_)));
    assert!(error.reason().contains("unsupported_worker_provider"));
}
