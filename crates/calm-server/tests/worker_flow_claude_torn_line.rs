mod support;

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::worker_flow::claude_transcript::CLAUDE_TRANSCRIPT_SOURCE_KIND;

use support::worker_flow as wf;

#[tokio::test]
async fn claude_transcript_preserves_unterminated_final_line_until_complete() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let seed = wf::seed_claude_card_and_runtime(
        &repo,
        "card-claude-torn",
        "session-claude-torn",
        "/tmp/claude-torn",
    )
    .await;
    let transcript_dir = tempfile::tempdir().unwrap();
    let path = transcript_dir.path().join("session-claude-torn.jsonl");
    wf::write_transcript(
        &path,
        &[
            wf::claude_system("sys-1", "/tmp/claude-torn"),
            wf::claude_user_string("user-1", "one"),
        ],
    );
    let first_len = file_len(&path);

    let (token, handle) =
        wf::spawn_claude_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wf::wait_until(Duration::from_secs(1), || {
        let repo = repo.clone();
        async move { item_count(&repo, "card-claude-torn").await == 2 }
    })
    .await;
    assert_cursor(&repo, "card-claude-torn", 2, first_len).await;

    let next = serde_json::to_string(&wf::claude_assistant(
        "assistant-1",
        "/tmp/claude-torn",
        vec![wf::claude_text("two")],
    ))
    .unwrap();
    let split_at = next.len() / 2;
    append_raw(&path, &next[..split_at]);
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(item_count(&repo, "card-claude-torn").await, 2);
    assert_cursor(&repo, "card-claude-torn", 2, first_len).await;

    append_raw(&path, &format!("{}\n", &next[split_at..]));
    let second_len = file_len(&path);
    wf::wait_until(Duration::from_secs(1), || {
        let repo = repo.clone();
        async move { item_count(&repo, "card-claude-torn").await == 3 }
    })
    .await;
    assert_cursor(&repo, "card-claude-torn", 3, second_len).await;

    token.cancel();
    handle.await.unwrap().unwrap();
}

fn append_raw(path: &std::path::Path, text: &str) {
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

fn file_len(path: &std::path::Path) -> i64 {
    std::fs::metadata(path).unwrap().len() as i64
}

async fn item_count(repo: &SqlxRepo, card_id: &str) -> usize {
    repo.worker_flow_item_list_by_card(card_id, 0, 100, false)
        .await
        .unwrap()
        .len()
}

async fn assert_cursor(repo: &SqlxRepo, card_id: &str, record_index: i64, byte_offset: i64) {
    let cursor = repo
        .worker_flow_cursor_get(card_id, CLAUDE_TRANSCRIPT_SOURCE_KIND)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cursor.record_index, record_index);
    assert_eq!(cursor.byte_offset, byte_offset);
}
