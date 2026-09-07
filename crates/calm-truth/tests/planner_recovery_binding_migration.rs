//! Upgrade keeps prior sessions/attempts byte-identical and invents no old-turn authority.
use calm_truth::MIGRATOR;
use sqlx::{Connection, SqliteConnection, migrate::Migrate, sqlite::SqliteConnectOptions};

#[tokio::test]
async fn planner_binding_upgrade_preserves_old_facts_and_track_cascades_new_records() {
    let mut db = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .in_memory(true)
            .foreign_keys(true),
    )
    .await
    .unwrap();
    db.ensure_migrations_table().await.unwrap();
    for migration in MIGRATOR
        .iter()
        .filter(|m| m.version <= 100 && !m.migration_type.is_down_migration())
    {
        db.apply(migration).await.unwrap();
    }
    sqlx::raw_sql("INSERT INTO areas(id,name,color,sort,created_at,updated_at) VALUES('area','A','red',0,1,2);
      INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES('track','area','T',0,1,2);
      INSERT INTO worker_sessions(id,track_id,provider,mode,contract,state,thread_id,handle_state_json,created_at_ms,updated_at_ms)
      VALUES('session','track','codex','resumable','planner','idle','old-thread','{\"schema_version\":1,\"retained\":true}',1,2);
      INSERT INTO task_attempt_allocations(attempt_id,track_id,key,generation,origin_json,created_at_ms)
      VALUES('attempt','track','a',1,'{\"kind\":\"initial\"}',1);")
      .execute(&mut db).await.unwrap();
    let snapshot: String =
        sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id='session'")
            .fetch_one(&mut db)
            .await
            .unwrap();
    db.apply(MIGRATOR.iter().find(|m| m.version == 101).unwrap())
        .await
        .unwrap();
    let after: String =
        sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id='session'")
            .fetch_one(&mut db)
            .await
            .unwrap();
    assert_eq!(after, snapshot);
    let original: String = sqlx::query_scalar(
        "SELECT attempt_id FROM task_attempt_allocations WHERE track_id='track'",
    )
    .fetch_one(&mut db)
    .await
    .unwrap();
    assert_eq!(original, "attempt");
    for table in [
        "planner_recovery_threads",
        "planner_recovery_issuances",
        "planner_recovery_turns",
        "planner_recovery_calls",
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&mut db)
            .await
            .unwrap();
        assert_eq!(count, 0, "upgrade must not infer old bindings");
    }
    sqlx::raw_sql("INSERT INTO planner_recovery_threads VALUES('new-thread','track','card',3);
        INSERT INTO planner_recovery_issuances VALUES('issuance','track','historical-session','new-thread','[]','[{\"key\":\"a\",\"expected_attempt_id\":\"attempt\",\"event_id\":1,\"request_key\":\"stable-action\",\"capability\":{\"allowed\":false,\"code\":\"explicit_user\",\"reason\":\"User recovery required\"}}]',3);
        INSERT INTO planner_recovery_turns VALUES('new-thread','turn','historical-session','track','issuance',4);
        INSERT INTO planner_recovery_calls VALUES('new-thread','turn','call','historical-session','track','Recover','0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef',5);")
        .execute(&mut db).await.unwrap();
    assert!(sqlx::query("INSERT INTO planner_recovery_turns VALUES('new-thread','turn','historical-session','track','issuance',6)").execute(&mut db).await.is_err());
    assert!(sqlx::query("INSERT INTO planner_recovery_turns VALUES('new-thread','other','wrong-session','track','issuance',6)").execute(&mut db).await.is_err());
    // Records deliberately have no FK to a session or retained event. Removing
    // a session cannot invalidate immutable provenance for an already issued turn.
    sqlx::query("DELETE FROM worker_sessions")
        .execute(&mut db)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM planner_recovery_calls")
        .fetch_one(&mut db)
        .await
        .unwrap();
    assert_eq!(count, 1);
    sqlx::query("DELETE FROM tracks WHERE id='track'")
        .execute(&mut db)
        .await
        .unwrap();
    for table in [
        "planner_recovery_threads",
        "planner_recovery_issuances",
        "planner_recovery_turns",
        "planner_recovery_calls",
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&mut db)
            .await
            .unwrap();
        assert_eq!(count, 0, "Track deletion must clean {table}");
    }
}
