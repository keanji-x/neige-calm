mod support;

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::worker_flow::cursor::CODEX_ROLLOUT_SOURCE_KIND;

use support::worker_flow as wf;

#[tokio::test]
async fn codex_rollout_preserves_unterminated_final_line_until_complete() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let thread_id = "thread-torn";
    let seed = wf::seed_card_and_runtime(&repo, "card-torn", Some(thread_id)).await;
    let codex_home = tempfile::tempdir().unwrap();
    let path = wf::rollout_path(codex_home.path(), thread_id);
    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::user_message("u1", "one"),
            wf::assistant_message("a1", "two"),
        ],
    );

    let (token, handle) =
        wf::spawn_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wf::wait_until(Duration::from_secs(1), || {
        let repo = repo.clone();
        async move { item_count(&repo, "card-torn").await == 2 }
    })
    .await;
    assert_cursor(&repo, "card-torn", 3).await;

    let next = serde_json::to_string(&wf::reasoning("r1", "three")).unwrap();
    let split_at = next.len() / 2;
    append_raw(&path, &next[..split_at]);
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(item_count(&repo, "card-torn").await, 2);
    assert_cursor(&repo, "card-torn", 3).await;

    append_raw(&path, &format!("{}\n", &next[split_at..]));
    wf::wait_until(Duration::from_secs(1), || {
        let repo = repo.clone();
        async move { item_count(&repo, "card-torn").await == 3 }
    })
    .await;
    assert_cursor(&repo, "card-torn", 4).await;

    token.cancel();
    handle.await.unwrap().unwrap();
}

fn append_raw(path: &std::path::Path, text: &str) {
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(text.as_bytes()).unwrap();
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
