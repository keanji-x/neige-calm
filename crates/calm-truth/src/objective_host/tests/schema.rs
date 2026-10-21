use std::borrow::Cow;

use sqlx::{Connection, SqliteConnection};

// Refusal fixtures use the head schema; the upgrade fixture stops at the last
// released version before 0182, with real SQLx migration bookkeeping.
async fn migrated_schema(before_0182: bool) -> SqliteConnection {
    let mut db = SqliteConnection::connect("sqlite::memory:").await.unwrap();
    if before_0182 {
        let migrator = sqlx::migrate::Migrator {
            migrations: Cow::Owned(
                crate::MIGRATOR
                    .iter()
                    .filter(|migration| migration.version < 182)
                    .cloned()
                    .collect(),
            ),
            ..sqlx::migrate::Migrator::DEFAULT
        };
        migrator.run(&mut db).await.unwrap();
    } else {
        crate::MIGRATOR.run(&mut db).await.unwrap();
    }
    sqlx::raw_sql(
        "PRAGMA foreign_keys=ON; PRAGMA recursive_triggers=OFF;
         INSERT INTO areas(id,name,color,sort,created_at,updated_at)
         VALUES('area','a','#000',0,1,1);
         INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at)
         VALUES('t','area','t',0,1,1),('other','area','other',1,2,2);",
    )
    .execute(&mut db)
    .await
    .unwrap();
    for (pragma, expected) in [("foreign_keys", 1_i64), ("recursive_triggers", 0)] {
        let actual: i64 = sqlx::query_scalar(&format!("PRAGMA {pragma}"))
            .fetch_one(&mut db)
            .await
            .unwrap();
        assert_eq!(actual, expected);
    }
    sqlx::raw_sql(
        "INSERT INTO objective_events VALUES
         ('t',1,1,'host_idle',NULL,'{\"event\":\"host_idle\",\"queue_len\":0}','harness_turn','one',0),
         ('t',2,1,'host_idle',NULL,'{\"event\":\"host_idle\",\"queue_len\":1}','harness_turn','two',1),
         ('other',1,1,'host_idle',NULL,'{\"event\":\"host_idle\",\"queue_len\":0}','harness_turn','one',0);
         INSERT INTO objective_states VALUES('t',2,1,'{\"snapshot\":2}');
         INSERT INTO objective_intents(track_id,event_seq,ordinal,kind,intent_json,effect_key,created_at_ms)
         VALUES('t',1,0,'notify','{\"intent\":\"notify\"}','[\"effect\"]',0);",
    )
    .execute(&mut db)
    .await
    .unwrap();
    db
}

async fn schema() -> SqliteConnection {
    migrated_schema(false).await
}

#[tokio::test]
async fn rebuild_preserves_events_states_intents() {
    let mut db = migrated_schema(true).await;
    sqlx::query("UPDATE objective_intents SET status='settled',dispatched_at_ms=7,settled_at_ms=8,settlement_json='{}'")
        .execute(&mut db).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO objective_intents(track_id,event_seq,ordinal,kind,intent_json,effect_key,
         standard_hash,target_session_id,created_at_ms)
         VALUES('t',2,1,'run_check','{\"intent\":\"run_check\"}','[\"pending\"]','hash',NULL,9),
         ('other',1,0,'execute','{\"intent\":\"execute\"}','[\"other\"]',NULL,'session',10);
         INSERT INTO objective_states VALUES('other',1,3,'{\"snapshot\":1}');",
    )
    .execute(&mut db)
    .await
    .unwrap();
    let before = contents(&mut db).await;
    let tracks_before: Vec<(String, String)> =
        sqlx::query_as("SELECT id,title FROM tracks ORDER BY id")
            .fetch_all(&mut db)
            .await
            .unwrap();
    crate::MIGRATOR.run(&mut db).await.unwrap();
    let applied: bool =
        sqlx::query_scalar("SELECT success FROM _sqlx_migrations WHERE version=182")
            .fetch_one(&mut db)
            .await
            .unwrap();
    assert!(applied);
    let tracks_after: Vec<(String, String)> =
        sqlx::query_as("SELECT id,title FROM tracks ORDER BY id")
            .fetch_all(&mut db)
            .await
            .unwrap();
    assert_eq!(tracks_after, tracks_before);
    assert_eq!(contents(&mut db).await, before);
    let violations =
        sqlx::query_as::<_, (String, Option<i64>, String, i64)>("PRAGMA foreign_key_check")
            .fetch_all(&mut db)
            .await
            .unwrap();
    assert!(violations.is_empty(), "{violations:?}");
    for (sql, expected) in [
        (
            "UPDATE objective_events SET appended_at_ms=99 WHERE track_id='t'",
            "objective events are append-only",
        ),
        (
            "DELETE FROM objective_events WHERE track_id='t'",
            "objective events are append-only",
        ),
        (
            "INSERT OR REPLACE INTO objective_states VALUES('t',1,0,'{}')",
            "objective snapshot cannot be replaced",
        ),
        (
            "INSERT OR REPLACE INTO objective_intents SELECT * FROM objective_intents WHERE track_id='t' AND event_seq=1",
            "objective intent cannot be replaced",
        ),
        (
            "UPDATE objective_intents SET created_at_ms=99 WHERE track_id='t'",
            "immutable objective intent",
        ),
        (
            "UPDATE objective_intents SET status='pending' WHERE track_id='t' AND event_seq=1",
            "objective intent status cannot decrease",
        ),
        (
            "UPDATE objective_intents SET settlement_json='[]' WHERE track_id='t' AND event_seq=1",
            "objective intent settlement is write-once",
        ),
        (
            "INSERT OR REPLACE INTO objective_events SELECT * FROM objective_events WHERE track_id='t'",
            "objective events are append-only",
        ),
    ] {
        let error = sqlx::query(sql).execute(&mut db).await.expect_err(sql);
        assert!(error.to_string().contains(expected), "{sql}: {error}");
        assert_eq!(contents(&mut db).await, before, "{sql}");
    }
    // Exercise the renamed FK targets and the exception to append-only deletion.
    sqlx::query("DELETE FROM tracks WHERE id='t'")
        .execute(&mut db)
        .await
        .unwrap();
    let remaining: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM objective_events),
         (SELECT count(*) FROM objective_states),(SELECT count(*) FROM objective_intents)",
    )
    .fetch_one(&mut db)
    .await
    .unwrap();
    assert_eq!(remaining, (1, 1, 1));
    let remaining_tracks: Vec<String> = sqlx::query_scalar(
        "SELECT track_id FROM objective_events UNION ALL
         SELECT track_id FROM objective_states UNION ALL SELECT track_id FROM objective_intents",
    )
    .fetch_all(&mut db)
    .await
    .unwrap();
    assert_eq!(remaining_tracks, vec!["other", "other", "other"]);
}

