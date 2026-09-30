//! #1893 S2: the file-delivery and candidate tables are dropped with their code, children before
//! parents under enforced foreign keys; their operations and settled events stay as inert history.
use calm_truth::MIGRATOR;
use sqlx::{Connection, SqliteConnection, migrate::Migrate, sqlite::SqliteConnectOptions};

const DROPPED: [&str; 8] = [
    "task_candidate_decision_bindings",
    "task_candidate_decisions",
    "task_candidate_input_bindings",
    "task_candidate_verification_allocations",
    "task_file_candidates",
    "task_file_input_bindings",
    "task_file_publications",
    "task_candidate_repairs",
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
async fn file_delivery_tables_are_dropped() {
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
        .find(|m| m.description == "drop file delivery")
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
        INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,target_type,target_json,payload_json,phase,created_at_ms,updated_at_ms)
          VALUES('publication','publication','task-file-publication','producer','h','task','{}','{}','succeeded',1,2),
                ('verification','verification','candidate-verify','verify','h','task','{}','{}','succeeded',1,2);
        INSERT INTO events(kind,payload,actor,at) VALUES('task.candidate_verification_settled','{}','kernel',1);
        INSERT INTO task_file_publications VALUES('publication','track','producer','source','result','{}');
        INSERT INTO task_file_input_bindings VALUES('consumer','track','publication','{}','bound',NULL);
        INSERT INTO task_file_candidates VALUES('publication','track','producer','project','{}');
        INSERT INTO task_candidate_verification_allocations VALUES('publication','track','verification');
        INSERT INTO task_candidate_input_bindings VALUES('reviewer','track','publication','verification','{}','bound',NULL);
        INSERT INTO task_candidate_decisions VALUES(1,'track','producer','{}');
        INSERT INTO task_candidate_decision_bindings VALUES('reviewer',1);
        INSERT INTO task_candidate_repairs VALUES('repair','track','produce','repair','review','{}');")
        .execute(&mut db)
        .await
        .unwrap();
    assert!(
        schema_objects(&mut db).await.len() > DROPPED.len(),
        "every table and its triggers exist before the drop"
    );

    db.apply(drop).await.unwrap();

    assert_eq!(schema_objects(&mut db).await, Vec::<String>::new());
    let history: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM operations), (SELECT count(*) FROM events WHERE kind='task.candidate_verification_settled')",
    )
    .fetch_one(&mut db)
    .await
    .unwrap();
    assert_eq!(history, (2, 1), "operations and events stay as history");
}
