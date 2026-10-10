//! #2516: a Claude `/clear` starts a new session id and a new transcript in the same runtime;
//! the recorder follows the id the card's runtime moved to, so capture goes on after the clear.

use crate::support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::worker_flow::WorkerFlowDriver;
use calm_server::worker_flow::claude_transcript::ClaudeTranscriptFlowSourceOptions;
use calm_server::worker_flow::codex_rollout::CodexRolloutFlowSourceOptions;
use calm_truth::worker_flow_sink::WorkerFlowSink;
use calm_types::worker_flow::{MessageBlock, WorkerFlowItem};
use serde_json::{Value, json};

use support::claude_hooks as hooks;
use support::worker_flow as wf;

const CWD: &str = "/x/repo";

fn transcript(root: &Path, session_id: &str, text: &str) -> PathBuf {
    let path = root.join(format!("{session_id}.jsonl"));
    wf::write_transcript(
        &path,
        &[
            wf::claude_system(&format!("sys-{text}"), CWD),
            wf::claude_user_string(&format!("user-{text}"), text),
        ],
    );
    path
}

fn session_start(session_id: &str, source: &str, path: &Path) -> Value {
    json!({
        "hook_event_name": "SessionStart",
        "session_id": session_id,
        "source": source,
        "cwd": CWD,
        "transcript_path": path,
    })
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

async fn wait_for_user_texts(repo: &Arc<SqlxRepo>, card_id: &str, expected: &[&str]) {
    wf::wait_until(wf::LIVENESS_BUDGET, || {
        let repo = repo.clone();
        async move { user_texts(&repo, card_id).await == expected }
    })
    .await;
}

#[tokio::test]
async fn claude_capture_follows_the_new_session_after_clear() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-claude-clear";
    let seed = wf::seed_claude_card_and_runtime(&repo, card_id, "session-before", CWD).await;
    let root = tempfile::tempdir().unwrap();
    let before = transcript(root.path(), "session-before", "before clear");
    let after = transcript(root.path(), "session-after", "after clear");

    let events = EventBus::new();
    let driver = WorkerFlowDriver::new_with_source_options_for_test(
        repo.clone(),
        SharedCodexAppServer::new_stub(repo.clone()),
        Arc::new(WorkerFlowSink::new(repo.clone())),
        events.clone(),
        CodexRolloutFlowSourceOptions {
            path_override: None,
            poll_interval: Duration::from_millis(20),
            lazy_retry_delay: Duration::from_millis(10),
            lazy_retry_attempts: 1,
        },
        ClaudeTranscriptFlowSourceOptions {
            path_override: None,
            poll_interval: Duration::from_millis(20),
            lazy_retry_delay: Duration::from_millis(10),
            lazy_retry_attempts: 1,
        },
    );
    driver.start_on_boot().await.unwrap();
    hooks::post_claude_hook_on(
        &repo,
        events.clone(),
        card_id,
        session_start("session-before", "startup", &before),
    )
    .await;
    wait_for_user_texts(&repo, card_id, &["before clear"]).await;

    hooks::post_claude_hook_on(
        &repo,
        events,
        card_id,
        session_start("session-after", "clear", &after),
    )
    .await;
    wait_for_user_texts(&repo, card_id, &["before clear", "after clear"]).await;

    assert_eq!(
        driver.task_runtime_ids_for_test().await,
        std::slice::from_ref(&seed.runtime.id),
        "the same runtime keeps one capture task"
    );
    for stop in driver.task_stop_tokens_for_test().await {
        stop.cancel();
    }
}
