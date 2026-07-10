//! #695 PR2 — storage-layer tests for `worker_flow_items`. Mirrors the
//! harness-item db coverage: insert via the `_tx` free fn, list/page by
//! card, delete-by-card, and the durability guarantee that a card delete
//! turns `card_id` NULL (FK `ON DELETE SET NULL`) instead of cascading
//! the row away.
use super::{
    SqlxRepo, card_create_with_id_tx, cove_create_tx, session_insert_tx, wave_create_tx,
    worker_flow_item_insert_tx, worker_flow_items_delete_by_card_tx,
};
use crate::db::RepoRead;
use crate::model::{CardRole, NewCard, NewCove, NewWave, RequestTheme};
use calm_types::worker::{
    LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession, WorkerSessionId,
    WorkerSessionState,
};

/// Seed a real cove → wave → card chain through the typed `_tx` helpers
/// (so the FKs target genuine rows) and return the card/wave ids.
async fn seed_card_and_session(repo: &SqlxRepo, session_id: &str) -> (String, String) {
    let mut tx = repo.pool().begin().await.unwrap();
    let cove = cove_create_tx(
        &mut tx,
        NewCove {
            name: "c".into(),
            color: "#fff".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let wave = wave_create_tx(
        &mut tx,
        NewWave {
            cove_id: cove.id.clone(),
            title: "w".into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        repo.wave_cove_cache(),
    )
    .await
    .unwrap();
    let card = card_create_with_id_tx(
        &mut tx,
        "card-1".into(),
        NewCard {
            wave_id: wave.id.clone(),
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
            wave_id: wave.id.clone(),
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
    (card.id.to_string(), wave.id.to_string())
}

#[tokio::test]
async fn insert_list_paging_delete_and_set_null_on_card_delete() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let session_id = "rt-flow-item-1";
    let (card_id, wave_id) = seed_card_and_session(&repo, session_id).await;

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
            Some(&wave_id),
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
    assert_eq!(asc[0].runtime_id.as_deref(), Some(session_id));
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
    let (card_id, wave_id) = seed_card_and_session(&repo, session_id).await;
    for n in 1..=2 {
        let mut tx = repo.pool().begin().await.unwrap();
        worker_flow_item_insert_tx(
            &mut tx,
            Some(&card_id),
            Some(session_id),
            Some(&wave_id),
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