#[tokio::test]
async fn report_block_and_ask_answered_kinds_pair_with_their_events() {
    let mut db = schema().await;
    for source in ["report_block", "ask_answered"] {
        for kind in [
            "definition_changed",
            "approval_granted",
            "requested",
            "check_finished",
            "revision_delivered",
            "settled",
            "closed",
            "host_idle",
            "usage",
        ] {
            let payload = match kind {
                "definition_changed" => {
                    serde_json::json!({"event": kind, "objective": {"key": "build"}})
                }
                "revision_delivered" | "host_idle" | "usage" => serde_json::json!({"event": kind}),
                _ => serde_json::json!({"event": kind, "objective": "build"}),
            };
            let key = match kind {
                "revision_delivered" | "host_idle" | "usage" => None,
                _ => Some("build"),
            };
            let before = contents(&mut db).await;
            let result =
                sqlx::query("INSERT INTO objective_events VALUES('t',3,1,?,?,?,?,'new',0)")
                    .bind(kind)
                    .bind(key)
                    .bind(payload.to_string())
                    .bind(source)
                    .execute(&mut db)
                    .await;
            let allowed = (source == "report_block" && kind == "definition_changed")
                || (source == "ask_answered" && kind == "approval_granted");
            if allowed {
                result.unwrap_or_else(|e| panic!("{source}/{kind}: {e}"));
                // Fresh database for each accepted insert: log rows cannot be deleted.
                db = schema().await;
            } else {
                let error = result.expect_err(&format!("{source}/{kind} was accepted"));
                assert!(
                    error.to_string().contains("CHECK constraint failed"),
                    "{error}"
                );
                assert_eq!(contents(&mut db).await, before);
            }
        }
    }
}

// Capture every stored field and the row counts, including cascade dependents.
async fn contents(db: &mut SqliteConnection) -> String {
    sqlx::query_scalar(
        "SELECT json_array(
         (SELECT json_group_array(json_array(track_id,seq,schema_version,kind,objective_key,
          payload_json,source_kind,source_key,appended_at_ms)) FROM (SELECT * FROM objective_events ORDER BY track_id,seq)),
         (SELECT json_group_array(json_array(track_id,seq,state_version,state_json)) FROM (SELECT * FROM objective_states ORDER BY track_id)),
         (SELECT json_group_array(json_array(track_id,event_seq,ordinal,kind,intent_json,effect_key,
          standard_hash,target_session_id,status,dispatched_at_ms,settled_at_ms,settlement_json,
          created_at_ms)) FROM (SELECT * FROM objective_intents ORDER BY track_id,event_seq,ordinal)))",
    )
    .fetch_one(db)
    .await
    .unwrap()
}

async fn refuse_conflicts(insert: &str, error: &str) {
    let mut db = schema().await;
    sqlx::query("UPDATE objective_intents SET status='settled',dispatched_at_ms=7,settled_at_ms=8,settlement_json='{}'")
        .execute(&mut db).await.unwrap();
    let before = contents(&mut db).await;
    for verb in ["INSERT OR REPLACE", "INSERT", "INSERT OR IGNORE"] {
        let sql = format!("{verb} {insert}");
        let result = sqlx::query(&sql).execute(&mut db).await;
        assert_eq!(contents(&mut db).await, before, "{sql} changed stored rows");
        let failure = result.expect_err(&sql);
        assert!(failure.to_string().contains(error), "{sql}: {failure}");
    }
}

