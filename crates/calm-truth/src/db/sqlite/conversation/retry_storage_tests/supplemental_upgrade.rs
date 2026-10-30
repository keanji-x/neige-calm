//! Upgrade a populated canonical graph through the real shipped migrator.
use super::grok_turns_upgrade::{repo_on, rows, through};
use super::*;

#[tokio::test]
async fn pending_controls_migration_preserves_retries_claims_and_imported_presentations() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/supplemental.db?mode=rwc", dir.path().display());
    let db = super::super::super::SqliteDb::connect(&url).await.unwrap();
    through(201).run(db.writer_pool()).await.unwrap();
    let repo = repo_on(db);
    let (_, card) = super::super::tests::fixture_card(&repo).await;
    let s = seed_repo(repo, card, false).await;
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    successor(&mut tx, &s).await;
    tx.commit().await.unwrap();
    let track: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1")
        .bind(&s.card)
        .fetch_one(s.repo.pool())
        .await
        .unwrap();
    let raw = s
        .repo
        .fixture_transcript_insert(
            &s.carrier,
            &s.card,
            &track,
            "imported-thread",
            Some("imported-turn"),
            Some("imported-item"),
            Some("agentMessage"),
            "item/completed",
            r#"{"item":{"text":"old imported answer"}}"#,
            None,
        )
        .await
        .unwrap();
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    let boundary: i64 = sqlx::query_scalar(
        "SELECT COALESCE(max(sequence),0) FROM conversation_presentations WHERE card_id=?1",
    )
    .bind(&s.card)
    .fetch_one(&mut **tx)
    .await
    .unwrap();
    import_item_tx(&mut tx, &s.card, raw, boundary)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::query(
        "UPDATE conversation_presentations SET submission_id=?1 WHERE source_kind!='imported_item'",
    )
    .bind(s.root.as_str())
    .execute(s.repo.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE sqlite_sequence SET seq=1000 WHERE name='conversation_presentations'")
        .execute(s.repo.pool())
        .await
        .unwrap();
    let tables = [
        "conversation_turns",
        "conversation_input_submissions",
        "conversation_submission_inputs",
        "conversation_presentations",
        "conversation_input_retry_relations",
        "conversation_messages",
        "conversation_message_revisions",
    ];
    let mut before = Vec::new();
    for table in tables {
        before.push(rows(s.repo.pool(), table).await);
    }
    let sequences: Vec<(String, i64)> =
        sqlx::query_as("SELECT name,seq FROM sqlite_sequence ORDER BY name")
            .fetch_all(s.repo.pool())
            .await
            .unwrap();
    let db = s.repo.db();
    through(202).run(db.writer_pool()).await.unwrap();
    for (table, expected) in tables.into_iter().zip(before) {
        assert_eq!(
            rows(s.repo.pool(), table).await,
            expected,
            "upgrade changed {table}"
        );
    }
    assert_eq!(
        sqlx::query_as::<_, (String, i64)>("SELECT name,seq FROM sqlite_sequence ORDER BY name")
            .fetch_all(s.repo.pool())
            .await
            .unwrap(),
        sequences
    );
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(s.repo.pool())
            .await
            .unwrap()
            .is_empty()
    );
    // The old Initial remains unique; an inline positive-ordinal supplement is now representable.
    let id = crate::model::new_id();
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    let turn: String = sqlx::query_scalar(
        "SELECT logical_turn_id FROM conversation_input_submissions WHERE id=?1",
    )
    .bind(s.root.as_str())
    .fetch_one(&mut **tx)
    .await
    .unwrap();
    let digest = super::super::digest(&s.render).unwrap();
    sqlx::query(
        "INSERT INTO conversation_input_submissions(
         id,card_id,logical_turn_id,ordinal,kind,render_plan_json,render_plan_sha256,created_at_ms)
         VALUES(?1,?2,?3,1,'supplemental',?4,?5,1)",
    )
    .bind(&id)
    .bind(&s.card)
    .bind(&turn)
    .bind(serde_json::to_string(&s.render).unwrap())
    .bind(&digest)
    .execute(&mut **tx)
    .await
    .unwrap();
    assert!(
        sqlx::query(
            "INSERT INTO conversation_input_submissions(
         id,card_id,logical_turn_id,ordinal,kind,render_plan_json,render_plan_sha256,created_at_ms)
         VALUES(?1,?2,?3,2,'initial',?4,?5,1)"
        )
        .bind(crate::model::new_id())
        .bind(&s.card)
        .bind(turn)
        .bind(serde_json::to_string(&s.render).unwrap())
        .bind(digest)
        .execute(&mut **tx)
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
}
