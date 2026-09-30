//! #1893 S3: the semantic-recovery binding tables are dropped with their code, children before
//! parents under enforced foreign keys.
use calm_truth::MIGRATOR;
use sqlx::{Connection, SqliteConnection, migrate::Migrate, sqlite::SqliteConnectOptions};

const DROPPED: [&str; 4] = [
    "planner_recovery_calls",
    "planner_recovery_turns",
    "planner_recovery_issuances",
    "planner_recovery_threads",
];

async fn schema_objects(db: &mut SqliteConnection) -> Vec<String> {
    let mut names = Vec::new();
    for table in DROPPED {
        let rows: Vec<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE name=?1 OR tbl_name=?1")
                .bind(table)
                .fetch_all(&mut *db)
                .await
                .unwrap();
        names.extend(rows);
    }
    names
}

#[tokio::test]
async fn planner_recovery_tables_are_dropped() {
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
        .find(|m| m.description == "drop planner recovery bindings")
        .expect("the drop migration is embedded");
    for migration in MIGRATOR
        .iter()
        .filter(|m| m.version < drop.version && !m.migration_type.is_down_migration())
    {
        db.apply(migration).await.unwrap();
    }
    // One row per table, linked the way production linked them, so a parent dropped before its
    // child fails on the foreign key.
    sqlx::raw_sql("INSERT INTO areas(id,name,color,sort,created_at,updated_at) VALUES('area','A','red',0,1,2);
        INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES('track','area','T',0,1,2);
        INSERT INTO planner_recovery_threads VALUES('thread','track','card',3);
        INSERT INTO planner_recovery_issuances VALUES('issuance','track','session','thread','[]','[{\"key\":\"a\"}]',3);
        INSERT INTO planner_recovery_turns VALUES('thread','turn','session','track','issuance',4);
        INSERT INTO planner_recovery_calls VALUES('thread','turn','call','session','track','Recover','0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef',5);")
        .execute(&mut db)
        .await
        .unwrap();
    assert!(
        schema_objects(&mut db).await.len() >= DROPPED.len(),
        "every table exists before the drop"
    );

    db.apply(drop).await.unwrap();

    assert_eq!(schema_objects(&mut db).await, Vec::<String>::new());
    let tracks: i64 = sqlx::query_scalar("SELECT count(*) FROM tracks")
        .fetch_one(&mut db)
        .await
        .unwrap();
    assert_eq!(tracks, 1, "the parent track stays");
}
