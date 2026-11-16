//! Epoch upgrades run through the same migrator and repository open as production.
use std::borrow::Cow;

#[tokio::test]
async fn mcp_token_epoch_upgrade_preserves_mainline_sessions_and_hashes() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("mainline.db").display()
    );
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let mainline = sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|m| m.version <= 221)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    mainline.run(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO areas(id,name,color,sort,created_at,updated_at) VALUES('a','kept','#000',0,1,2);
         INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES('t','a','kept',0,3,4);
         INSERT INTO worker_sessions(id,track_id,provider,mode,contract,state,mcp_token_hash,
             thread_id,agent_session_id,handle_state_json,created_at_ms,updated_at_ms)
         VALUES('w','t','opencode','resumable','planner','idle','legacy-random-hash',
             'thread','native','{\"kept\":true}',5,6);
         INSERT INTO workspace_executions(execution_id,track_id,alias,intent_json,secrets_json,desired)
         VALUES('e','t','alias','{}','{}','run');
         INSERT INTO acp_resident_workspace_bindings VALUES('e','w','t','/kept-workspace');",
    ).execute(&pool).await.unwrap();
    let select = "SELECT id,track_id,provider,mode,contract,state,mcp_token_hash,thread_id,
                         agent_session_id,handle_state_json,created_at_ms,updated_at_ms
                  FROM worker_sessions WHERE id='w'";
    type Session = (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        i64,
        i64,
    );
    let before: Session = sqlx::query_as(select).fetch_one(&pool).await.unwrap();
    let checksum: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version=221")
            .fetch_one(&pool)
            .await
            .unwrap();
    pool.close().await;
    let repo = super::SqlxRepo::open(&url).await.unwrap();
    assert_eq!(
        sqlx::query_as::<_, Session>(select)
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        before
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT mcp_token_epoch FROM worker_sessions WHERE id='w'")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, Vec<u8>>("SELECT checksum FROM _sqlx_migrations WHERE version=221")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        checksum
    );
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(repo.pool())
            .await
            .unwrap()
            .is_empty()
    );
    // Mainline's generation-to-workspace ownership survives the upgrade.
    let binding: (String, String, String, String) = sqlx::query_as(
        "SELECT execution_id,worker_session_id,track_id,cwd FROM acp_resident_workspace_bindings",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(
        binding,
        ("e".into(), "w".into(), "t".into(), "/kept-workspace".into())
    );
    crate::MIGRATOR.run(repo.pool()).await.unwrap();
    assert_eq!(
        sqlx::query_as::<_, Session>(select)
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        before
    );
}

#[tokio::test]
async fn mcp_token_epoch_empty_database_boots_and_migration_versions_are_unique() {
    let mut versions = std::collections::BTreeSet::new();
    for migration in crate::MIGRATOR.iter() {
        assert!(
            versions.insert(migration.version),
            "duplicate migration version {}",
            migration.version
        );
    }
    let repo = super::SqlxRepo::open("sqlite::memory:").await.unwrap();
    sqlx::query("SELECT mcp_token_epoch FROM worker_sessions")
        .fetch_all(repo.pool())
        .await
        .unwrap();
    crate::MIGRATOR.run(repo.pool()).await.unwrap();
}
