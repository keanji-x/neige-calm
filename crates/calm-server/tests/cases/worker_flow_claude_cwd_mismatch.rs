use crate::support;

use std::sync::Arc;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::SqlxRepo;
use serde_json::json;

use support::claude_hooks as hooks;
use support::worker_flow as wf;

#[tokio::test]
async fn claude_transcript_inband_cwd_mismatch_warns_but_keeps_flowing() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let seed = wf::seed_claude_card_and_runtime(
        &repo,
        "card-claude-inband-cwd",
        "session-claude-inband-cwd",
        "/tmp/claude-right",
    )
    .await;
    let transcript_root = tempfile::tempdir().unwrap();
    let path = transcript_root
        .path()
        .join("-tmp-claude-right")
        .join("session-claude-inband-cwd.jsonl");
    wf::write_transcript(
        &path,
        &[
            json!({
                "type": "permission-mode",
                "uuid": "perm-1",
                "timestamp": "2026-06-13T00:00:00Z"
            }),
            wf::claude_system("sys-1", "/tmp/claude-wrong"),
            wf::claude_user_string("user-1", "records keep flowing"),
        ],
    );
    hooks::post_claude_hook(
        &repo,
        "card-claude-inband-cwd",
        hooks::session_start("session-claude-inband-cwd", "/tmp/claude-right", &path),
    )
    .await;

    let (token, handle) = hooks::spawn_claude_source(repo.clone(), seed.runtime.clone(), &seed);
    wf::wait_until(wf::LIVENESS_BUDGET, || {
        let repo = repo.clone();
        async move { item_count(&repo, "card-claude-inband-cwd").await == 3 }
    })
    .await;
    token.cancel();
    handle.await.unwrap().unwrap();

    assert_eq!(item_count(&repo, "card-claude-inband-cwd").await, 3);
}

async fn item_count(repo: &SqlxRepo, card_id: &str) -> usize {
    repo.worker_flow_item_list_by_card(card_id, 0, 100, false)
        .await
        .unwrap()
        .len()
}
