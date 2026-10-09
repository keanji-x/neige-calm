use crate::support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::SqlxRepo;
use calm_types::worker_flow::{MessageBlock, WorkerFlowItem};
use serde_json::json;

use support::claude_hooks as hooks;
use support::worker_flow as wf;

const DOTTED_CWD: &str = "/x/repo/.claude/worktrees/track-abc";
// Directory name Claude Code 2.1.280 created for DOTTED_CWD.
const CLI_PROJECT_DIR: &str = "-x-repo--claude-worktrees-track-abc";

#[tokio::test]
async fn claude_transcript_source_reads_hook_reported_path_for_dotted_cwd() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-claude-dotted-cwd";
    let session_id = "session-claude-dotted-cwd";
    let seed = wf::seed_claude_card_and_runtime(&repo, card_id, session_id, DOTTED_CWD).await;
    let root = tempfile::tempdir().unwrap();
    let path = transcript_file(root.path(), CLI_PROJECT_DIR, session_id, "dotted cwd");
    hooks::post_claude_hook(
        &repo,
        card_id,
        hooks::session_start(session_id, DOTTED_CWD, &path),
    )
    .await;

    let (token, handle) = hooks::spawn_claude_source(repo.clone(), seed.runtime.clone(), &seed);
    wait_for_user_text(&repo, card_id, "dotted cwd").await;
    token.cancel();
    handle.await.unwrap().unwrap();

    assert_eq!(user_texts(&repo, card_id).await, vec!["dotted cwd"]);
}

#[tokio::test]
async fn claude_transcript_source_waits_for_session_hook_before_ingesting() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-claude-hook-later";
    let session_id = "session-claude-hook-later";
    let seed = wf::seed_claude_card_and_runtime(&repo, card_id, session_id, DOTTED_CWD).await;
    let root = tempfile::tempdir().unwrap();
    // No layout rule: any absolute path Claude reports is used as-is.
    let path = write_transcript_at(root.path().join("anywhere").join("t.jsonl"), "hook later");

    let (token, handle) = hooks::spawn_claude_source(repo.clone(), seed.runtime.clone(), &seed);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!handle.is_finished(), "source must keep waiting for a hook");
    assert_eq!(item_count(&repo, card_id).await, 0);

    hooks::post_claude_hook(
        &repo,
        card_id,
        hooks::session_start(session_id, DOTTED_CWD, &path),
    )
    .await;
    wait_for_user_text(&repo, card_id, "hook later").await;
    token.cancel();
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn claude_transcript_source_ignores_hooks_for_another_session_on_the_card() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-claude-other-session";
    let session_id = "session-claude-mine";
    let seed = wf::seed_claude_card_and_runtime(&repo, card_id, session_id, DOTTED_CWD).await;
    let root = tempfile::tempdir().unwrap();
    let other = transcript_file(root.path(), "-other", "session-claude-other", "other");
    let mine = transcript_file(root.path(), CLI_PROJECT_DIR, session_id, "mine");
    hooks::post_claude_hook(
        &repo,
        card_id,
        hooks::session_start("session-claude-other", DOTTED_CWD, &other),
    )
    .await;

    let (token, handle) = hooks::spawn_claude_source(repo.clone(), seed.runtime.clone(), &seed);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !handle.is_finished(),
        "another session's hook must not end the wait"
    );
    assert_eq!(item_count(&repo, card_id).await, 0);

    hooks::post_claude_hook(
        &repo,
        card_id,
        hooks::session_start(session_id, DOTTED_CWD, &mine),
    )
    .await;
    wait_for_user_text(&repo, card_id, "mine").await;
    token.cancel();
    handle.await.unwrap().unwrap();

    assert_eq!(user_texts(&repo, card_id).await, vec!["mine"]);
}

#[tokio::test]
async fn claude_transcript_source_exits_without_ingesting_on_conflicting_hook_paths() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-claude-conflicting-paths";
    let session_id = "session-claude-conflicting";
    let seed = wf::seed_claude_card_and_runtime(&repo, card_id, session_id, DOTTED_CWD).await;
    let root = tempfile::tempdir().unwrap();
    for dir in [CLI_PROJECT_DIR, "-x-repo-.claude-worktrees-track-abc"] {
        let path = transcript_file(root.path(), dir, session_id, "conflict");
        hooks::post_claude_hook(
            &repo,
            card_id,
            hooks::session_start(session_id, DOTTED_CWD, &path),
        )
        .await;
    }

    let (_token, handle) = hooks::spawn_claude_source(repo.clone(), seed.runtime.clone(), &seed);
    tokio::time::timeout(wf::LIVENESS_BUDGET, handle)
        .await
        .expect("conflicting transcript paths must end the source while the runtime runs")
        .unwrap()
        .unwrap();

    assert_eq!(item_count(&repo, card_id).await, 0);
}

