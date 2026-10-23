//! Forward repair at 0193; only codex conversation markers follow their stored role.
use calm_truth::MIGRATOR;
use serde_json::{Value, json};
use sqlx::{Connection, SqliteConnection, migrate::Migrate, sqlite::SqliteConnectOptions};

#[tokio::test]
async fn planner_switch_marker_migration_repairs_roles_without_other_changes() {
    let mut db = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .in_memory(true)
            .foreign_keys(true),
    )
    .await
    .unwrap();
    db.ensure_migrations_table().await.unwrap();
    for migration in MIGRATOR.iter().filter(|m| m.version <= 192) {
        db.apply(migration).await.unwrap();
    }
    sqlx::raw_sql(
        "INSERT INTO areas(id,name,color,sort,created_at,updated_at)
        VALUES('a','a','#000',0,1,1);
        INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at)
        VALUES('t','a','t',0,1,1);",
    )
    .execute(&mut db)
    .await
    .unwrap();
    let mut expected = Vec::new();
    for (index, (kind, role)) in [
        ("codex", "planner"),
        ("codex", "assistant"),
        ("codex", "worker"),
        ("note", "worker"),
    ]
    .into_iter()
    .enumerate()
    {
        for (variant, marker) in [
            None,
            Some(json!(true)),
            Some(json!(false)),
            Some(json!("stale")),
        ]
        .into_iter()
        .enumerate()
        {
            let id = format!("c{index}-{variant}");
            // Each Planner needs its own Track to respect the production unique index.
            let track = format!("t{index}-{variant}");
            sqlx::query("INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES(?1,'a','t',0,1,1)")
                .bind(&track).execute(&mut db).await.unwrap();
            let mut payload =
                json!({"schemaVersion":1,"custom":{"keep":[1,2]},"planner_provider":"claude"});
            if let Some(marker) = marker {
                payload["planner_harness"] = marker;
            }
            sqlx::query("INSERT INTO cards(id,track_id,kind,role,sort,payload,created_at,updated_at) VALUES(?1,?2,?3,?4,0,?5,1,2)")
                .bind(&id).bind(&track).bind(kind).bind(role).bind(payload.to_string())
                .execute(&mut db).await.unwrap();
            if kind == "codex" {
                payload.as_object_mut().unwrap().remove("planner_harness");
                if role == "planner" {
                    payload["planner_harness"] = json!(true);
                }
            }
            expected.push((id, kind.to_owned(), role.to_owned(), payload, 1_i64, 2_i64));
        }
    }
    let events_before: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(&mut db)
        .await
        .unwrap();
    db.apply(MIGRATOR.iter().find(|m| m.version == 193).unwrap())
        .await
        .unwrap();
    let rows: Vec<(String, String, String, String, i64, i64)> =
        sqlx::query_as("SELECT id,kind,role,payload,created_at,updated_at FROM cards ORDER BY id")
            .fetch_all(&mut db)
            .await
            .unwrap();
    let actual: Vec<_> = rows
        .into_iter()
        .map(|(id, kind, role, payload, c, u)| {
            (
                id,
                kind,
                role,
                serde_json::from_str::<Value>(&payload).unwrap(),
                c,
                u,
            )
        })
        .collect();
    expected.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(actual, expected);
    let events_after: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(&mut db)
        .await
        .unwrap();
    assert_eq!(
        events_after, events_before,
        "data repair must not fabricate events"
    );
    let fk: Vec<(String, i64, String, i64)> = sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(&mut db)
        .await
        .unwrap();
    assert!(fk.is_empty());
}
