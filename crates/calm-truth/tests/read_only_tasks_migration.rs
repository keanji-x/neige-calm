//! 0130 (#1917): `tasks.access` and `workspace_leases.access_mode` default every existing row to
//! `read_write`, and the active-path index keeps one writer per path while read-only leases of the
//! same path are held beside it.

use calm_truth::MIGRATOR;
use sqlx::{Connection, SqliteConnection, migrate::Migrate, sqlite::SqliteConnectOptions};

async fn schema_up_to(version: i64) -> SqliteConnection {
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
        .filter(|m| m.version <= version && !m.migration_type.is_down_migration())
    {
        db.apply(migration).await.unwrap();
    }
    db
}

async fn exec(db: &mut SqliteConnection, sql: &str) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(sql).execute(db).await.map(|_| ())
}

fn lease(id: &str, state: &str, access: Option<&str>) -> String {
    let (column, value) = match access {
        Some(access) => (",access_mode", format!(",'{access}'")),
        None => ("", String::new()),
    };
    format!(
        "INSERT INTO workspace_leases(lease_id,card_id,track_id,path,state,lease_owner,
         created_at_ms,updated_at_ms{column})
         VALUES('{id}','card-{id}','track','/checkout','{state}','op-{id}',1,1{value});"
    )
}

#[tokio::test]
async fn old_rows_read_write_and_one_writer_per_path() {
    let mut db = schema_up_to(129).await;
    exec(
        &mut db,
        "INSERT INTO areas(id,name,color,sort,created_at,updated_at) VALUES('area','A','red',0,1,2);
         INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES('track','area','T',0,1,2);
         INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,created_at_ms,updated_at_ms)
         VALUES('track:old','track','old','codex','g','{}','done',1,1);",
    )
    .await
    .unwrap();
    for statement in [
        lease("old-released", "released", None),
        lease("old-held", "held", None),
    ] {
        exec(&mut db, &statement).await.unwrap();
    }

    for migration in MIGRATOR
        .iter()
        .filter(|m| m.version == 130 && !m.migration_type.is_down_migration())
    {
        db.apply(migration).await.unwrap();
    }

    let task: String = sqlx::query_scalar("SELECT access FROM current_tasks WHERE id='track:old'")
        .fetch_one(&mut db)
        .await
        .unwrap();
    assert_eq!(task, "read_write", "the view exposes the new column");
    let leases: Vec<String> =
        sqlx::query_scalar("SELECT access_mode FROM workspace_leases ORDER BY lease_id")
            .fetch_all(&mut db)
            .await
            .unwrap();
    assert_eq!(leases, ["read_write", "read_write"]);

    assert!(
        exec(&mut db, &lease("writer-2", "held", None))
            .await
            .is_err(),
        "a second active writer lease of the path is refused"
    );
    assert!(
        exec(&mut db, &lease("writer-3", "releasing", Some("read_write")))
            .await
            .is_err()
    );
    for reader in ["reader-1", "reader-2"] {
        exec(&mut db, &lease(reader, "held", Some("read_only")))
            .await
            .unwrap();
    }
    for refused in [
        lease("bad-mode", "held", Some("readonly")),
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,access,created_at_ms,updated_at_ms)
         VALUES('track:bad','track','bad','codex','g','{}','write',1,1);"
            .to_string(),
    ] {
        assert!(exec(&mut db, &refused).await.is_err(), "{refused}");
    }
}