#[tokio::test]
async fn claude_transcript_source_uses_main_path_from_subagent_stop_hook() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-claude-subagent-stop";
    let session_id = "session-claude-subagent-stop";
    let seed = wf::seed_claude_card_and_runtime(&repo, card_id, session_id, DOTTED_CWD).await;
    let root = tempfile::tempdir().unwrap();
    let main = transcript_file(root.path(), CLI_PROJECT_DIR, session_id, "main");
    let agent = write_transcript_at(
        root.path()
            .join(CLI_PROJECT_DIR)
            .join(session_id)
            .join("subagents")
            .join("agent-x.jsonl"),
        "agent",
    );
    hooks::post_claude_hook(
        &repo,
        card_id,
        hooks::session_start(session_id, DOTTED_CWD, &main),
    )
    .await;
    hooks::post_claude_hook(
        &repo,
        card_id,
        json!({
            "hook_event_name": "SubagentStop",
            "session_id": session_id,
            "cwd": DOTTED_CWD,
            "transcript_path": main,
            "agent_id": "agent-x",
            "agent_transcript_path": agent,
            "stop_hook_active": false
        }),
    )
    .await;

    let (token, handle) = hooks::spawn_claude_source(repo.clone(), seed.runtime.clone(), &seed);
    wait_for_user_text(&repo, card_id, "main").await;
    token.cancel();
    handle.await.unwrap().unwrap();

    assert_eq!(user_texts(&repo, card_id).await, vec!["main"]);
}

#[tokio::test]
async fn claude_transcript_source_skips_relative_transcript_path() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-claude-relative-path";
    let session_id = "session-claude-relative-path";
    let seed = wf::seed_claude_card_and_runtime(&repo, card_id, session_id, DOTTED_CWD).await;
    let root = tempfile::tempdir().unwrap();
    let path = transcript_file(root.path(), CLI_PROJECT_DIR, session_id, "absolute");
    hooks::post_claude_hook(
        &repo,
        card_id,
        hooks::session_start(session_id, DOTTED_CWD, Path::new("relative/t.jsonl")),
    )
    .await;

    let (token, handle) = hooks::spawn_claude_source(repo.clone(), seed.runtime.clone(), &seed);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!handle.is_finished(), "a relative path must not resolve");
    assert_eq!(item_count(&repo, card_id).await, 0);

    // Were the relative path kept, this absolute one would be a conflict and end the source.
    hooks::post_claude_hook(
        &repo,
        card_id,
        hooks::session_start(session_id, DOTTED_CWD, &path),
    )
    .await;
    wait_for_user_text(&repo, card_id, "absolute").await;
    token.cancel();
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn claude_transcript_source_does_not_tail_a_non_regular_transcript_path() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-claude-directory-path";
    let session_id = "session-claude-directory-path";
    let seed = wf::seed_claude_card_and_runtime(&repo, card_id, session_id, DOTTED_CWD).await;
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join(CLI_PROJECT_DIR);
    std::fs::create_dir_all(&directory).unwrap();
    hooks::post_claude_hook(
        &repo,
        card_id,
        hooks::session_start(session_id, DOTTED_CWD, &directory),
    )
    .await;

    let (token, handle) = hooks::spawn_claude_source(repo.clone(), seed.runtime.clone(), &seed);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !handle.is_finished(),
        "a non-regular path must keep the source waiting"
    );
    token.cancel();
    tokio::time::timeout(wf::LIVENESS_BUDGET, handle)
        .await
        .expect("cancelled source must end")
        .unwrap()
        .unwrap();

    assert_eq!(item_count(&repo, card_id).await, 0);
}

fn transcript_file(root: &Path, dir: &str, session_id: &str, text: &str) -> PathBuf {
    write_transcript_at(root.join(dir).join(format!("{session_id}.jsonl")), text)
}

fn write_transcript_at(path: PathBuf, text: &str) -> PathBuf {
    wf::write_transcript(
        &path,
        &[
            json!({
                "type": "permission-mode",
                "uuid": format!("perm-{text}"),
                "timestamp": "2026-06-13T00:00:00Z"
            }),
            wf::claude_system(&format!("sys-{text}"), DOTTED_CWD),
            wf::claude_user_string(&format!("user-{text}"), text),
        ],
    );
    path
}

async fn wait_for_user_text(repo: &Arc<SqlxRepo>, card_id: &str, text: &str) {
    wf::wait_until(wf::LIVENESS_BUDGET, || {
        let repo = repo.clone();
        async move {
            user_texts(&repo, card_id)
                .await
                .iter()
                .any(|seen| seen == text)
        }
    })
    .await;
}

async fn user_texts(repo: &SqlxRepo, card_id: &str) -> Vec<String> {
    repo.worker_flow_item_list_by_card(card_id, 0, 100, false)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|row| match serde_json::from_str(&row.payload).ok()? {
            WorkerFlowItem::UserMessage { content, .. } => match content.as_slice() {
                [MessageBlock::Text { text }] => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

async fn item_count(repo: &SqlxRepo, card_id: &str) -> usize {
    repo.worker_flow_item_list_by_card(card_id, 0, 100, false)
        .await
        .unwrap()
        .len()
}
