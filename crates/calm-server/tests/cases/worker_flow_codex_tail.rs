use crate::support;

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::worker_flow::cursor::CODEX_ROLLOUT_SOURCE_KIND;

use support::worker_flow as wf;

#[tokio::test]
async fn codex_rollout_tail_records_and_resumes_from_cursor() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let thread_id = "thread-tail";
    let seed = wf::seed_card_and_runtime(&repo, "card-tail", Some(thread_id)).await;
    let codex_home = tempfile::tempdir().unwrap();
    let path = wf::rollout_path(codex_home.path(), thread_id);
    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::user_message("u1", "one"),
            wf::reasoning("r1", "two"),
            wf::assistant_message("a1", "three"),
        ],
    );

    let (token, handle) =
        wf::spawn_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wf::wait_until(Duration::from_secs(1), || {
        let repo = repo.clone();
        async move { item_count(&repo, "card-tail").await == 3 }
    })
    .await;
    assert_cursor(&repo, "card-tail", 4).await;

    wf::append_rollout(
        &path,
        &[
            wf::function_call("call-1", "pwd"),
            wf::function_output("call-1", "/tmp"),
        ],
    );
    wf::wait_until(Duration::from_secs(1), || {
        let repo = repo.clone();
        async move { item_count(&repo, "card-tail").await == 5 }
    })
    .await;
    assert_cursor(&repo, "card-tail", 6).await;

    token.cancel();
    handle.await.unwrap().unwrap();

    wf::append_rollout(&path, &[wf::assistant_message("a2", "after restart")]);
    let (token, handle) =
        wf::spawn_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wf::wait_until(Duration::from_secs(1), || {
        let repo = repo.clone();
        async move { item_count(&repo, "card-tail").await == 6 }
    })
    .await;
    wait_for_cursor(&repo, "card-tail", 7).await;
    token.cancel();
    handle.await.unwrap().unwrap();
}

async fn item_count(repo: &SqlxRepo, card_id: &str) -> usize {
    repo.worker_flow_item_list_by_card(card_id, 0, 100, false)
        .await
        .unwrap()
        .len()
}

async fn assert_cursor(repo: &SqlxRepo, card_id: &str, record_index: i64) {
    let cursor = repo
        .worker_flow_cursor_get(card_id, CODEX_ROLLOUT_SOURCE_KIND)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cursor.record_index, record_index);
}

async fn wait_for_cursor(repo: &SqlxRepo, card_id: &str, record_index: i64) {
    wf::wait_until(Duration::from_millis(120), || async {
        repo.worker_flow_cursor_get(card_id, CODEX_ROLLOUT_SOURCE_KIND)
            .await
            .unwrap()
            .is_some_and(|cursor| cursor.record_index == record_index)
    })
    .await;
}
