use crate::support;

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::sqlite::{SqlxRepo, card_delete_tx, worker_flow_item_insert_tx};

use support::worker_flow as wf;

#[tokio::test]
async fn worker_flow_items_key_worker_session_id_by_runtime_id() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let thread_id = "thread-ws-keying";
    let seed = wf::seed_card_and_runtime(&repo, "card-ws-keying", Some(thread_id)).await;
    let card_id = seed.card.id.to_string();
    let runtime_id = seed.runtime.id.clone();
    let agent_session_id = seed.runtime.session_id.clone().unwrap();
    assert_ne!(runtime_id, agent_session_id);
    assert_ne!(runtime_id, thread_id);

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
    wf::wait_until(Duration::from_secs(1), || {
        let repo = repo.clone();
        let card_id = card_id.clone();
        async move { flow_item_count(&repo, &card_id).await == 2 }
    })
    .await;
    token.cancel();
    handle.await.unwrap().unwrap();

    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT worker_session_id, runtime_id
         FROM worker_flow_items
         WHERE card_id = ?1
         ORDER BY id",
    )
    .bind(&card_id)
    .fetch_all(repo.pool())
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    for (worker_session_id, row_runtime_id) in &rows {
        assert_eq!(worker_session_id, &runtime_id);
        assert_eq!(row_runtime_id, &runtime_id);
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
        assert_eq!(joined_id, runtime_id);
        assert_eq!(
            joined_agent_session_id.as_deref(),
            Some(agent_session_id.as_str())
        );
        assert_eq!(joined_thread_id.as_deref(), Some(thread_id));
    }

    sqlx::query("DELETE FROM worker_sessions WHERE id = ?1")
        .bind(&runtime_id)
        .execute(repo.pool())
        .await
        .unwrap();
    let rows_after_session_delete: Vec<(Option<String>, String)> = sqlx::query_as(
        "SELECT worker_session_id, runtime_id
         FROM worker_flow_items
         WHERE card_id = ?1
         ORDER BY id",
    )
    .bind(&card_id)
    .fetch_all(repo.pool())
    .await
    .unwrap();
    assert_eq!(rows_after_session_delete.len(), 2);
    for (worker_session_id, row_runtime_id) in rows_after_session_delete {
        assert_eq!(worker_session_id.as_deref(), None);
        assert_eq!(row_runtime_id, runtime_id);
    }
}

#[tokio::test]
async fn card_delete_preserves_worker_flow_items_and_nulls_card_and_session_keys() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let seed =
        wf::seed_card_and_runtime(&repo, "card-delete-preserves-flow", Some("thread-delete")).await;
    let card_id = seed.card.id.to_string();
    let runtime_id = seed.runtime.id.clone();
    let wave_id = seed.card.wave_id.as_str().to_string();

    let rows = [
        ("user_message", r#"{"text":"first"}"#, 1_i64),
        ("assistant_message", r#"{"text":"second"}"#, 2_i64),
    ];
    for (kind, payload, created_at_ms) in rows {
        let mut tx = repo.pool().begin().await.unwrap();
        worker_flow_item_insert_tx(
            &mut tx,
            Some(&card_id),
            Some(&runtime_id),
            Some(&wave_id),
            Some(&runtime_id),
            kind,
            payload,
            created_at_ms,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

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
            .bind(&runtime_id)
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
