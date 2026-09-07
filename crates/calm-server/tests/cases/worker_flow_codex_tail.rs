use crate::support;

use std::sync::Arc;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::SqlxRepo;

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
    wf::wait_for_codex_cursor(&repo, "card-tail", 4).await;
    assert_eq!(item_count(&repo, "card-tail").await, 3);

    wf::append_rollout(
        &path,
        &[
            wf::function_call("call-1", "pwd"),
            wf::function_output("call-1", "/tmp"),
        ],
    );
    wf::wait_for_codex_cursor(&repo, "card-tail", 6).await;
    assert_eq!(item_count(&repo, "card-tail").await, 5);

    token.cancel();
    handle.await.unwrap().unwrap();

    wf::append_rollout(&path, &[wf::assistant_message("a2", "after restart")]);
    let (token, handle) =
        wf::spawn_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wf::wait_for_codex_cursor(&repo, "card-tail", 7).await;
    assert_eq!(item_count(&repo, "card-tail").await, 6);
    token.cancel();
    handle.await.unwrap().unwrap();
}

async fn item_count(repo: &SqlxRepo, card_id: &str) -> usize {
    repo.worker_flow_item_list_by_card(card_id, 0, 100, false)
        .await
        .unwrap()
        .len()
}
