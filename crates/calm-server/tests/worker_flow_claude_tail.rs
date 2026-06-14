mod support;

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::worker_flow::claude_transcript::CLAUDE_TRANSCRIPT_SOURCE_KIND;

use support::worker_flow as wf;

#[tokio::test]
async fn claude_transcript_tail_records_and_resumes_from_byte_cursor() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let seed = wf::seed_claude_card_and_runtime(
        &repo,
        "card-claude-tail",
        "session-claude-tail",
        "/tmp/claude-tail",
    )
    .await;
    let transcript_dir = tempfile::tempdir().unwrap();
    let path = transcript_dir.path().join("session-claude-tail.jsonl");
    wf::write_transcript(
        &path,
        &[
            wf::claude_system("sys-1", "/tmp/claude-tail"),
            wf::claude_user_string("user-1", "one"),
            wf::claude_assistant(
                "assistant-1",
                "/tmp/claude-tail",
                vec![wf::claude_text("two")],
            ),
        ],
    );
    let first_len = file_len(&path);

    let (token, handle) =
        wf::spawn_claude_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wf::wait_until(Duration::from_secs(1), || {
        let repo = repo.clone();
        async move { item_count(&repo, "card-claude-tail").await == 3 }
    })
    .await;
    assert_cursor(&repo, "card-claude-tail", 3, first_len).await;

    wf::append_transcript(
        &path,
        &[wf::claude_assistant(
            "assistant-2",
            "/tmp/claude-tail",
            vec![wf::claude_text("three")],
        )],
    );
    let second_len = file_len(&path);
    wf::wait_until(Duration::from_secs(1), || {
        let repo = repo.clone();
        async move { item_count(&repo, "card-claude-tail").await == 4 }
    })
    .await;
    assert_cursor(&repo, "card-claude-tail", 4, second_len).await;

    token.cancel();
    handle.await.unwrap().unwrap();

    wf::append_transcript(&path, &[wf::claude_user_string("user-2", "after restart")]);
    let third_len = file_len(&path);
    let (token, handle) =
        wf::spawn_claude_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wf::wait_until(Duration::from_secs(1), || {
        let repo = repo.clone();
        async move { item_count(&repo, "card-claude-tail").await == 5 }
    })
    .await;
    assert_cursor(&repo, "card-claude-tail", 5, third_len).await;
    token.cancel();
    handle.await.unwrap().unwrap();
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
