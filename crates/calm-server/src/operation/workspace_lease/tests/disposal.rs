use super::*;
use crate::operation::terminal_disposal::{Scope, require_safe_tx};
use crate::operation::workspace_lease::execution_guard::{
    self, ExecutionWriteGuard, NativeProvider,
};

#[tokio::test]
async fn disposal_transaction_retains_native_and_forge_execution_references() {
    let cwd = tempfile::tempdir().unwrap();
    let (repo, track, card) = lease_fixture(cwd.path()).await;
    execution_guard::bind_execution(
        repo.pool(),
        NativeProvider::Codex,
        &card,
        "thread",
        cwd.path().to_str().unwrap(),
    )
    .await
    .unwrap();
    let native = ExecutionWriteGuard::acquire_native(
        repo.pool(),
        &card,
        "thread",
        "",
        NativeProvider::Codex,
    )
    .await
    .unwrap();
    let area: String = sqlx::query_scalar("SELECT area_id FROM tracks WHERE id=?1")
        .bind(&track)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    let scopes = [
        Scope::Card(card.clone()),
        Scope::Track(track.clone()),
        Scope::Area(area),
    ];
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    execution_guard::acquire_execution_write_tx(
        &mut tx,
        &track,
        &card,
        "forge-child",
        "forge",
        cwd.path(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    for scope in &scopes {
        let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
        let result = require_safe_tx(&mut tx, scope).await;
        tx.rollback().await.unwrap();
        assert!(
            result.is_err(),
            "destructive scope must retain live native/Forge references"
        );
    }
    native.rejected().await.unwrap();
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    assert!(
        require_safe_tx(&mut tx, &scopes[0]).await.is_err(),
        "the surviving Forge descendant keeps its fence"
    );
    tx.rollback().await.unwrap();
    execution_guard::release_stopped_execution(repo.pool(), "forge", "forge-child")
        .await
        .unwrap();
    for scope in &scopes {
        let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
        require_safe_tx(&mut tx, scope).await.unwrap();
        tx.rollback().await.unwrap();
    }
}
