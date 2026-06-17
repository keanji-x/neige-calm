mod support;

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::{SqlxRepo, session_set_status_tx};
use calm_server::runtime_repo::RunStatus;
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

#[tokio::test]
async fn claude_tail_drains_records_appended_after_eof_when_runtime_exits_without_event() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-claude-tail-terminal-drain";
    let session_id = "session-claude-tail-terminal-drain";
    let cwd = "/tmp/claude-tail-terminal-drain";
    let seed = wf::seed_claude_card_and_runtime(&repo, card_id, session_id, cwd).await;
    let transcript_dir = tempfile::tempdir().unwrap();
    let path = transcript_dir.path().join(format!("{session_id}.jsonl"));
    wf::write_transcript(
        &path,
        &[
            wf::claude_user_string("user-terminal-1", "one"),
            wf::claude_assistant("assistant-terminal-1", cwd, vec![wf::claude_text("two")]),
            wf::claude_user_string("user-terminal-2", "three"),
            wf::claude_assistant("assistant-terminal-2", cwd, vec![wf::claude_text("four")]),
        ],
    );
    let initial_len = file_len(&path);

    let (_token, handle) =
        wf::spawn_claude_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wf::wait_until(Duration::from_secs(1), || {
        let repo = repo.clone();
        async move { item_count(&repo, card_id).await == 4 }
    })
    .await;
    assert_cursor(&repo, card_id, 4, initial_len).await;

    let mut tx = repo.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    session_set_status_tx(&mut tx, &seed.runtime.id, RunStatus::Exited)
        .await
        .unwrap();
    wf::append_transcript(
        &path,
        &[
            wf::claude_user_string("user-terminal-final", "five"),
            wf::claude_assistant(
                "assistant-terminal-final",
                cwd,
                vec![wf::claude_text("six")],
            ),
        ],
    );
    let final_len = file_len(&path);
    tx.commit().await.unwrap();

    wf::wait_until(Duration::from_millis(500), || {
        let repo = repo.clone();
        let finished = handle.is_finished();
        async move { item_count(&repo, card_id).await == 6 && finished }
    })
    .await;
    assert_cursor(&repo, card_id, 6, final_len).await;
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn claude_tail_drains_unterminated_final_record_when_runtime_exits() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-claude-tail-unterminated-final";
    let session_id = "session-claude-tail-unterminated-final";
    let cwd = "/tmp/claude-tail-unterminated-final";
    let seed = wf::seed_claude_card_and_runtime(&repo, card_id, session_id, cwd).await;
    let transcript_dir = tempfile::tempdir().unwrap();
    let path = transcript_dir.path().join(format!("{session_id}.jsonl"));
    wf::write_transcript(
        &path,
        &[
            wf::claude_user_string("user-unterminated-1", "one"),
            wf::claude_assistant(
                "assistant-unterminated-1",
                cwd,
                vec![wf::claude_text("two")],
            ),
        ],
    );
    let initial_len = file_len(&path);

    let (_token, handle) =
        wf::spawn_claude_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wf::wait_until(Duration::from_secs(1), || {
        let repo = repo.clone();
        async move { item_count(&repo, card_id).await == 2 }
    })
    .await;
    assert_cursor(&repo, card_id, 2, initial_len).await;

    let final_record = serde_json::to_string(&wf::claude_assistant(
        "assistant-unterminated-final",
        cwd,
        vec![wf::claude_text("three")],
    ))
    .unwrap();
    let mut tx = repo.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
    session_set_status_tx(&mut tx, &seed.runtime.id, RunStatus::Exited)
        .await
        .unwrap();
    append_raw(&path, &final_record);
    let final_len = file_len(&path);
    tx.commit().await.unwrap();

    wf::wait_until(Duration::from_millis(500), || {
        let repo = repo.clone();
        let finished = handle.is_finished();
        async move { item_count(&repo, card_id).await == 3 && finished }
    })
    .await;
    assert_cursor(&repo, card_id, 3, final_len).await;
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn claude_tail_terminal_drain_leaves_invalid_unterminated_tail_unrecorded() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-claude-tail-invalid-final";
    let session_id = "session-claude-tail-invalid-final";
    let cwd = "/tmp/claude-tail-invalid-final";
    let seed = wf::seed_claude_card_and_runtime(&repo, card_id, session_id, cwd).await;
    let transcript_dir = tempfile::tempdir().unwrap();
    let path = transcript_dir.path().join(format!("{session_id}.jsonl"));
    wf::write_transcript(
        &path,
        &[
            wf::claude_user_string("user-invalid-1", "one"),
            wf::claude_assistant("assistant-invalid-1", cwd, vec![wf::claude_text("two")]),
        ],
    );
    let initial_len = file_len(&path);

    let (_token, handle) =
        wf::spawn_claude_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wf::wait_until(Duration::from_secs(1), || {
        let repo = repo.clone();
        async move { item_count(&repo, card_id).await == 2 }
    })
    .await;
    assert_cursor(&repo, card_id, 2, initial_len).await;

    let mut tx = repo.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
    session_set_status_tx(&mut tx, &seed.runtime.id, RunStatus::Exited)
        .await
        .unwrap();
    append_raw(&path, "{not-json");
    tx.commit().await.unwrap();

    wf::wait_until(Duration::from_millis(500), || {
        let finished = handle.is_finished();
        async move { finished }
    })
    .await;
    assert_eq!(item_count(&repo, card_id).await, 2);
    assert_cursor(&repo, card_id, 2, initial_len).await;
    handle.await.unwrap().unwrap();
}

fn file_len(path: &std::path::Path) -> i64 {
    std::fs::metadata(path).unwrap().len() as i64
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

async fn assert_cursor(repo: &SqlxRepo, card_id: &str, record_index: i64, byte_offset: i64) {
    let cursor = repo
        .worker_flow_cursor_get(card_id, CLAUDE_TRANSCRIPT_SOURCE_KIND)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cursor.record_index, record_index);
    assert_eq!(cursor.byte_offset, byte_offset);
}
