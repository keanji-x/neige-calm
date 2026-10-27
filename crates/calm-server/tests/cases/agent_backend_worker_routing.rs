//! Session-owned routing through production pool construction, without selection or writes.
use super::agent_backend_pool::{bind_fixture, pool};
use super::agent_backend_upgrade::multi_backend_cfg;
use super::*;

#[tokio::test]
async fn agent_backend_session_and_thread_route_to_fixture_binding() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let backends = pool(&multi_backend_cfg(&root), repo.clone()).await;
    let card = seed_card(&repo, 1).await;
    let session = seed_runtime_thread(&repo, &card, "bound-thread").await;
    assert!(Arc::ptr_eq(
        backends.for_session(&session).await.unwrap(),
        backends.default_only()
    ));
    bind_fixture(&repo, &session, "bound-thread", "other").await;
    assert!(Arc::ptr_eq(
        backends.for_session(&session).await.unwrap(),
        &backends.instances()[1]
    ));
    assert!(Arc::ptr_eq(
        backends.for_thread("bound-thread").await.unwrap(),
        &backends.instances()[1]
    ));
    // Routing only reads the existing binding. No new publication or selection occurs.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM worker_session_backends")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
}
