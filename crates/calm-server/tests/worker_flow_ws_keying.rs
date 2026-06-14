mod support;

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;

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

    sqlx::query("DELETE FROM worker_sessions WHERE id = ?1")
        .bind(&runtime_id)
        .execute(repo.pool())
        .await
        .unwrap();
    let state = wf::app_state(repo.clone(), EventBus::new());
    calm_server::backfill_worker_sessions_from_runtimes_on_boot(&state)
        .await
        .unwrap();

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
    assert_eq!(flow_item_count(&repo, &card_id).await, 0);

    calm_server::backfill_worker_sessions_from_runtimes_on_boot(&state)
        .await
        .unwrap();
}

async fn flow_item_count(repo: &SqlxRepo, card_id: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM worker_flow_items WHERE card_id = ?1")
        .bind(card_id)
        .fetch_one(repo.pool())
        .await
        .unwrap()
}
