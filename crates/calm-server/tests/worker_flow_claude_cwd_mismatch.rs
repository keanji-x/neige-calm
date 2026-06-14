mod support;

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::worker_flow::claude_transcript::slug_for_projects;
use serde_json::json;

use support::worker_flow as wf;

#[tokio::test]
async fn claude_transcript_path_slug_mismatch_exits_without_ingesting_wrong_file() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let seed = wf::seed_claude_card_and_runtime(
        &repo,
        "card-claude-wrong-slug",
        "session-claude-wrong-slug",
        "/tmp/claude-right",
    )
    .await;
    let transcript_root = tempfile::tempdir().unwrap();
    let path = transcript_path(
        transcript_root.path(),
        &slug_for_projects("/tmp/claude-wrong"),
        "session-claude-wrong-slug",
    );
    wf::write_transcript(
        &path,
        &[
            json!({
                "type": "permission-mode",
                "uuid": "perm-1",
                "timestamp": "2026-06-13T00:00:00Z"
            }),
            wf::claude_user_string("user-1", "wrong file"),
        ],
    );

    let (_token, handle) =
        wf::spawn_claude_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    handle.await.unwrap().unwrap();

    assert_eq!(item_count(&repo, "card-claude-wrong-slug").await, 0);
}

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
    let path = transcript_path(
        transcript_root.path(),
        &slug_for_projects("/tmp/claude-right"),
        "session-claude-inband-cwd",
    );
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

    let (token, handle) =
        wf::spawn_claude_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wf::wait_until(Duration::from_secs(1), || {
        let repo = repo.clone();
        async move { item_count(&repo, "card-claude-inband-cwd").await == 3 }
    })
    .await;
    token.cancel();
    handle.await.unwrap().unwrap();

    assert_eq!(item_count(&repo, "card-claude-inband-cwd").await, 3);
}

fn transcript_path(root: &std::path::Path, slug: &str, session_id: &str) -> std::path::PathBuf {
    root.join(".claude")
        .join("projects")
        .join(slug)
        .join(format!("{session_id}.jsonl"))
}

async fn item_count(repo: &SqlxRepo, card_id: &str) -> usize {
    repo.worker_flow_item_list_by_card(card_id, 0, 100, false)
        .await
        .unwrap()
        .len()
}
