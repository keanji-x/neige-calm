use crate::support;

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::worker_flow::cursor::CODEX_ROLLOUT_SOURCE_KIND;

use support::worker_flow as wf;

#[tokio::test]
async fn codex_rollout_rewrite_resets_cursor_and_reingests_shorter_history() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let thread_id = "thread-compact";
    let seed = wf::seed_card_and_runtime(&repo, "card-compact", Some(thread_id)).await;
    let codex_home = tempfile::tempdir().unwrap();
    let path = wf::rollout_path(codex_home.path(), thread_id);
    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::user_message("u1", "one"),
            wf::reasoning("r1", "two"),
            wf::assistant_message("a1", "three"),
            wf::function_call("call-1", "pwd"),
            wf::function_output("call-1", "/tmp"),
        ],
    );

    let (token, handle) =
        wf::spawn_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wf::wait_until(Duration::from_secs(1), || {
        let repo = repo.clone();
        async move { item_count(&repo).await == 5 }
    })
    .await;
    assert_cursor(&repo, 6).await;
    token.cancel();
    handle.await.unwrap().unwrap();

    // Codex compaction can rewrite the rollout to a shorter history. The
    // source keeps the append-only sink contract and resets the cursor to
    // re-read the compacted file instead of trying to delete old rows.
    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::user_message("u2", "after compact"),
            wf::assistant_message("a2", "short history"),
        ],
    );
    let (token, handle) =
        wf::spawn_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wf::wait_until(Duration::from_secs(1), || {
        let repo = repo.clone();
        async move { item_count(&repo).await == 7 }
    })
    .await;
    assert_cursor(&repo, 3).await;
    token.cancel();
    handle.await.unwrap().unwrap();
}

async fn item_count(repo: &SqlxRepo) -> usize {
    repo.worker_flow_item_list_by_card("card-compact", 0, 100, false)
        .await
        .unwrap()
        .len()
}

async fn assert_cursor(repo: &SqlxRepo, record_index: i64) {
    let cursor = repo
        .worker_flow_cursor_get("card-compact", CODEX_ROLLOUT_SOURCE_KIND)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cursor.record_index, record_index);
}
