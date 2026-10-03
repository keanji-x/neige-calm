//! #1968: a claude worker turn that an API error ended reads like a codex one in the Planner-readable
//! `cards/<id>/conversation.md`, from the worker's own transcript record.

use crate::support;

use std::sync::Arc;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::worker_flow::claude_transcript::CLAUDE_TRANSCRIPT_SOURCE_KIND;

use support::worker_flow as wf;

#[tokio::test]
async fn claude_api_error_shows_as_a_failed_turn_end_in_conversation_md() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-claude-turn-end";
    let cwd = "/tmp/claude-turn-end";
    let seed =
        wf::seed_claude_card_and_runtime(&repo, card_id, "session-claude-turn-end", cwd).await;
    let transcript_dir = tempfile::tempdir().unwrap();
    let path = transcript_dir.path().join("session-claude-turn-end.jsonl");
    let error_text = "API Error: 403 {\"type\":\"error\",\"error\":{\"type\":\"forbidden\",\"message\":\"Request not allowed for this organization\"}}";
    wf::write_transcript(
        &path,
        &[
            wf::claude_system("sys-1", cwd),
            wf::claude_user_string("user-1", "do the task"),
            wf::claude_api_error("assistant-err", cwd, error_text, "unknown"),
        ],
    );

    let (token, handle) =
        wf::spawn_claude_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wf::wait_until(wf::LIVENESS_BUDGET, || async {
        repo.worker_flow_cursor_get(card_id, CLAUDE_TRANSCRIPT_SOURCE_KIND)
            .await
            .unwrap()
            .is_some_and(|cursor| cursor.record_index == 3)
    })
    .await;
    token.cancel();
    handle.await.unwrap().unwrap();

    let conversation = wf::cat_conversation(&repo, &seed.card).await;
    assert!(
        conversation.contains(
            "- Turn ended: failed — API Error: 403: Request not allowed for this organization\n"
        ),
        "conversation.md = {conversation}"
    );
}