#[tokio::test]
async fn objective_events_primary_key_replacement_is_refused() {
    refuse_conflicts(
        "INTO objective_events VALUES('t',1,1,'host_idle',NULL,
         '{\"event\":\"host_idle\",\"queue_len\":99}','harness_turn','new',99)",
        "objective events are append-only",
    )
    .await;
}

#[tokio::test]
async fn objective_events_source_replacement_is_refused() {
    refuse_conflicts(
        "INTO objective_events VALUES('t',3,1,'host_idle',NULL,
         '{\"event\":\"host_idle\",\"queue_len\":99}','harness_turn','one',99)",
        "objective events are append-only",
    )
    .await;
}

#[tokio::test]
async fn objective_states_replacement_cannot_reset_cas() {
    refuse_conflicts(
        "INTO objective_states VALUES('t',1,0,'{}')",
        "objective snapshot cannot be replaced",
    )
    .await;
}

#[tokio::test]
async fn objective_intents_identity_replacement_is_refused() {
    refuse_conflicts(
        "INTO objective_intents(track_id,event_seq,ordinal,kind,intent_json,effect_key,created_at_ms)
         VALUES('t',1,0,'notify','{\"intent\":\"notify\",\"changed\":true}','[\"new\"]',99)",
        "objective intent cannot be replaced",
    )
    .await;
}

#[tokio::test]
async fn objective_intents_effect_replacement_is_refused() {
    refuse_conflicts(
        "INTO objective_intents(track_id,event_seq,ordinal,kind,intent_json,effect_key,created_at_ms)
         VALUES('t',2,1,'notify','{\"intent\":\"notify\",\"changed\":true}','[\"effect\"]',99)",
        "objective intent cannot be replaced",
    )
    .await;
}

// Use fresh declared keys so only the hidden rowid could collide. The event
// collision would cascade away its intent; the intent collision would erase its
// settlement; the snapshot collision would move it to another track.
#[tokio::test]
async fn objective_events_explicit_rowid_is_refused() {
    refuse_conflicts(
        "INTO objective_events(rowid,track_id,seq,schema_version,kind,objective_key,
         payload_json,source_kind,source_key,appended_at_ms)
         VALUES(1,'t',3,1,'host_idle',NULL,'{\"event\":\"host_idle\",\"queue_len\":99}',
         'harness_turn','new',99)",
        "has no column named rowid",
    )
    .await;
}

#[tokio::test]
async fn objective_states_explicit_rowid_is_refused() {
    refuse_conflicts(
        "INTO objective_states(rowid,track_id,seq,state_version,state_json)
         VALUES(1,'other',1,0,'{}')",
        "has no column named rowid",
    )
    .await;
}

#[tokio::test]
async fn objective_intents_explicit_rowid_is_refused() {
    refuse_conflicts(
        "INTO objective_intents(rowid,track_id,event_seq,ordinal,kind,intent_json,effect_key,created_at_ms)
         VALUES(1,'t',2,1,'notify','{\"intent\":\"notify\"}','[\"new\"]',99)",
        "has no column named rowid",
    )
    .await;
}

#[tokio::test]
async fn partial_settlement_and_dispatch_are_refused() {
    let mut db = schema().await;
    for status in ["pending", "dispatched"] {
        if status == "dispatched" {
            sqlx::query("UPDATE objective_intents SET status='dispatched',dispatched_at_ms=5")
                .execute(&mut db)
                .await
                .unwrap();
        }
        for change in [
            "settled_at_ms=10",
            "settlement_json='{}'",
            "settled_at_ms=10,settlement_json='{}'",
            "status='settled',settled_at_ms=10",
            "status='settled',settlement_json='{}'",
            "status='dispatched',dispatched_at_ms=NULL",
            "status='pending',dispatched_at_ms=10",
        ] {
            let before = contents(&mut db).await;
            let result = sqlx::query(&format!("UPDATE objective_intents SET {change}"))
                .execute(&mut db)
                .await;
            assert!(result.is_err(), "{status}: {change} was accepted");
            assert_eq!(contents(&mut db).await, before);
        }
    }
}

#[tokio::test]
async fn legitimate_settlement_paths_pass() {
    for dispatched in [false, true] {
        let mut db = schema().await;
        if dispatched {
            sqlx::query("UPDATE objective_intents SET status='dispatched',dispatched_at_ms=5")
                .execute(&mut db)
                .await
                .unwrap();
        }
        sqlx::query(
            "UPDATE objective_intents SET status='settled',settled_at_ms=10,settlement_json='{}'",
        )
        .execute(&mut db)
        .await
        .unwrap();
        let before = contents(&mut db).await;
        assert!(
            sqlx::query("UPDATE objective_intents SET settled_at_ms=11,settlement_json='[]'")
                .execute(&mut db)
                .await
                .is_err()
        );
        assert_eq!(contents(&mut db).await, before);
    }
}
