use crate::support;

use std::sync::Arc;

use calm_exec::flow::{CaptureCheckpoint, CaptureOutcome, CapturePosition};
use calm_server::db::RepoOutOfDomain;
use calm_server::db::sqlite::{SqlxRepo, card_delete_tx};
use calm_server::worker_flow::cursor::CODEX_ROLLOUT_SOURCE_KIND;
use calm_truth::db::worker_flow_capture::{CaptureItem, WorkerFlowCapture};

use support::worker_flow as wf;

#[tokio::test]
async fn worker_flow_items_key_worker_session_id_not_agent_session_or_thread() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let thread_id = "thread-ws-keying";
    let seed = wf::seed_card_and_runtime(&repo, "card-ws-keying", Some(thread_id)).await;
    let card_id = seed.card.id.to_string();
    let session_id = seed.runtime.id.clone();
    let agent_session_id = seed.runtime.session_id.clone().unwrap();
    assert_ne!(session_id, agent_session_id);
    assert_ne!(session_id, thread_id);

    let codex_home = tempfile::tempdir().unwrap();
    let path = wf::rollout_path(codex_home.path(), thread_id);
    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::user_message("user-ws-keying", "run"),
            wf::assistant_message("assistant-ws-keying", "done"),
        ],
    );

    let (token, handle) =
        wf::spawn_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wf::wait_until(wf::LIVENESS_BUDGET, || {
        let repo = repo.clone();
        let card_id = card_id.clone();
        async move { flow_item_count(&repo, &card_id).await == 2 }
    })
    .await;
    token.cancel();
    handle.await.unwrap().unwrap();

    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT worker_session_id, captured_session_id
         FROM worker_flow_items
         WHERE card_id = ?1
         ORDER BY id",
    )
    .bind(&card_id)
    .fetch_all(repo.pool())
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    for (worker_session_id, row_captured) in &rows {
        assert_eq!(worker_session_id, &session_id);
        assert_eq!(row_captured, &session_id);
        assert_ne!(worker_session_id, &agent_session_id);
        assert_ne!(worker_session_id, thread_id);
    }

    let joined: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT ws.id, ws.agent_session_id, ws.thread_id
         FROM worker_flow_items w
         JOIN worker_sessions ws ON ws.id = w.worker_session_id
         WHERE w.card_id = ?1
         ORDER BY w.id",
    )
    .bind(&card_id)
    .fetch_all(repo.pool())
    .await
    .unwrap();
    assert_eq!(joined.len(), 2);
    for (joined_id, joined_agent_session_id, joined_thread_id) in joined {
        assert_eq!(joined_id, session_id);
        assert_eq!(
            joined_agent_session_id.as_deref(),
            Some(agent_session_id.as_str())
        );
        assert_eq!(joined_thread_id.as_deref(), Some(thread_id));
    }

    sqlx::query("DELETE FROM worker_sessions WHERE id = ?1")
        .bind(&session_id)
        .execute(repo.pool())
        .await
        .unwrap();
    let rows_after_session_delete: Vec<(Option<String>, String)> = sqlx::query_as(
        "SELECT worker_session_id, captured_session_id
         FROM worker_flow_items
         WHERE card_id = ?1
         ORDER BY id",
    )
    .bind(&card_id)
    .fetch_all(repo.pool())
    .await
    .unwrap();
    assert_eq!(rows_after_session_delete.len(), 2);
    for (worker_session_id, row_captured) in rows_after_session_delete {
        assert_eq!(worker_session_id.as_deref(), None);
        assert_eq!(row_captured, session_id);
    }
}

#[tokio::test]
async fn card_delete_preserves_worker_flow_items_and_nulls_card_and_session_keys() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let seed =
        wf::seed_card_and_runtime(&repo, "card-delete-preserves-flow", Some("thread-delete")).await;
    let card_id = seed.card.id.to_string();
    let session_id = seed.runtime.id.clone();
    let track_id = seed.card.track_id.as_str().to_string();

    let items = [
        ("user_message", r#"{"text":"first"}"#),
        ("assistant_message", r#"{"text":"second"}"#),
    ]
    .into_iter()
    .map(|(kind, payload)| CaptureItem {
        kind: kind.into(),
        payload: payload.into(),
    })
    .collect();
    let outcome = repo
        .worker_flow_capture_commit(&WorkerFlowCapture {
            card_id: card_id.clone(),
            source_kind: CODEX_ROLLOUT_SOURCE_KIND.into(),
            session_id: session_id.as_str().into(),
            track_id: Some(track_id),
            expected: CaptureCheckpoint::Missing,
            next: CapturePosition {
                source_path: "/tmp/rollout.jsonl".into(),
                record_index: 1,
                byte_offset: 0,
                last_source_uuid: None,
                last_line_hash: None,
            },
            items,
        })
        .await
        .unwrap();
    assert!(matches!(outcome, CaptureOutcome::Applied(_)));

    let mut tx = repo.pool().begin().await.unwrap();
    card_delete_tx(&mut tx, &card_id, repo.card_role_cache())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let card_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cards WHERE id = ?1")
        .bind(&card_id)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(card_count, 0);

    let session_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM worker_sessions WHERE id = ?1")
            .bind(&session_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(session_count, 0);

    let captured: Vec<(Option<String>, Option<String>, String, String)> = sqlx::query_as(
        "SELECT card_id, worker_session_id, kind, payload
         FROM worker_flow_items
         ORDER BY id",
    )
    .fetch_all(repo.pool())
    .await
    .unwrap();
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[0].0.as_deref(), None);
    assert_eq!(captured[0].1.as_deref(), None);
    assert_eq!(captured[0].2, "user_message");
    assert_eq!(captured[0].3, r#"{"text":"first"}"#);
    assert_eq!(captured[1].0.as_deref(), None);
    assert_eq!(captured[1].1.as_deref(), None);
    assert_eq!(captured[1].2, "assistant_message");
    assert_eq!(captured[1].3, r#"{"text":"second"}"#);
}

async fn flow_item_count(repo: &SqlxRepo, card_id: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM worker_flow_items WHERE card_id = ?1")
        .bind(card_id)
        .fetch_one(repo.pool())
        .await
        .unwrap()
}
