//! The backfill migration stamps `permission_mode: "never"` on exactly the cards
//! `PlannerBinding::from_shape` binds as a Planner and that carry no mode yet; every other row,
//! including the near misses, keeps its payload byte for byte.

use std::borrow::Cow;

use serde_json::Value;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::harness::profile::{HarnessProfile, PlannerBinding};
use crate::model::CardRole;

/// The backfill's version. Migration numbers are assigned at merge; this is the one place to move.
const BACKFILL: i64 = 155;

fn migrator(keep: impl Fn(i64) -> bool) -> sqlx::migrate::Migrator {
    sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            calm_truth::MIGRATOR
                .iter()
                .filter(|migration| keep(migration.version))
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    }
}

/// (id, kind, role, payload text). The ids name what each row is a near miss of.
const ROWS: &[(&str, &str, &str, &str)] = &[
    (
        "planner-codex",
        "codex",
        "planner",
        r#"{"schemaVersion":1,"codex_source":"shared","planner_harness":true,"planner_provider":"codex"}"#,
    ),
    (
        "planner-claude",
        "codex",
        "planner",
        r#"{"schemaVersion":1,"planner_harness":true,"planner_provider":"claude","model":"opus"}"#,
    ),
    (
        "planner-legacy-shape",
        "codex",
        "planner",
        r#"{"schemaVersion":1,"harness":{"snapshotVersion":0,"pendingQueue":[]},"planner_provider":"codex"}"#,
    ),
    (
        "provider-unknown",
        "codex",
        "planner",
        r#"{"schemaVersion":1,"planner_provider":"gpt"}"#,
    ),
    (
        "provider-wrong-case",
        "codex",
        "planner",
        r#"{"schemaVersion":1,"planner_provider":"Codex"}"#,
    ),
    (
        "provider-not-a-string",
        "codex",
        "planner",
        r#"{"schemaVersion":1,"planner_provider":["codex"]}"#,
    ),
    (
        "provider-null",
        "codex",
        "planner",
        r#"{"schemaVersion":1,"planner_provider":null}"#,
    ),
    (
        "provider-missing",
        "codex",
        "planner",
        r#"{"schemaVersion":1}"#,
    ),
    (
        "kind-claude",
        "claude",
        "planner",
        r#"{"schemaVersion":1,"planner_provider":"codex"}"#,
    ),
    (
        "kind-terminal",
        "terminal",
        "planner",
        r#"{"schemaVersion":1,"planner_provider":"codex"}"#,
    ),
    (
        "role-worker",
        "codex",
        "worker",
        r#"{"schemaVersion":1,"planner_provider":"codex"}"#,
    ),
    (
        "role-assistant",
        "codex",
        "assistant",
        r#"{"schemaVersion":1,"harness_profile":"assistant","planner_provider":"codex"}"#,
    ),
    (
        "plain-chat",
        "codex",
        "worker",
        r#"{"schemaVersion":1,"harness_profile":"plain_chat","planner_provider":"claude"}"#,
    ),
    (
        "mode-ask",
        "codex",
        "planner",
        r#"{"planner_provider":"codex","permission_mode":"ask"}"#,
    ),
    (
        "mode-corrupt",
        "codex",
        "planner",
        r#"{"planner_provider":"codex","permission_mode":42}"#,
    ),
    (
        "mode-null",
        "codex",
        "planner",
        r#"{"planner_provider":"codex","permission_mode":null}"#,
    ),
    ("payload-null", "codex", "planner", "null"),
    (
        "payload-array",
        "codex",
        "planner",
        r#"["planner_provider","codex"]"#,
    ),
    (
        "payload-invalid",
        "codex",
        "planner",
        r#"{"planner_provider":"codex""#,
    ),
];

/// What the migration must do to a row, decided by the production predicate itself.
fn oracle_stamps(kind: &str, role: &str, text: &str) -> bool {
    let Ok(payload) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    let role = CardRole::try_from(role.to_string()).expect("a seeded role parses");
    PlannerBinding::from_shape(kind, role, &payload)
        .is_some_and(|binding| binding.profile == HarnessProfile::Planner)
        && payload.get("permission_mode").is_none()
}

async fn payloads(pool: &SqlitePool) -> Vec<(String, String, i64)> {
    sqlx::query_as("SELECT id, payload, updated_at FROM cards ORDER BY id")
        .fetch_all(pool)
        .await
        .expect("read cards back")
}

#[tokio::test]
async fn the_backfill_stamps_never_on_exactly_the_cards_from_shape_binds_as_a_planner() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .in_memory(true)
                .foreign_keys(true),
        )
        .await
        .expect("open migration fixture");
    migrator(|version| version < BACKFILL)
        .run(&pool)
        .await
        .expect("apply the migrations before the backfill");
    sqlx::raw_sql(
        "INSERT INTO areas (id, name, color, sort, created_at, updated_at) VALUES ('a', 'a', '#000', 0, 1, 1);",
    )
    .execute(&pool)
    .await
    .expect("seed area");
    // One track per card: a track holds at most one Planner.
    for (sort, (id, kind, role, payload)) in ROWS.iter().enumerate() {
        sqlx::query(
            "INSERT INTO tracks (id, area_id, title, sort, created_at, updated_at)
             VALUES (?1, 'a', ?1, ?2, 1, 1)",
        )
        .bind(id)
        .bind(sort as f64)
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("seed track {id}: {e}"));
        sqlx::query(
            "INSERT INTO cards (id, track_id, kind, sort, payload, created_at, updated_at, role)
             VALUES (?1, ?1, ?2, ?3, ?4, 1, 7, ?5)",
        )
        .bind(id)
        .bind(kind)
        .bind(sort as f64)
        .bind(payload)
        .bind(role)
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("seed card {id}: {e}"));
    }

    migrator(|version| version <= BACKFILL)
        .run(&pool)
        .await
        .expect("apply the backfill");

    let after = payloads(&pool).await;
    let mut stamped = Vec::new();
    for (id, kind, role, before) in ROWS {
        let (_, text, updated_at) = after
            .iter()
            .find(|(row, _, _)| row == id)
            .unwrap_or_else(|| panic!("card {id} is still there"));
        assert_eq!(*updated_at, 7, "card {id}: the backfill is not an edit");
        if oracle_stamps(kind, role, before) {
            let mut expected: Value = serde_json::from_str(before).unwrap();
            expected["permission_mode"] = Value::from("never");
            let actual: Value = serde_json::from_str(text).unwrap();
            assert_eq!(actual, expected, "card {id} is a Planner without a mode");
            stamped.push(*id);
        } else {
            assert_eq!(
                text, before,
                "card {id} must keep its payload byte for byte"
            );
        }
    }
    assert_eq!(
        stamped,
        ["planner-codex", "planner-claude", "planner-legacy-shape"],
        "the oracle itself must still pick the three Planner shapes"
    );

    // A rerun changes nothing.
    let backfill = calm_truth::MIGRATOR
        .iter()
        .find(|migration| migration.version == BACKFILL)
        .expect("the backfill is embedded");
    sqlx::raw_sql(&backfill.sql)
        .execute(&pool)
        .await
        .expect("rerun the backfill");
    assert_eq!(payloads(&pool).await, after);
}
