//! #1893 S4: the Planner dispatch receipt table is dropped with the retired `task.dispatch` tool.
use calm_truth::MIGRATOR;
use sqlx::{Connection, SqliteConnection, migrate::Migrate, sqlite::SqliteConnectOptions};

async fn schema_objects(db: &mut SqliteConnection) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE name='planner_dispatch_receipts' \
         OR tbl_name='planner_dispatch_receipts'",
    )
    .fetch_all(db)
    .await
    .unwrap()
}

#[tokio::test]
async fn planner_dispatch_receipts_table_is_dropped() {
    let mut db = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .in_memory(true)
            .foreign_keys(true),
    )
    .await
    .unwrap();
    db.ensure_migrations_table().await.unwrap();
    let drop = MIGRATOR
        .iter()
        .find(|m| m.description == "drop planner dispatch receipts")
        .expect("the drop migration is embedded");
    for migration in MIGRATOR
        .iter()
        .filter(|m| m.version < drop.version && !m.migration_type.is_down_migration())
    {
        db.apply(migration).await.unwrap();
    }
    // A released receipt row, as 4140 holds three.
    sqlx::raw_sql("INSERT INTO areas(id,name,color,sort,created_at,updated_at) VALUES('area','A','red',0,1,2);
        INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES('track','area','T',0,1,2);
        INSERT INTO planner_dispatch_receipts VALUES('track','Research','{\"workspace\":\"empty\"}','dispatch-1','report','b_task',3);")
        .execute(&mut db)
        .await
        .unwrap();
    assert!(
        !schema_objects(&mut db).await.is_empty(),
        "the table exists before the drop"
    );

    db.apply(drop).await.unwrap();

    assert_eq!(schema_objects(&mut db).await, Vec::<String>::new());
    let tracks: i64 = sqlx::query_scalar("SELECT count(*) FROM tracks")
        .fetch_one(&mut db)
        .await
        .unwrap();
    assert_eq!(tracks, 1, "the parent track stays");
}
