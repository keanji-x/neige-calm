use sqlx::{Connection, SqliteConnection};

// Execute the shipped SQL, including its FK actions, without connection defaults
// accidentally enabling DELETE triggers for REPLACE.
async fn schema() -> SqliteConnection {
    let mut db = SqliteConnection::connect("sqlite::memory:").await.unwrap();
    sqlx::raw_sql(
        "PRAGMA foreign_keys=ON; PRAGMA recursive_triggers=OFF;
         CREATE TABLE tracks(id TEXT PRIMARY KEY);
         INSERT INTO tracks VALUES('t'),('other');",
    )
    .execute(&mut db)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!("../../../migrations/0177_objective_host.sql"))
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

// Capture every stored field and the row counts, including cascade dependents.
async fn contents(db: &mut SqliteConnection) -> String {
    sqlx::query_scalar(
        "SELECT json_array(
         (SELECT json_group_array(json_array(track_id,seq,schema_version,kind,objective_key,
          payload_json,source_kind,source_key,appended_at_ms)) FROM objective_events),
         (SELECT json_group_array(json_array(track_id,seq,state_version,state_json)) FROM objective_states),
         (SELECT json_group_array(json_array(track_id,event_seq,ordinal,kind,intent_json,effect_key,
          standard_hash,target_session_id,status,dispatched_at_ms,settled_at_ms,settlement_json,
          created_at_ms)) FROM objective_intents))",
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
