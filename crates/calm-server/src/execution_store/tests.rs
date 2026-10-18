use super::*;
use calm_truth::db::{Repo, sqlite::SqlxRepo};
use workspace_runtime::execution::test_support::{self, StoreHarness};

async fn fixture() -> (SqliteExecutionStore, SqlitePool) {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let pool = repo.sqlite_pool().unwrap();
    (SqliteExecutionStore::new(pool.clone()), pool)
}
#[async_trait]
impl StoreHarness for SqliteExecutionStore {
    async fn begin(&self) -> Transaction<'_, Sqlite> {
        self.pool.begin().await.unwrap()
    }
    async fn commit(&self, tx: Transaction<'_, Sqlite>) {
        tx.commit().await.unwrap();
    }
    async fn rollback(&self, tx: Transaction<'_, Sqlite>) {
        tx.rollback().await.unwrap();
    }
}
mod port_contract {
    #[tokio::test]
    async fn store() {
        let (store, _) = super::fixture().await;
        workspace_runtime::execution::test_support::port_contract::store(&store).await;
    }
}
async fn inserted(store: &SqliteExecutionStore, pool: &SqlitePool) -> ExecutionIntent {
    let intent = test_support::intent("/".into());
    let mut tx = pool.begin().await.unwrap();
    store.insert(&mut tx, &intent).await.unwrap();
    tx.commit().await.unwrap();
    intent
}
#[tokio::test]
async fn store_admit_rolls_back_with_domain_write() {
    let (store, pool) = fixture().await;
    let engine = Engine::new(
        store.clone(),
        test_support::ScriptedExecutor::new(Default::default(), []),
    );
    let intent = test_support::intent("/".into());
    let id = intent.metadata.id.clone();
    let mut tx = pool.begin().await.unwrap();
    // A real domain-table write shares the caller's transaction with Engine::admit.
    sqlx::query(
        "INSERT INTO areas (id, name, color, sort, created_at, updated_at)
         VALUES ('execution-domain', 'Execution domain', '#000000', 0, 0, 0)",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    let _ticket = engine.admit(&mut tx, intent).await.unwrap();
    tx.rollback().await.unwrap();
    assert!(store.load(&id).await.unwrap().is_none());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM areas WHERE id = 'execution-domain'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}
#[tokio::test]
async fn store_stop_settlement_keeps_secrets_check() {
    let (store, pool) = fixture().await;
    let intent = inserted(&store, &pool).await;
    let id = &intent.metadata.id;
    let mut tx = pool.begin().await.unwrap();
    store.request(&mut tx, id, Desired::Stop).await.unwrap();
    tx.commit().await.unwrap();
    store
        .settle(id, &Settlement::Journal(ExecutionState::Revoked))
        .await
        .unwrap();
    let Some(Stored::Active {
        secrets,
        desired,
        settlement,
        ..
    }) = store.load(id).await.unwrap()
    else {
        panic!("Stop must retain secrets")
    };
    assert!(secrets.ticket.matches(&intent.secrets.ticket));
    assert_eq!(desired, Desired::Stop);
    assert_eq!(
        settlement,
        Some(Settlement::Journal(ExecutionState::Revoked))
    );
    assert!(store.pending().await.unwrap().is_empty());
}
#[tokio::test]
async fn store_pending_includes_settled_retire() {
    let (store, pool) = fixture().await;
    let intent = inserted(&store, &pool).await;
    let id = &intent.metadata.id;
    assert_eq!(store.pending().await.unwrap(), vec![id.clone()]);
    store
        .settle(id, &Settlement::Journal(ExecutionState::Revoked))
        .await
        .unwrap();
    assert!(store.pending().await.unwrap().is_empty());
    let mut tx = pool.begin().await.unwrap();
    store.request(&mut tx, id, Desired::Retire).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(store.pending().await.unwrap(), vec![id.clone()]);
    store.finish(id).await.unwrap();
    assert!(store.pending().await.unwrap().is_empty());
    assert!(matches!(
        store.load(id).await.unwrap(),
        Some(Stored::Finished { .. })
    ));
}
#[tokio::test]
async fn store_triggers_reject_rewrites() {
    let (store, pool) = fixture().await;
    let intent = inserted(&store, &pool).await;
    let id = &intent.metadata.id;
    for assignment in [
        "execution_id = 'rewritten'",
        "track_id = 'rewritten'",
        "alias = 'rewritten'",
        "intent_json = '{}'",
        "secrets_json = '{}'",
        "secrets_json = NULL",
        "settled_at_ms = 1",
        "settlement_json = '{}'",
    ] {
        let query = format!("UPDATE workspace_executions SET {assignment} WHERE execution_id = ?");
        assert!(
            sqlx::query(&query)
                .bind(id.as_str())
                .execute(&pool)
                .await
                .is_err(),
            "accepted {assignment}"
        );
    }
    // Exact rewrites are permitted.
    sqlx::query(
        "UPDATE workspace_executions SET execution_id=execution_id, track_id=track_id,
         alias=alias, intent_json=intent_json, secrets_json=secrets_json, desired=desired",
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut same_alias = test_support::intent("/".into());
    same_alias.metadata.alias = intent.metadata.alias.clone();
    let mut tx = pool.begin().await.unwrap();
    assert!(store.insert(&mut tx, &same_alias).await.is_err());
    tx.rollback().await.unwrap();
    for (desired, rejected) in [(Desired::Stop, "run"), (Desired::Retire, "stop")] {
        let mut tx = pool.begin().await.unwrap();
        store.request(&mut tx, id, desired).await.unwrap();
        tx.commit().await.unwrap();
        assert!(
            sqlx::query("UPDATE workspace_executions SET desired=? WHERE execution_id=?")
                .bind(rejected)
                .bind(id.as_str())
                .execute(&pool)
                .await
                .is_err()
        );
    }
    // Retiring frees the current-alias uniqueness index; the old row is retained.
    let mut tx = pool.begin().await.unwrap();
    store.insert(&mut tx, &same_alias).await.unwrap();
    tx.commit().await.unwrap();
    store
        .settle(id, &Settlement::Journal(ExecutionState::Revoked))
        .await
        .unwrap();
    for assignment in [
        "settled_at_ms = 0",
        "settlement_json = '{}'",
        "settled_at_ms = NULL, settlement_json = NULL",
    ] {
        let query = format!("UPDATE workspace_executions SET {assignment} WHERE execution_id=?");
        assert!(
            sqlx::query(&query)
                .bind(id.as_str())
                .execute(&pool)
                .await
                .is_err(),
            "accepted {assignment}"
        );
    }
    sqlx::query("UPDATE workspace_executions SET settled_at_ms=settled_at_ms, settlement_json=settlement_json WHERE execution_id=?").bind(id.as_str()).execute(&pool).await.unwrap();
    store.finish(id).await.unwrap();
    let secrets = serde_json::to_string(&intent.secrets).unwrap();
    assert!(
        sqlx::query("UPDATE workspace_executions SET secrets_json=? WHERE execution_id=?")
            .bind(secrets)
            .bind(id.as_str())
            .execute(&pool)
            .await
            .is_err()
    );
    store.finish(id).await.unwrap();
    assert!(matches!(
        store.load(id).await.unwrap(),
        Some(Stored::Finished { .. })
    ));
}

#[test]
fn terminal_alias_mapping() {
    assert_eq!(sql::terminal_alias("123"), "term:123");
    assert_eq!(sql::TERMINAL_ALIAS_SQL, "'term:' || terminals.id");
}
