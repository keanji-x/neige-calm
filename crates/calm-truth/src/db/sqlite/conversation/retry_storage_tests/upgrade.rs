//! Populated forward-upgrade preservation, including the deferred FK cycle.
use super::*;

async fn foreign_keys(pool: &sqlx::SqlitePool, table: &str) -> Vec<String> {
    sqlx::query_scalar("SELECT json_array(\"table\",\"from\",\"to\",on_update,on_delete,\"match\") FROM pragma_foreign_key_list(?1)")
        .bind(table).fetch_all(pool).await.unwrap()
}

#[tokio::test]
async fn canonical_retry_storage_inline_upgrade_preserves_populated_graph() {
    populated_upgrade(false).await;
    populated_upgrade(true).await;
    // Never-allocated presentations must not gain an artificial sequence row.
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/empty.db?mode=rwc", dir.path().display());
    let db = super::super::super::SqliteDb::connect(&url).await.unwrap();
    old_migrator().run(db.read_pool()).await.unwrap();
    let before: Vec<(String, i64)> =
        sqlx::query_as("SELECT name,seq FROM sqlite_sequence ORDER BY name")
            .fetch_all(db.read_pool())
            .await
            .unwrap();
    drop(db);
    let repo = SqlxRepo::open(&url).await.unwrap();
    assert_eq!(
        sqlx::query_as::<_, (String, i64)>("SELECT name,seq FROM sqlite_sequence ORDER BY name")
            .fetch_all(repo.pool())
            .await
            .unwrap(),
        before
    );
}

fn old_migrator() -> sqlx::migrate::Migrator {
    sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|m| m.version <= 183)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    }
}

