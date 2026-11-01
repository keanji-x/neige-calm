use super::*;
use calm_truth::db::{Repo, sqlite::SqlxRepo};
use workspace_runtime::execution::test_support::{self, StoreHarness};

async fn fixture() -> (SqliteExecutionStore, SqliteDb) {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let pool = repo.sqlite_db().unwrap();
    (
        SqliteExecutionStore::new(pool.clone())
            .with_projection(test_support::TEST_CONSUMER.into(), Arc::new(NoopProjection)),
        pool,
    )
}
#[async_trait]
impl StoreHarness for SqliteExecutionStore {
    async fn transaction(
        &self,
        end: test_support::TransactionEnd,
        f: test_support::StoreWrite<'_, Self>,
    ) {
        self.pool
            .writer()
            .with_writer(|write_session| {
                Box::pin(async move {
                    let mut tx = write_session.begin_immediate().await.unwrap();
                    f(self, &mut tx).await;
                    match end {
                        test_support::TransactionEnd::Commit => tx.commit().await.unwrap(),
                        test_support::TransactionEnd::Rollback => tx.rollback().await.unwrap(),
                    }
                })
            })
            .await
            .expect("independent store contract operation");
    }
}

mod port_contract {
    #[tokio::test]
    async fn store() {
        let (store, _) = super::fixture().await;
        workspace_runtime::execution::test_support::port_contract::store(&store).await;
    }
}
async fn inserted(store: &SqliteExecutionStore, pool: &SqliteDb) -> ExecutionIntent {
    let intent = test_support::intent("/".into());
    let write_db = (pool).clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                let mut tx = write_session.begin_immediate().await.unwrap();
                store.insert(&mut tx, &intent).await.unwrap();
                tx.commit().await.unwrap();
                intent
            })
        })
        .await
        .expect("independent write operation")
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
    let write_db = (pool).clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                let mut tx = write_session.begin_immediate().await.unwrap();
                // A real domain-table write shares the caller's transaction with Engine::admit.
                sqlx::query(
                    "INSERT INTO areas (id, name, color, sort, created_at, updated_at)
         VALUES ('execution-domain', 'Execution domain', '#000000', 0, 0, 0)",
                )
                .execute(&mut **tx)
                .await
                .unwrap();
                let _ticket = engine.admit(&mut tx, intent).await.unwrap();
                tx.rollback().await.unwrap();
                assert!(store.load(&id).await.unwrap().is_none());
                assert_eq!(
                    sqlx::query_scalar::<_, i64>(
                        "SELECT COUNT(*) FROM areas WHERE id = 'execution-domain'"
                    )
                    .fetch_one(pool.read_pool())
                    .await
                    .unwrap(),
                    0
                );
            })
        })
        .await
        .expect("independent write operation")
}
#[tokio::test]
async fn store_stop_settlement_keeps_secrets_check() {
    let (store, pool) = fixture().await;
    let intent = inserted(&store, &pool).await;
    let id = &intent.metadata.id;
    let write_db = (pool).clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                let mut tx = write_session.begin_immediate().await.unwrap();
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
            })
        })
        .await
        .expect("independent write operation")
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
    let write_db = (pool).clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                let mut tx = write_session.begin_immediate().await.unwrap();
                store.request(&mut tx, id, Desired::Retire).await.unwrap();
                tx.commit().await.unwrap();
                assert_eq!(store.pending().await.unwrap(), vec![id.clone()]);
                store.finish(id).await.unwrap();
                assert!(store.pending().await.unwrap().is_empty());
                assert!(matches!(
                    store.load(id).await.unwrap(),
                    Some(Stored::Finished { .. })
                ));
            })
        })
        .await
        .expect("independent write operation")
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
            (pool)
                .writer()
                .execute(sqlx::query(&query).bind(id.as_str()))
                .await
                .is_err(),
            "accepted {assignment}"
        );
    }
    // Exact rewrites are permitted.
    (pool)
        .writer()
        .execute(sqlx::query(
            "UPDATE workspace_executions SET execution_id=execution_id, track_id=track_id,
         alias=alias, intent_json=intent_json, secrets_json=secrets_json, desired=desired",
        ))
        .await
        .unwrap();
    let mut same_alias = test_support::intent("/".into());
    same_alias.metadata.alias = intent.metadata.alias.clone();
    let write_db = (pool).clone();
    write_db.writer().with_writer(|write_session| Box::pin(async move {
        let mut tx = write_session.begin_immediate().await.unwrap();
        assert!(store.insert(&mut tx, &same_alias).await.is_err());
        tx.rollback().await.unwrap();
        for (desired, rejected) in [(Desired::Stop, "run"), (Desired::Retire, "stop")] {
            let mut tx = write_session.begin_immediate().await.unwrap();
            store.request(&mut tx, id, desired).await.unwrap();
            tx.commit().await.unwrap();
            assert!(
                (pool).writer().execute(sqlx::query("UPDATE workspace_executions SET desired=? WHERE execution_id=?")
                    .bind(rejected)
                    .bind(id.as_str()))
                    .await
                    .is_err()
            );
        }
        // Retiring frees the current-alias uniqueness index; the old row is retained.
        let mut tx = write_session.begin_immediate().await.unwrap();
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
                (pool).writer().execute(sqlx::query(&query)
                    .bind(id.as_str()))
                    .await
                    .is_err(),
                "accepted {assignment}"
            );
        }
        (pool).writer().execute(sqlx::query("UPDATE workspace_executions SET settled_at_ms=settled_at_ms, settlement_json=settlement_json WHERE execution_id=?").bind(id.as_str())).await.unwrap();
        store.finish(id).await.unwrap();
        let secrets = serde_json::to_string(&intent.secrets).unwrap();
        assert!(
            (pool).writer().execute(sqlx::query("UPDATE workspace_executions SET secrets_json=? WHERE execution_id=?")
                .bind(secrets)
                .bind(id.as_str()))
                .await
                .is_err()
        );
        store.finish(id).await.unwrap();
        assert!(matches!(
            store.load(id).await.unwrap(),
            Some(Stored::Finished { .. })
        ));

    })).await.expect("independent write operation")
}

