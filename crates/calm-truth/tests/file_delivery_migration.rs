//! Forward-only publication/input identity, preparation monotonicity and retention.
use calm_truth::MIGRATOR;
use sqlx::{Connection, SqliteConnection, migrate::Migrate, sqlite::SqliteConnectOptions};
#[tokio::test]
async fn file_delivery_migration_freezes_identity_and_retains_until_track_delete() {
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
        .filter(|m| m.version <= 101 && !m.migration_type.is_down_migration())
    {
        db.apply(migration).await.unwrap();
    }
    sqlx::raw_sql("INSERT INTO areas(id,name,color,sort,created_at,updated_at) VALUES('area','A','red',0,1,2);
        INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES('track','area','T',0,1,2);").execute(&mut db).await.unwrap();
    db.apply(MIGRATOR.iter().find(|m| m.version == 102).unwrap())
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM task_file_input_bindings")
        .fetch_one(&mut db)
        .await
        .unwrap();
    assert_eq!(count, 0, "upgrade never backfills input authority");
    sqlx::raw_sql("INSERT INTO task_file_publications VALUES('publication','track','producer','source','result','{}');
        INSERT INTO task_file_input_bindings VALUES('consumer','track','publication','{}','bound',NULL);").execute(&mut db).await.unwrap();
    for query in [
        "UPDATE task_file_publications SET source_operation_id='other'",
        "UPDATE task_file_publications SET receipt_json='{\"different\":true}'",
        "UPDATE task_file_input_bindings SET binding_json='{\"different\":true}'",
        "UPDATE task_file_input_bindings SET state='prepared'",
    ] {
        assert!(
            sqlx::query(query).execute(&mut db).await.is_err(),
            "{query}"
        );
    }
    sqlx::query(
        "UPDATE task_file_input_bindings SET state='prepared',prepared_operation_id='consumer-op'",
    )
    .execute(&mut db)
    .await
    .unwrap();
    assert!(
        sqlx::query("UPDATE task_file_input_bindings SET state='bound',prepared_operation_id=NULL")
            .execute(&mut db)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE task_file_input_bindings SET prepared_operation_id='other'")
            .execute(&mut db)
            .await
            .is_err()
    );
    sqlx::query("DELETE FROM tracks WHERE id='track'")
        .execute(&mut db)
        .await
        .unwrap();
    for table in ["task_file_publications", "task_file_input_bindings"] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&mut db)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
}
