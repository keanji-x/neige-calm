//! Failure identity is an atomic transition fact, independent of mutable snapshots.
use super::SqlxRepo;

#[tokio::test]
async fn session_failure_epoch_upgrade_insert_transition_and_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("old.sqlite").display()
    );
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let old = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|m| m.version < 212)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    old.run(&pool).await.unwrap();
    sqlx::query("INSERT INTO areas(id,name,color,sort,kind,created_at,updated_at) VALUES('area','area','#000',0,'user',0,0)").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES('track','area','track',0,0,0)").execute(&pool).await.unwrap();
    for state in ["failed", "idle"] {
        sqlx::query("INSERT INTO worker_sessions(id,track_id,provider,mode,contract,state,created_at_ms,updated_at_ms) VALUES(?1,'track','codex','resumable','planner',?1,0,0)").bind(state).execute(&pool).await.unwrap();
    }
    pool.close().await;
    let repo = SqlxRepo::open(&url).await.unwrap();
    let epochs = || async {
        sqlx::query_as::<_, (String, i64)>(
            "SELECT id,failure_epoch FROM worker_sessions ORDER BY id",
        )
        .fetch_all(repo.pool())
        .await
        .unwrap()
    };
    assert_eq!(
        epochs().await,
        vec![("failed".into(), 1), ("idle".into(), 0)]
    );
    let db = repo.db().clone();
    db.writer().execute(sqlx::query("INSERT INTO worker_sessions(id,track_id,provider,mode,contract,state,created_at_ms,updated_at_ms) VALUES('new','track','codex','resumable','planner','failed',0,0)")).await.unwrap();
    assert_eq!(epochs().await.last().unwrap(), &("new".into(), 1));
    db.writer()
        .execute(sqlx::query(
            "UPDATE worker_sessions SET state='failed',updated_at_ms=77 WHERE id='failed'",
        ))
        .await
        .unwrap();
    assert_eq!(epochs().await[0].1, 1);
    db.writer()
        .execute(sqlx::query(
            "UPDATE worker_sessions SET state='idle' WHERE id='failed'",
        ))
        .await
        .unwrap();
    db.writer()
        .with_writer(|write| {
            Box::pin(async move {
                let mut tx = write.begin_immediate().await.unwrap();
                sqlx::query("UPDATE worker_sessions SET state='failed' WHERE id='failed'")
                    .execute(&mut **tx)
                    .await
                    .unwrap();
                let epoch: i64 = sqlx::query_scalar(
                    "SELECT failure_epoch FROM worker_sessions WHERE id='failed'",
                )
                .fetch_one(&mut **tx)
                .await
                .unwrap();
                assert_eq!(epoch, 2);
                tx.rollback().await.unwrap();
            })
        })
        .await
        .unwrap();
    assert_eq!(
        epochs().await[0].1,
        1,
        "rolled-back failure must not consume an episode"
    );
    db.writer()
        .execute(sqlx::query(
            "UPDATE worker_sessions SET state='failed' WHERE id='failed'",
        ))
        .await
        .unwrap();
    assert_eq!(epochs().await[0].1, 2);
    assert!(
        db.writer()
            .execute(sqlx::query(
                "UPDATE worker_sessions SET failure_epoch=-1 WHERE id='failed'"
            ))
            .await
            .is_err()
    );
}