#[test]
fn terminal_alias_mapping() {
    assert_eq!(sql::terminal_alias("123"), "term:123");
    assert_eq!(sql::TERMINAL_ALIAS_SQL, "'term:' || terminals.id");
}

struct RecordingProjection(&'static str);
#[async_trait]
impl ExecutionProjection for RecordingProjection {
    async fn spawned(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        alias: &str,
        _: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("INSERT INTO projection_calls (owner, alias, phase) VALUES (?, ?, 'spawned')")
            .bind(self.0)
            .bind(alias)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }
    async fn settled(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        alias: &str,
        _: &Settlement,
        _: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("INSERT INTO projection_calls (owner, alias, phase) VALUES (?, ?, 'settled')")
            .bind(self.0)
            .bind(alias)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }
}
// Projection-free consumers must opt in explicitly via registration.
pub(crate) struct NoopProjection;
#[async_trait]
impl ExecutionProjection for NoopProjection {
    async fn spawned(
        &self,
        _: &mut Transaction<'_, Sqlite>,
        _: &str,
        _: i64,
    ) -> Result<(), sqlx::Error> {
        Ok(())
    }
    async fn settled(
        &self,
        _: &mut Transaction<'_, Sqlite>,
        _: &str,
        _: &Settlement,
        _: i64,
    ) -> Result<(), sqlx::Error> {
        Ok(())
    }
}
#[tokio::test]
async fn projection_registered_per_kind() {
    let (store, pool) = fixture().await;
    (pool)
        .writer()
        .execute(sqlx::query(
            "CREATE TABLE projection_calls (owner TEXT, alias TEXT, phase TEXT)",
        ))
        .await
        .unwrap();
    let store = store
        .with_projection("consumer-a".into(), Arc::new(RecordingProjection("a")))
        .with_projection("consumer-b".into(), Arc::new(RecordingProjection("b")))
        .with_projection("projection-free".into(), Arc::new(NoopProjection));
    let write_db = (pool).clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                for kind in ["consumer-a", "consumer-b", "projection-free"] {
                    let mut intent = test_support::intent("/".into());
                    intent.metadata.alias = ProcessAlias(kind.into());
                    intent.metadata.consumer = kind.into();
                    let mut tx = write_session.begin_immediate().await.unwrap();
                    store.insert(&mut tx, &intent).await.unwrap();
                    tx.commit().await.unwrap();
                    store.spawned(&intent.metadata.id).await.unwrap();
                    store
                        .settle(
                            &intent.metadata.id,
                            &Settlement::Journal(ExecutionState::Revoked),
                        )
                        .await
                        .unwrap();
                    assert!(
                        store
                            .load(&intent.metadata.id)
                            .await
                            .unwrap()
                            .unwrap()
                            .is_settled()
                    );
                }
                let calls: Vec<(String, String, String)> = sqlx::query_as(
                    "SELECT owner, alias, phase FROM projection_calls ORDER BY owner, phase",
                )
                .fetch_all(pool.read_pool())
                .await
                .unwrap();
                assert_eq!(
                    calls,
                    vec![
                        ("a".into(), "consumer-a".into(), "settled".into()),
                        ("a".into(), "consumer-a".into(), "spawned".into()),
                        ("b".into(), "consumer-b".into(), "settled".into()),
                        ("b".into(), "consumer-b".into(), "spawned".into()),
                    ]
                );
            })
        })
        .await
        .expect("independent write operation")
}

