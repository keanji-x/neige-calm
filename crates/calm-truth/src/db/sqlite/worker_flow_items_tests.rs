//! #695 PR2 — storage-layer tests for `worker_flow_items`. Mirrors the
//! harness-item db coverage: insert via the `_tx` free fn, list/page by
//! card, delete-by-card, and the durability guarantee that a card delete
//! turns `card_id` NULL (FK `ON DELETE SET NULL`) instead of cascading
//! the row away.
use super::{
    SqlxRepo, area_create_tx, card_create_with_id_tx, session_insert_tx, track_create_tx,
    worker_flow_item_insert_tx, worker_flow_items_delete_by_card_tx,
};
use crate::db::RepoRead;
use crate::model::{CardRole, NewArea, NewCard, NewTrack, RequestTheme};
use calm_types::worker::{
    LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession, WorkerSessionId,
    WorkerSessionState,
};

/// Seed a real area → track → card chain through the typed `_tx` helpers
/// (so the FKs target genuine rows) and return the card/track ids.
async fn seed_card_and_session(repo: &SqlxRepo, session_id: &str) -> (String, String) {
    let mut tx = repo.pool().begin().await.unwrap();
    let area = area_create_tx(
        &mut tx,
        NewArea {
            name: "c".into(),
            color: "#fff".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let track = track_create_tx(
        &mut tx,
        NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "w".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        None,
        &crate::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
        None,
        repo.track_area_cache(),
    )
    .await
    .unwrap();
    let card = card_create_with_id_tx(
        &mut tx,
        "card-1".into(),
        NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "worker".into(),
            sort: None,
            payload: serde_json::json!({}),
        },
        CardRole::Worker,
        true,
        repo.card_role_cache(),
    )
    .await
    .unwrap();
    session_insert_tx(
        &mut tx,
        WorkerSession {
            id: WorkerSessionId::from(session_id),
            track_id: track.id.clone(),
            provider: WorkerProviderKind::Codex,
            mode: SessionMode::Resumable,
            contract: WorkerContract::Executor,
            parent_session_id: None,
            requester_session_id: None,
            state: WorkerSessionState::Running,
            mcp_token_hash: None,
            thread_id: Some(format!("thread-{session_id}")),
            agent_session_id: Some(format!("agent-{session_id}")),
            active_turn_id: None,
            terminal_run_id: None,
            card_id: Some(card.id.clone()),
            handle_state_json: None,
            liveness: LivenessTag::Alive,
            liveness_probed_at_ms: None,
            exit_code: None,
            exit_interpretation: None,
            spawn_op_id: None,
            last_activity_ms: None,
            last_thread_status: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            completed_at_ms: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (card.id.to_string(), track.id.to_string())
}

#[tokio::test]
async fn insert_list_paging_delete_and_set_null_on_card_delete() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let session_id = "rt-flow-item-1";
    let (card_id, track_id) = seed_card_and_session(&repo, session_id).await;

    // Insert three flow items for the card via the `_tx` free fn.
    let mut ids = Vec::new();
    for (n, kind) in [
        (1_i64, "user_message"),
        (2, "assistant_message"),
        (3, "tool_call"),
    ] {
        let mut tx = repo.pool().begin().await.unwrap();
        let id = worker_flow_item_insert_tx(
            &mut tx,
            Some(&card_id),
            Some(session_id),
            Some(&track_id),
            Some(session_id),
            kind,
            &format!(r#"{{"kind":"{kind}","seq":{n}}}"#),
            1_000 + n,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        ids.push(id);
    }

    // Ascending list returns all three in id order.
    let asc = repo
        .worker_flow_item_list_by_card(&card_id, 0, 100, false)
        .await
        .unwrap();
    assert_eq!(asc.iter().map(|r| r.id).collect::<Vec<_>>(), ids);
    assert_eq!(asc[0].kind, "user_message");
    assert_eq!(asc[0].card_id.as_deref(), Some(card_id.as_str()));
    assert_eq!(asc[0].captured_session_id.as_deref(), Some(session_id));
    assert_eq!(asc[0].worker_session_id.as_deref(), Some(session_id));

    // Ascending paging: after the first id, limit 1 -> the second row.
    let page = repo
        .worker_flow_item_list_by_card(&card_id, ids[0], 1, false)
        .await
        .unwrap();
    assert_eq!(page.iter().map(|r| r.id).collect::<Vec<_>>(), vec![ids[1]]);

    // Descending: newest-first cursor (after_id = 0 -> from the tip),
    // but rows still come back in ascending id order (reversed in-fn).
    let desc = repo
        .worker_flow_item_list_by_card(&card_id, 0, 2, true)
        .await
        .unwrap();
    assert_eq!(
        desc.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![ids[1], ids[2]]
    );

    // Durability guarantee: deleting the card must NOT destroy the rows;
    // `ON DELETE SET NULL` leaves them present with `card_id = NULL`.
    {
        let mut tx = repo.pool().begin().await.unwrap();
        super::card_delete_tx(&mut tx, &card_id, repo.card_role_cache())
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    // The card-scoped query no longer matches (card_id is now NULL)...
    let after_card_delete = repo
        .worker_flow_item_list_by_card(&card_id, 0, 100, false)
        .await
        .unwrap();
    assert!(
        after_card_delete.is_empty(),
        "card_id should be NULL, not match"
    );
    // ...but the rows survive with NULL card_id.
    let (surviving, null_cards): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COUNT(*) FILTER (WHERE card_id IS NULL) FROM worker_flow_items",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(surviving, 3, "rows must survive card delete");
    assert_eq!(null_cards, 3, "FK ON DELETE SET NULL must null card_id");
}

#[tokio::test]
async fn delete_by_card_tx_purges_rows() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let session_id = "rt-flow-item-delete";
    let (card_id, track_id) = seed_card_and_session(&repo, session_id).await;
    for n in 1..=2 {
        let mut tx = repo.pool().begin().await.unwrap();
        worker_flow_item_insert_tx(
            &mut tx,
            Some(&card_id),
            Some(session_id),
            Some(&track_id),
            Some(session_id),
            "user_message",
            &format!(r#"{{"seq":{n}}}"#),
            n,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    let mut tx = repo.pool().begin().await.unwrap();
    worker_flow_items_delete_by_card_tx(&mut tx, &card_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let rows = repo
        .worker_flow_item_list_by_card(&card_id, 0, 100, false)
        .await
        .unwrap();
    assert!(rows.is_empty(), "explicit delete-by-card must purge rows");
}

// ---------------------------------------------------------------------------
// #1316 S4a — migration 0086 renames `runtime_id` to `captured_session_id`.
//
// This slice started as a DROP, on the claim that the column duplicated
// `worker_session_id` and that the id was also in the payload. Two review
// channels falsified the second half by replaying the real migration chain,
// and the test below is that counter-example, kept as a regression pin: the
// shape it seeds is exactly the one a drop would have destroyed.
// ---------------------------------------------------------------------------

fn migrator_through_0085() -> sqlx::migrate::Migrator {
    sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|migration| migration.version <= 85)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    }
}

#[tokio::test]
async fn migration_0086_preserves_the_id_the_payload_does_not_carry() {
    let dir = tempfile::tempdir().expect("temporary database directory");
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("pre-pr5.sqlite").display()
    );
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("open migration fixture");
    migrator_through_0085()
        .run(&pool)
        .await
        .expect("apply migrations through 0085");

    // The pre-PR5 shape, as `0049` leaves it: the resolved RUNTIME id in the
    // un-FK'd column, no session mirror so `worker_session_id` is NULL, and a
    // payload whose `session_id` is the PROVIDER's agent session string —
    // a different value. `0055` then dropped `runtimes`; its step-1 bridge
    // carries the mapping into `worker_sessions.agent_session_id`, so the id
    // is recoverable — but only by an ambiguous join, and only while that
    // mirror survives. This column answers it directly.
    sqlx::query(
        "INSERT INTO worker_flow_items \
         (card_id, runtime_id, track_id, worker_session_id, kind, payload, created_at_ms) \
         VALUES (NULL, 'rt-1', NULL, NULL, 'user_message', \
                 '{\"type\":\"userMessage\",\"session_id\":\"agent-sess-abc\"}', 1)",
    )
    .execute(&pool)
    .await
    .expect("seed the pre-PR5 row shape");
    pool.close().await;

    let repo = SqlxRepo::open(&url).await.expect("migrate through 0086");
    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('worker_flow_items') ORDER BY name")
            .fetch_all(repo.pool())
            .await
            .expect("read post-0086 column list");
    assert!(
        columns.iter().any(|c| c == "captured_session_id"),
        "0086 must rename the column, not remove it; got {columns:?}"
    );
    assert!(
        !columns.iter().any(|c| c == "runtime_id"),
        "the retiring spelling must be gone; got {columns:?}"
    );

    let (captured, payload): (Option<String>, String) =
        sqlx::query_as("SELECT captured_session_id, payload FROM worker_flow_items")
            .fetch_one(repo.pool())
            .await
            .expect("read the migrated row");
    assert_eq!(
        captured.as_deref(),
        Some("rt-1"),
        "the rename must preserve the value; dropping the column would have \
         destroyed the only copy of this id"
    );
    let value: serde_json::Value = serde_json::from_str(&payload).expect("payload is JSON");
    assert_eq!(
        value.get("session_id").and_then(serde_json::Value::as_str),
        Some("agent-sess-abc"),
        "and the payload still carries a DIFFERENT id, which is why 'the \
         payload has it too' was not a safe premise for a drop"
    );
}
