use crate::support;

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::worker_flow::cursor::CODEX_ROLLOUT_SOURCE_KIND;
use serde_json::Value;

use support::worker_flow as wf;

#[tokio::test]
async fn codex_rollout_position_survives_idle_poll_before_append() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let thread_id = "thread-position";
    let seed = wf::seed_card_and_runtime(&repo, "card-position", Some(thread_id)).await;
    let codex_home = tempfile::tempdir().unwrap();
    let path = wf::rollout_path(codex_home.path(), thread_id);

    let mut lines = vec![wf::session_meta(thread_id)];
    for idx in 0..100 {
        lines.push(wf::user_message(
            &format!("u{idx}"),
            &format!("message {idx}"),
        ));
    }
    wf::write_rollout(&path, &lines);

    let (token, handle) =
        wf::spawn_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wf::wait_until(Duration::from_secs(1), || {
        let repo = repo.clone();
        async move { item_count(&repo, "card-position").await == 100 }
    })
    .await;
    assert_cursor(&repo, "card-position", 101).await;
    tokio::time::sleep(Duration::from_millis(80)).await;

    let prior_max_seq = max_seq(&repo, "card-position").await;
    wf::append_rollout(&path, &[wf::assistant_message("a-next", "after idle poll")]);
    wf::wait_until(Duration::from_secs(1), || {
        let repo = repo.clone();
        async move { item_count(&repo, "card-position").await == 101 }
    })
    .await;

    let rows = repo
        .worker_flow_item_list_by_card("card-position", 0, 200, false)
        .await
        .unwrap();
    let appended: Value = serde_json::from_str(&rows.last().unwrap().payload).unwrap();
    assert_eq!(appended["seq"].as_u64().unwrap(), prior_max_seq + 1);

    token.cancel();
    handle.await.unwrap().unwrap();
}

async fn item_count(repo: &SqlxRepo, card_id: &str) -> usize {
    repo.worker_flow_item_list_by_card(card_id, 0, 200, false)
        .await
        .unwrap()
        .len()
}

async fn max_seq(repo: &SqlxRepo, card_id: &str) -> u64 {
    repo.worker_flow_item_list_by_card(card_id, 0, 200, false)
        .await
        .unwrap()
        .into_iter()
        .map(|row| {
            let payload: Value = serde_json::from_str(&row.payload).unwrap();
            payload["seq"].as_u64().unwrap()
        })
        .max()
        .unwrap()
}

async fn assert_cursor(repo: &SqlxRepo, card_id: &str, record_index: i64) {
    let cursor = repo
        .worker_flow_cursor_get(card_id, CODEX_ROLLOUT_SOURCE_KIND)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cursor.record_index, record_index);
}
