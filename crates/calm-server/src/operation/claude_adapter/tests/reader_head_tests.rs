//! #1933 T3 — a read-only claude task whose declared `head` is not the track checkout's HEAD is
//! refused before spawn, as a codex one is (`codex_adapter/tests/reader_head_tests.rs`).
use super::support::*;
use crate::operation::workspace_lease::worker::TRACK_HEAD_MISMATCH;

fn git_rev(dir: &std::path::Path, rev: &str) -> String {
    let output = std::process::Command::new("git")
        .args(["rev-parse", rev])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// The harness checkout has two commits; the reader declares the first.
#[tokio::test]
async fn claude_reader_at_another_head_is_refused_before_spawn() {
    let harness = claude_worker_harness().await;
    let actual = git_rev(&harness.worktree, "HEAD");
    let declared = git_rev(&harness.worktree, "HEAD~1");
    sqlx::query(
        "INSERT INTO tasks (id, track_id, key, kind, goal, context_json, depends_on_json, status, \
         created_at_ms, updated_at_ms, access, head) \
         VALUES (?1, ?2, 'review', 'claude', 'review', 'null', '[]', 'dispatched', 1, 1, \
         'read_only', ?3)",
    )
    .bind(format!("{}:review", harness.track_id))
    .bind(&harness.track_id)
    .bind(&declared)
    .execute(harness.repo.pool())
    .await
    .unwrap();
    let message = try_prepare_claude_worker(&harness, "review")
        .await
        .expect_err("a reader at another head is refused")
        .to_string();
    assert!(
        message.contains(&format!("refused: {TRACK_HEAD_MISMATCH}:"))
            && message.contains(&declared)
            && message.contains(&actual),
        "{message}"
    );
}