#[tokio::test]
async fn unregistered_projection_fails_and_rolls_back() {
    let (store, pool) = fixture().await;
    let intent = test_support::intent("/".into());
    let write_db = (pool).clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                            let mut tx = write_session.begin_immediate().await.unwrap();
                            store.insert(&mut tx, &intent).await.unwrap();
                            tx.commit().await.unwrap();
                            let store = SqliteExecutionStore::new(pool.clone());
                            let id = &intent.metadata.id;
                            let spawned = store.spawned(id).await;
                            let settled = store
                                .settle(id, &Settlement::Journal(ExecutionState::Revoked))
                                .await;
                            for result in [spawned, settled] {
                                assert_eq!(
                                    result.unwrap_err().0,
                                    "missing execution projection for consumer: port-contract"
                                );
                            }
                            let durable: (Option<i64>, Option<String>) = sqlx::query_as(
                    "SELECT settled_at_ms, settlement_json FROM workspace_executions WHERE execution_id = ?",
                )
                .bind(id.as_str())
                .fetch_one(pool.read_pool())
                .await
                .unwrap();
                            assert_eq!(durable, (None, None));
                            assert!(!store.load(id).await.unwrap().unwrap().is_settled());
                            assert_eq!(store.pending().await.unwrap(), vec![id.clone()]);
            })
        })
        .await
        .expect("independent write operation")
}

#[tokio::test]
async fn unregistered_consumer_refused_at_admit() {
    let (_, pool) = fixture().await;
    let store = SqliteExecutionStore::new(pool.clone());
    let engine = Engine::new(
        store.clone(),
        test_support::ScriptedExecutor::new(Default::default(), []),
    );
    let intent = test_support::intent("/".into());
    let id = intent.metadata.id.clone();
    let write_db = (pool).clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                let mut tx = write_session.begin_immediate().await.unwrap();
                assert_eq!(
                    store.insert(&mut tx, &intent).await.unwrap_err().0,
                    "missing execution projection for consumer: port-contract"
                );
                assert!(engine.admit(&mut tx, intent).await.is_err());
                tx.commit().await.unwrap();
                assert!(store.load(&id).await.unwrap().is_none());
                assert!(store.pending().await.unwrap().is_empty());
            })
        })
        .await
        .expect("independent write operation")
}

#[tokio::test]
async fn c4_consumer_inventory_real_store_unclassified_rows_and_scan_cost() {
    let (store, pool) = fixture().await;
    let intent = inserted(&store, &pool).await;
    let id = intent.metadata.id.as_str();
    let inventory = sql::consumer_inventory(pool.read_pool(), test_support::TEST_CONSUMER)
        .await
        .unwrap();
    let row = inventory[0].1.as_ref().unwrap();
    assert_eq!(row.execution_id, id);
    assert_eq!(row.alias, intent.metadata.alias.0);
    assert!(!row.settled);
    (pool)
        .writer()
        .execute(sqlx::query("DROP TRIGGER workspace_executions_immutable"))
        .await
        .unwrap();
    // A malformed intent is unclassified even when queried for another consumer.
    (pool)
        .writer()
        .execute(
            sqlx::query("UPDATE workspace_executions SET intent_json='{}' WHERE execution_id=?")
                .bind(id),
        )
        .await
        .unwrap();
    for consumer in [test_support::TEST_CONSUMER, "another-consumer"] {
        let rows = sql::consumer_inventory(pool.read_pool(), consumer)
            .await
            .unwrap();
        assert!(rows[0].1.is_err());
    }
    (pool)
        .writer()
        .execute(
            sqlx::query("UPDATE workspace_executions SET intent_json=? WHERE execution_id=?")
                .bind(serde_json::to_string(&intent.metadata).unwrap())
                .bind(id),
        )
        .await
        .unwrap();
    // Real Store serialization across a representative retained-history inventory.
    let write_db = (pool).clone();
    write_db.writer().with_writer(|write_session| Box::pin(async move {
        let mut tx = write_session.begin_immediate().await.unwrap();
        for _ in 0..1000 {
            let next = test_support::intent("/".into());
            store.insert(&mut tx, &next).await.unwrap();
        }
        tx.commit().await.unwrap();
        let before = std::time::Instant::now();
        let rows = sql::consumer_inventory(pool.read_pool(), test_support::TEST_CONSUMER)
            .await
            .unwrap();
        eprintln!(
            "consumer_inventory: {} non-retired real Store rows in {:?}",
            rows.len(),
            before.elapsed()
        );
        assert_eq!(rows.len(), 1001);
        assert!(rows.iter().all(|(_, row)| row.is_ok()));
        // Uniform with Store::pending(): one malformed identity fails the entire scan.
        (pool).writer().execute(sqlx::query("UPDATE workspace_executions SET execution_id='malformed' WHERE execution_id=?")
            .bind(id))
            .await
            .unwrap();
        assert!(store.pending().await.is_err());
        assert!(
            sql::consumer_inventory(pool.read_pool(), test_support::TEST_CONSUMER)
                .await
                .is_err()
        );

    })).await.expect("independent write operation")
}
