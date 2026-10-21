use super::provider_execution::*;
use crate::db::sqlite::SqliteDb;
use crate::error::CalmError;
use sqlx::{Sqlite, Transaction};

async fn fixture() -> SqliteDb {
    let db = SqliteDb::connect("sqlite::memory:").await.unwrap();
    sqlx::raw_sql("CREATE TABLE worker_sessions(id TEXT PRIMARY KEY); INSERT INTO worker_sessions VALUES ('worker'), ('empty');")
        .execute(db.writer_pool()).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../../../migrations/0185_provider_execution.sql"
    ))
    .execute(db.writer_pool())
    .await
    .unwrap();
    db
}
fn applied<T: std::fmt::Debug>(result: Cas<T>) -> T {
    match result {
        Cas::Applied(value) => value,
        Cas::LostOwnership => panic!("unexpected lost ownership"),
    }
}
async fn admission(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Cas<Claim> {
    admit(
        tx,
        "worker",
        id.into(),
        Purpose::RewindCheck,
        InitialRelease::Held,
    )
    .await
    .unwrap()
}

#[test]
fn codec_round_trips_all_variants_and_rejects_missing_required_fields() {
    let reasons = [
        StopReason::Finished,
        StopReason::Interrupted,
        StopReason::Shutdown,
        StopReason::TrackRetired,
        StopReason::KernelRestarted,
        StopReason::DriverLost,
        StopReason::ReleaseRefused,
        StopReason::SetupFailed,
        StopReason::OutputGap,
        StopReason::StdinTorn,
    ];
    let purposes = [
        Purpose::Turn {
            turn_id: "turn".into(),
        },
        Purpose::Compaction {
            turn_id: "compact".into(),
        },
        Purpose::RewindCheck,
    ];
    let releases = [
        Release::NotRequired,
        Release::Held,
        Release::Releasing {
            authorized_at_ms: 12,
        },
        Release::Released {
            authorized_at_ms: 12,
        },
    ];
    for purpose in purposes {
        for release in releases {
            for decision in std::iter::once(Decision::Observing).chain(
                reasons
                    .into_iter()
                    .map(|reason| Decision::Stopping { reason, at_ms: 34 }),
            ) {
                let record = Record {
                    version: Version::V1,
                    execution_id: "exec".into(),
                    purpose: purpose.clone(),
                    release,
                    decision,
                };
                let value = serde_json::to_value(&record).unwrap();
                assert_eq!(value["version"], serde_json::json!(1));
                assert_eq!(
                    serde_json::from_value::<Record>(value.clone()).unwrap(),
                    record
                );
                for key in ["version", "execution_id", "purpose", "release", "decision"] {
                    let mut missing = value.clone();
                    missing.as_object_mut().unwrap().remove(key);
                    assert!(serde_json::from_value::<Record>(missing).is_err(), "{key}");
                }
            }
        }
    }
    let valid = serde_json::json!({"version":1,"execution_id":"exec", "purpose":"RewindCheck",
        "release":"NotRequired", "decision":"Observing"});
    for bad in [
        serde_json::json!("1"),
        serde_json::json!(1.0),
        serde_json::json!(2),
        serde_json::Value::Null,
    ] {
        let mut value = valid.clone();
        value["version"] = bad;
        assert!(serde_json::from_value::<Record>(value).is_err());
    }
    for (key, bad) in [
        ("purpose", serde_json::json!({"Turn":{}})),
        ("purpose", serde_json::json!({"Compaction":{}})),
        ("release", serde_json::json!({"Releasing":{}})),
        ("release", serde_json::json!({"Released":{}})),
        (
            "decision",
            serde_json::json!({"Stopping":{"reason":"Finished"}}),
        ),
        ("decision", serde_json::json!({"Stopping":{"at_ms":1}})),
        (
            "decision",
            serde_json::json!({"Stopping":{"reason":"Other", "at_ms":1}}),
        ),
    ] {
        let mut value = valid.clone();
        value[key] = bad;
        assert!(serde_json::from_value::<Record>(value).is_err());
    }
}

#[tokio::test]
async fn migration_preserves_sessions_and_reads_fail_closed() {
    let db = fixture().await;
    assert!(load(db.read_pool(), "worker").await.unwrap().is_none());
    assert!(list(db.read_pool()).await.unwrap().is_empty());
    assert!(matches!(
        load(db.read_pool(), "deleted").await,
        Err(CalmError::Db(sqlx::Error::RowNotFound))
    ));
    sqlx::query("UPDATE worker_sessions SET provider_execution_json = '{}' WHERE id = 'worker'")
        .execute(db.writer_pool())
        .await
        .unwrap();
    assert!(load(db.read_pool(), "worker").await.is_err());
    let mut tx = db.writer_pool().begin().await.unwrap();
    let healthy = applied(
        admit(
            &mut tx,
            "empty",
            "healthy".into(),
            Purpose::RewindCheck,
            InitialRelease::NotRequired,
        )
        .await
        .unwrap(),
    );
    tx.commit().await.unwrap();
    let inventory = list(db.read_pool()).await.unwrap();
    assert_eq!(inventory.len(), 2);
    assert_eq!(inventory[0].0, "empty");
    assert_eq!(inventory[0].1.as_ref().unwrap(), &healthy);
    assert_eq!(inventory[1].0, "worker");
    assert!(inventory[1].1.is_err());
    let mut tx = db.writer_pool().begin().await.unwrap();
    assert_eq!(
        admission(&mut tx, "must-not-adopt").await,
        Cas::LostOwnership
    );
    tx.rollback().await.unwrap();
    // Also exercise the complete production migration chain.
    let repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let columns: Vec<(i64, String, String, i64, Option<String>, i64)> =
        sqlx::query_as("PRAGMA table_info(worker_sessions)")
            .fetch_all(repo.pool())
            .await
            .unwrap();
    assert!(
        columns
            .iter()
            .any(|c| c.1 == "provider_execution_json" && c.2 == "TEXT" && c.3 == 0)
    );
}

#[tokio::test]
async fn cas_transitions_compare_the_whole_record_and_preserve_other_fields() {
    let db = fixture().await;
    let mut tx = db.writer_pool().begin().await.unwrap();
    let held = applied(admission(&mut tx, "first").await);
    assert_eq!(admission(&mut tx, "second").await, Cas::LostOwnership);
    assert_eq!(load(&mut *tx, "worker").await.unwrap(), Some(held.clone()));
    let releasing = applied(advance_release(&mut tx, "worker", &held, 11).await.unwrap());
    assert_eq!(
        releasing.record().release,
        Release::Releasing {
            authorized_at_ms: 11
        }
    );
    assert_eq!(
        advance_release(&mut tx, "worker", &held, 11).await.unwrap(),
        Cas::LostOwnership
    );
    let stopping = applied(
        stop(&mut tx, "worker", &releasing, StopReason::Shutdown, 22)
            .await
            .unwrap(),
    );
    assert_eq!(
        stop(&mut tx, "worker", &releasing, StopReason::DriverLost, 23)
            .await
            .unwrap(),
        Cas::LostOwnership
    );
    // A driver with the old Observing claim cannot overwrite sweep's decision.
    assert_eq!(
        advance_release(&mut tx, "worker", &releasing, 11)
            .await
            .unwrap(),
        Cas::LostOwnership
    );
    assert_eq!(
        clear_on_completion(&mut tx, "worker", &releasing)
            .await
            .unwrap(),
        Cas::LostOwnership
    );
    let released = applied(
        advance_release(&mut tx, "worker", &stopping, 11)
            .await
            .unwrap(),
    );
    assert_eq!(
        released.record().release,
        Release::Released {
            authorized_at_ms: 11
        }
    );
    assert_eq!(
        released.record().decision,
        Decision::Stopping {
            reason: StopReason::Shutdown,
            at_ms: 22
        }
    );
    assert_eq!(released.record().execution_id, "first");
    assert_eq!(released.record().purpose, Purpose::RewindCheck);
    assert_eq!(
        advance_release(&mut tx, "worker", &stopping, 11)
            .await
            .unwrap(),
        Cas::LostOwnership
    );
    assert!(
        advance_release(&mut tx, "worker", &released, 11)
            .await
            .is_err()
    );
    assert!(
        stop(&mut tx, "worker", &released, StopReason::Finished, 33)
            .await
            .is_err()
    );
    let inventory = list(&mut *tx).await.unwrap();
    assert_eq!(inventory.len(), 1);
    assert_eq!(inventory[0].0, "worker");
    assert_eq!(inventory[0].1.as_ref().unwrap(), &released);
    applied(
        clear_on_completion(&mut tx, "worker", &released)
            .await
            .unwrap(),
    );
    assert_eq!(
        clear_on_completion(&mut tx, "worker", &released)
            .await
            .unwrap(),
        Cas::LostOwnership
    );
    let new = applied(admission(&mut tx, "different-execution").await);
    assert_eq!(
        clear_on_completion(&mut tx, "worker", &released)
            .await
            .unwrap(),
        Cas::LostOwnership
    );
    assert_eq!(load(&mut *tx, "worker").await.unwrap(), Some(new));
    tx.rollback().await.unwrap();
    assert!(load(db.read_pool(), "worker").await.unwrap().is_none());
}

#[tokio::test]
async fn concurrent_admission_has_exactly_one_winner() {
    let db = fixture().await;
    // The single-writer pool serializes these transactions; this checks CAS
    // exclusion under competing tasks, not independent SQLite writer contention.
    let race = |id: &'static str| {
        let db = db.clone();
        async move {
            let mut tx = db.writer_pool().begin().await.unwrap();
            let result = admit(
                &mut tx,
                "worker",
                id.into(),
                Purpose::Turn { turn_id: id.into() },
                InitialRelease::NotRequired,
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
            result
        }
    };
    let (a, b) = tokio::join!(race("a"), race("b"));
    let winner = match (a, b) {
        (Cas::Applied(claim), Cas::LostOwnership) | (Cas::LostOwnership, Cas::Applied(claim)) => {
            claim
        }
        other => panic!("expected exactly one winner: {other:?}"),
    };
    assert_eq!(
        load(db.read_pool(), "worker").await.unwrap(),
        Some(winner.clone())
    );
    assert_eq!(winner.record().release, Release::NotRequired);
    assert_eq!(winner.record().decision, Decision::Observing);
    // Deleting the owner also deletes the claim; stale writers cannot resurrect it.
    sqlx::query("DELETE FROM worker_sessions WHERE id = 'worker'")
        .execute(db.writer_pool())
        .await
        .unwrap();
    let mut tx = db.writer_pool().begin().await.unwrap();
    assert_eq!(
        clear_on_completion(&mut tx, "worker", &winner)
            .await
            .unwrap(),
        Cas::LostOwnership
    );
    assert_eq!(admission(&mut tx, "later").await, Cas::LostOwnership);
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn stale_completion_cannot_clear_a_different_execution() {
    let db = fixture().await;
    let mut tx = db.writer_pool().begin().await.unwrap();
    let old = applied(admission(&mut tx, "old").await);
    applied(clear_on_completion(&mut tx, "worker", &old).await.unwrap());
    let new = applied(admission(&mut tx, "new").await);
    assert_eq!(
        clear_on_completion(&mut tx, "worker", &old).await.unwrap(),
        Cas::LostOwnership
    );
    assert_eq!(load(&mut *tx, "worker").await.unwrap(), Some(new));
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn provider_execution_sweep_inventory_finds_orphans_including_settled() {
    use super::workspace_execution as executions;
    let db = fixture().await;
    sqlx::raw_sql(include_str!(
        "../../../migrations/0170_workspace_executions.sql"
    ))
    .execute(db.writer_pool())
    .await
    .unwrap();
    let mut tx = db.writer_pool().begin().await.unwrap();
    let consumer = "consumer'with-quote";
    for (id, session, settled, retired, selected) in [
        ("deleted", "deleted-session", false, false, true),
        ("null", "empty", false, false, true),
        ("mismatch", "worker", false, false, true),
        ("settled", "settled-session", true, false, true),
        ("healthy", "healthy-session", false, false, true),
        ("retired", "retired-session", true, true, true),
        ("other", "other-session", false, false, false),
    ] {
        sqlx::query("INSERT OR IGNORE INTO worker_sessions(id) VALUES (?)")
            .bind(session)
            .execute(&mut *tx)
            .await
            .unwrap();
        // ExecutionMetadata is persisted directly in intent_json by SqliteExecutionStore.
        let intent = serde_json::json!({"consumer": if selected { consumer } else { "other" },
            "alias": session})
        .to_string();
        executions::insert(&mut tx, id, "track", session, &intent, "{}")
            .await
            .unwrap();
        if settled {
            executions::settle_tx(&mut tx, id, "{}", 1).await.unwrap();
        }
        if retired {
            executions::request(&mut tx, id, "retire").await.unwrap();
        }
        if id == "healthy" || id == "mismatch" {
            applied(
                admit(
                    &mut tx,
                    session,
                    if id == "healthy" {
                        id
                    } else {
                        "another-execution"
                    }
                    .into(),
                    Purpose::RewindCheck,
                    InitialRelease::NotRequired,
                )
                .await
                .unwrap(),
            );
        }
    }
    sqlx::query("DELETE FROM worker_sessions WHERE id = 'deleted-session'")
        .execute(&mut *tx)
        .await
        .unwrap();
    // Exercise a stopped execution as well as desired=run.
    executions::request(&mut tx, "mismatch", "stop")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let inventory = executions::consumer_inventory(db.read_pool(), consumer)
        .await
        .unwrap();
    assert_eq!(
        inventory
            .iter()
            .map(|(id, row)| {
                assert_eq!(id, &row.as_ref().unwrap().execution_id);
                id.as_str()
            })
            .collect::<Vec<_>>(),
        ["deleted", "healthy", "mismatch", "null", "settled"]
    );
    let mut orphans = Vec::new();
    for (_, row) in inventory {
        let row = row.unwrap();
        assert_eq!(row.settled, row.execution_id == "settled");
        let claim = load(db.read_pool(), &row.alias).await;
        match row.execution_id.as_str() {
            "deleted" => assert!(matches!(
                &claim,
                Err(CalmError::Db(sqlx::Error::RowNotFound))
            )),
            "null" | "settled" => assert!(claim.as_ref().unwrap().is_none()),
            "mismatch" => assert_eq!(
                claim
                    .as_ref()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .record()
                    .execution_id,
                "another-execution"
            ),
            "healthy" => assert_eq!(
                claim
                    .as_ref()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .record()
                    .execution_id,
                row.execution_id
            ),
            _ => unreachable!(),
        }
        if !matches!(claim, Ok(Some(ref owner)) if owner.record().execution_id == row.execution_id)
        {
            orphans.push(row.execution_id);
        }
    }
    assert_eq!(orphans, ["deleted", "mismatch", "null", "settled"]);
    assert!(
        executions::consumer_inventory(db.read_pool(), "absent")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn provider_execution_column_decode_errors_are_isolated() {
    let db = fixture().await;
    let mut tx = db.writer_pool().begin().await.unwrap();
    let healthy = applied(admission(&mut tx, "healthy").await);
    sqlx::raw_sql(
        "INSERT INTO worker_sessions(id, provider_execution_json) VALUES
        ('blob', x'FF'), ('text', CAST(x'FF' AS TEXT));",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let inventory = list(db.read_pool()).await.unwrap();
    assert_eq!(inventory.len(), 3);
    for (id, claim) in inventory {
        if id == "worker" {
            assert_eq!(claim.unwrap(), healthy);
        } else {
            assert!(
                matches!(claim, Err(CalmError::Db(sqlx::Error::ColumnDecode { .. }))),
                "{id}"
            );
            assert!(
                matches!(
                    load(db.read_pool(), &id).await,
                    Err(CalmError::Db(sqlx::Error::ColumnDecode { .. }))
                ),
                "{id}"
            );
        }
    }
    sqlx::query("DROP TABLE worker_sessions")
        .execute(db.writer_pool())
        .await
        .unwrap();
    assert!(matches!(
        list(db.read_pool()).await,
        Err(CalmError::Db(sqlx::Error::Database(_)))
    ));
}

#[tokio::test]
async fn provider_execution_consumer_inventory_isolates_corrupt_rows() {
    use super::workspace_execution as executions;
    let db = fixture().await;
    sqlx::raw_sql(include_str!(
        "../../../migrations/0170_workspace_executions.sql"
    ))
    .execute(db.writer_pool())
    .await
    .unwrap();
    // Bypass the JSON CHECK only to exercise damaged persisted data, too.
    sqlx::raw_sql(r#"PRAGMA ignore_check_constraints = ON;
        INSERT INTO workspace_executions(execution_id,track_id,alias,intent_json,secrets_json,desired) VALUES
        ('healthy','track','healthy','{"consumer":"selected","alias":"worker"}','{}','run'),
        ('blob','track','blob',x'FF','{}','run'),
        ('text','track','text',CAST(x'FF' AS TEXT),'{}','run'),
        ('json','track','json','{','{}','run'),
        ('alias','track','alias','{"consumer":"selected","alias":null}','{}','run');
        PRAGMA ignore_check_constraints = OFF;"#)
        .execute(db.writer_pool()).await.unwrap();
    let inventory = executions::consumer_inventory(db.read_pool(), "selected")
        .await
        .unwrap();
    assert_eq!(inventory.len(), 5);
    for (id, row) in inventory {
        if id == "healthy" {
            let row = row.unwrap();
            assert_eq!(row.execution_id, id);
            assert_eq!(row.alias, "worker");
            assert!(!row.settled);
        } else {
            assert!(row.is_err(), "{id}");
        }
    }
    sqlx::query("DROP TABLE workspace_executions")
        .execute(db.writer_pool())
        .await
        .unwrap();
    assert!(matches!(
        executions::consumer_inventory(db.read_pool(), "selected").await,
        Err(sqlx::Error::Database(_))
    ));
}