async fn populated_upgrade(empty_presentations: bool) {
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/upgrade.db?mode=rwc", dir.path().display());
    let db = super::super::super::SqliteDb::connect(&url).await.unwrap();
    let old = old_migrator();
    old.run(db.read_pool()).await.unwrap();
    let repo = SqlxRepo {
        db,
        card_role_cache: super::super::super::CardRoleCache::new(),
        track_area_cache: super::super::super::TrackAreaCache::new(),
        database_id: Arc::new("upgrade-test".into()),
    };
    let (_, card) = super::super::tests::fixture_card(&repo).await;
    let s = seed_repo(repo, card, true).await;
    // Include native backing rows/presentations as well as Human and System.
    use crate::db::{RepoOutOfDomain, TranscriptReceiptItem};
    let track: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1")
        .bind(&s.card)
        .fetch_one(s.repo.pool())
        .await
        .unwrap();
    s.repo
        .transcript_receipt_restore(
            &s.carrier,
            &s.card,
            &track,
            "thread",
            "original-native-turn",
            &[TranscriptReceiptItem {
                item_uuid: "reply".into(),
                item_type: "agentMessage".into(),
                method: "item/completed".into(),
                params: r#"{"item":{"id":"reply","type":"agentMessage","text":"native reply"}}"#
                    .into(),
            }],
        )
        .await
        .unwrap();
    let native = s
        .repo
        .transcript_rows_of_thread(&s.card, "thread")
        .await
        .unwrap();
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    allocate_presentation_tx(
        &mut tx,
        &s.card,
        PresentationSource::NativeItem {
            row_id: native[0].id,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    // Leave both historical released and effective claims in the old database.
    sqlx::query(
        "UPDATE conversation_submission_inputs SET effective=0, released_by_attempt_id=?1 WHERE
            submission_id=?2 AND input_kind='human'",
    )
    .bind(ATTEMPT)
    .bind(s.root.as_str())
    .execute(s.repo.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE conversation_input_submissions SET native_correlation_json=?1 WHERE id=?2")
        .bind(r#"{"thread_id":"thread","native_turn_id":"original-native-turn"}"#)
        .bind(s.root.as_str())
        .execute(s.repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE conversation_presentations SET submission_id=?1")
        .bind(s.root.as_str())
        .execute(s.repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE sqlite_sequence SET seq=1000 WHERE name='conversation_presentations'")
        .execute(s.repo.pool())
        .await
        .unwrap();
    if empty_presentations {
        sqlx::query("DELETE FROM conversation_presentations")
            .execute(s.repo.pool())
            .await
            .unwrap();
    }
    let tables = [
        "conversation_turns",
        "conversation_input_submissions",
        "conversation_submission_inputs",
        "conversation_presentations",
    ];
    let mut before = Vec::new();
    for table in tables {
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
                .bind(table)
                .fetch_all(s.repo.pool())
                .await
                .unwrap();
        let columns = columns
            .iter()
            .map(|column| format!("\"{column}\""))
            .collect::<Vec<_>>()
            .join(",");
        let query = format!(
            "SELECT json_group_array(json(row)) FROM (SELECT json_array({columns}) AS row FROM {table} ORDER BY rowid)"
        );
        let rows: String = sqlx::query_scalar(&query)
            .fetch_one(s.repo.pool())
            .await
            .unwrap();
        let foreign_keys = foreign_keys(s.repo.pool(), table).await;
        before.push((table, query, rows, foreign_keys));
    }
    let sequences: Vec<(String, i64)> =
        sqlx::query_as("SELECT name,seq FROM sqlite_sequence ORDER BY name")
            .fetch_all(s.repo.pool())
            .await
            .unwrap();
    let objects: Vec<(String,String)> = sqlx::query_as("SELECT name,sql FROM sqlite_master WHERE type IN ('trigger','index') AND sql IS NOT NULL ORDER BY name").fetch_all(s.repo.pool()).await.unwrap();
    let Seed {
        repo, card, root, ..
    } = s;
    drop(repo);
    let upgraded = SqlxRepo::open(&url).await.unwrap();
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(upgraded.pool())
            .await
            .unwrap()
            .is_empty()
    );
    for (table, query, expected, keys) in before {
        let actual: String = sqlx::query_scalar(&query)
            .fetch_one(upgraded.pool())
            .await
            .unwrap();
        assert_eq!(actual, expected, "{table} lost or changed rows");
        let current_keys = foreign_keys(upgraded.pool(), table).await;
        for key in keys {
            assert!(current_keys.contains(&key), "{table} lost FK {key}");
        }
    }
    assert_eq!(
        sqlx::query_as::<_, (String, i64)>("SELECT name,seq FROM sqlite_sequence ORDER BY name")
            .fetch_all(upgraded.pool())
            .await
            .unwrap(),
        sequences
    );
    let restored_native = upgraded
        .transcript_rows_of_thread(&card, "thread")
        .await
        .unwrap();
    assert_eq!(restored_native.len(), native.len());
    assert_eq!(restored_native[0].id, native[0].id);
    for (name, sql) in objects {
        // The only creation-fence evolution is its declared live codec version.
        let sql = if name == "conversation_human_track_creation_fact_scope" {
            sql.replace("'$.schema_version')=4", "'$.schema_version')=5")
        } else if name == "acp_one_unresolved_submission" {
            // C2 intentionally replaces this receipt fence; its owner supplies
            // the head predicate. Every unrelated schema object stays exact.
            sql.replace(
                "state IN ('sending','unknown')",
                crate::db::sqlite::ACP_BLOCKING_RECEIPT_SQL,
            )
        } else {
            sql
        };
        let current: String = sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE name=?1")
            .bind(&name)
            .fetch_one(upgraded.pool())
            .await
            .unwrap();
        if !matches!(
            name.as_str(),
            "conversation_submission_plan_immutable"
                | "conversation_submission_only_private_erasure"
                | "conversation_submission_input_identity_immutable"
        ) {
            assert_eq!(current, sql, "{name}");
        }
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT seq FROM sqlite_sequence WHERE name='conversation_presentations'"
        )
        .fetch_one(upgraded.pool())
        .await
        .unwrap(),
        1000
    );
    let mut tx = begin_immediate_tx(upgraded.db()).await.unwrap();
    assert!(submission_plan_tx(&mut tx, &root).await.is_ok());
    let row = sqlx::query("SELECT retirement_reason,released_by_attempt_id FROM conversation_submission_inputs WHERE input_kind='human'").fetch_one(&mut **tx).await.unwrap();
    assert_eq!(row.get::<String, _>("retirement_reason"), "not_dispatched");
    assert_eq!(row.get::<String, _>("released_by_attempt_id"), ATTEMPT);
    let next = allocate_presentation_tx(
        &mut tx,
        &card,
        PresentationSource::SystemObservation(
            &SystemObservationId::parse("ffffffffffffffffffffffffffffffff").unwrap(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(next, 1001);
    tx.commit().await.unwrap();
    assert!(
        super::super::super::infra::check_no_unknown_future_migrations(upgraded.pool(), &old)
            .await
            .is_err(),
        "old binary must refuse upgraded database"
    );
}
