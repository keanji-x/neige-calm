mod support;

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventBus};
use calm_server::ids::ActorId;
use calm_server::runtime_repo::RunStatus;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::worker_flow::WorkerFlowDriver;
use calm_server::worker_flow::claude_transcript::ClaudeTranscriptFlowSourceOptions;
use calm_server::worker_flow::codex_rollout::CodexRolloutFlowSourceOptions;
use calm_truth::worker_flow_sink::WorkerFlowSink;

use support::worker_flow as wf;

#[tokio::test]
async fn claude_transcript_cwd_mismatch_exits_without_ingesting_wrong_file() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let seed = wf::seed_claude_card_and_runtime(
        &repo,
        "card-claude-wrong-cwd",
        "session-claude-wrong-cwd",
        "/tmp/claude-right",
    )
    .await;
    let transcript_dir = tempfile::tempdir().unwrap();
    let path = transcript_dir.path().join("session-claude-wrong-cwd.jsonl");
    wf::write_transcript(
        &path,
        &[
            wf::claude_system("sys-1", "/tmp/claude-wrong"),
            wf::claude_user_string("user-1", "wrong file"),
        ],
    );

    let driver = WorkerFlowDriver::new_with_source_options_for_test(
        repo.clone(),
        SharedCodexAppServer::new_stub(repo.clone()),
        Arc::new(WorkerFlowSink::new(repo.clone())),
        events.clone(),
        CodexRolloutFlowSourceOptions::default(),
        ClaudeTranscriptFlowSourceOptions {
            path_override: Some(path.clone()),
            poll_interval: Duration::from_millis(20),
            lazy_retry_delay: Duration::from_millis(10),
            lazy_retry_attempts: 3,
            cursor_persist_every: 1,
        },
    );
    driver.start_on_boot().await.unwrap();

    wf::wait_until(Duration::from_millis(500), || {
        let driver = driver.clone();
        async move { driver.tasks_alive_for_test().await == 0 }
    })
    .await;
    assert_eq!(item_count(&repo, "card-claude-wrong-cwd").await, 0);

    events.emit(
        ActorId::Kernel,
        Event::RuntimeStatusChanged {
            runtime_id: seed.runtime.id.clone(),
            card_id: seed.runtime.card_id.clone(),
            old_status: RunStatus::Idle,
            new_status: RunStatus::Running,
        },
    );
    wf::wait_until(Duration::from_millis(500), || {
        let driver = driver.clone();
        async move { driver.tasks_alive_for_test().await == 0 }
    })
    .await;
    assert_eq!(item_count(&repo, "card-claude-wrong-cwd").await, 0);
}

async fn item_count(repo: &SqlxRepo, card_id: &str) -> usize {
    repo.worker_flow_item_list_by_card(card_id, 0, 100, false)
        .await
        .unwrap()
        .len()
}
