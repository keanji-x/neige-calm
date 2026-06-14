mod support;

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::runtime_repo::{RunStatus, RuntimeRepo};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::worker_flow::WorkerFlowDriver;
use calm_server::worker_flow::claude_transcript::ClaudeTranscriptFlowSourceOptions;
use calm_server::worker_flow::codex_rollout::CodexRolloutFlowSourceOptions;
use calm_truth::worker_flow_sink::WorkerFlowSink;
use calm_types::worker_flow::{MessageBlock, WorkerFlowItem};

use support::worker_flow as wf;

#[tokio::test]
async fn claude_transcript_source_persists_past_lazy_retry_budget_until_file_appears() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-claude-lazy";
    let seed =
        wf::seed_claude_card_and_runtime(&repo, card_id, "session-claude-lazy", "/tmp/claude-lazy")
            .await;
    let transcript_dir = tempfile::tempdir().unwrap();
    let path = transcript_dir.path().join("session-claude-lazy.jsonl");
    let retry_delay = Duration::from_millis(10);
    let retry_attempts = 3;
    let poll_interval = Duration::from_millis(20);

    let driver = claude_driver_with_path(
        repo.clone(),
        path.clone(),
        poll_interval,
        retry_delay,
        retry_attempts,
    );
    driver
        .attach_runtime_for_test(seed.runtime.clone())
        .await
        .unwrap();

    tokio::time::sleep(retry_delay * retry_attempts as u32 + Duration::from_millis(200)).await;
    assert_eq!(driver.tasks_alive_for_test().await, 1);

    wf::write_transcript(
        &path,
        &[
            wf::claude_system("sys-lazy", "/tmp/claude-lazy"),
            wf::claude_user_string("user-lazy", "created later"),
        ],
    );

    wf::wait_until(Duration::from_millis(1_300), || {
        let repo = repo.clone();
        async move { user_message_seen(&repo, card_id, "created later").await }
    })
    .await;
}

#[tokio::test]
async fn claude_transcript_source_exits_during_lazy_retry_when_runtime_becomes_terminal() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-claude-lazy-terminal";
    let seed = wf::seed_claude_card_and_runtime(
        &repo,
        card_id,
        "session-claude-lazy-terminal",
        "/tmp/claude-lazy-terminal",
    )
    .await;
    let transcript_dir = tempfile::tempdir().unwrap();
    let path = transcript_dir
        .path()
        .join("session-claude-lazy-terminal.jsonl");
    let driver = claude_driver_with_path(
        repo.clone(),
        path,
        Duration::from_millis(20),
        Duration::from_millis(10),
        3,
    );
    driver
        .attach_runtime_for_test(seed.runtime.clone())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(driver.tasks_alive_for_test().await, 1);
    repo.runtime_set_status_for_card(card_id, RunStatus::Exited)
        .await
        .unwrap();

    wf::wait_until(Duration::from_secs(2), || {
        let driver = driver.clone();
        async move { driver.tasks_alive_for_test().await == 0 }
    })
    .await;
}

#[tokio::test]
async fn claude_transcript_source_drains_file_created_as_runtime_exits_during_lazy_retry() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-claude-lazy-race-terminal";
    let cwd = "/tmp/claude-lazy-race-terminal";
    let seed =
        wf::seed_claude_card_and_runtime(&repo, card_id, "session-claude-lazy-race-terminal", cwd)
            .await;
    let transcript_dir = tempfile::tempdir().unwrap();
    let path = transcript_dir
        .path()
        .join("session-claude-lazy-race-terminal.jsonl");
    let poll_interval = Duration::from_millis(20);
    let lazy_retry_delay = Duration::from_millis(100);
    let driver = claude_driver_with_path(
        repo.clone(),
        path.clone(),
        poll_interval,
        lazy_retry_delay,
        3,
    );
    driver
        .attach_runtime_for_test(seed.runtime.clone())
        .await
        .unwrap();

    wf::wait_until(Duration::from_secs(1), || {
        let driver = driver.clone();
        async move { driver.tasks_alive_for_test().await == 1 }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let write_path = path.clone();
    let exit_repo = repo.clone();
    let (_, _) = tokio::join!(
        async move {
            wf::write_transcript(
                &write_path,
                &[
                    wf::claude_user_string("user-lazy-race", "race user"),
                    wf::claude_assistant(
                        "assistant-lazy-race",
                        cwd,
                        vec![wf::claude_text("race assistant")],
                    ),
                ],
            );
        },
        async move {
            exit_repo
                .runtime_set_status_for_card(card_id, RunStatus::Exited)
                .await
                .unwrap();
        }
    );

    wf::wait_until(
        lazy_retry_delay + poll_interval * 2 + Duration::from_millis(500),
        || {
            let repo = repo.clone();
            let driver = driver.clone();
            async move {
                flow_item_count(&repo, card_id).await == 2
                    || driver.tasks_alive_for_test().await == 0
            }
        },
    )
    .await;
    assert_eq!(flow_item_count(&repo, card_id).await, 2);
    wf::wait_until(Duration::from_millis(500), || {
        let driver = driver.clone();
        async move { driver.tasks_alive_for_test().await == 0 }
    })
    .await;
}

fn claude_driver_with_path(
    repo: Arc<SqlxRepo>,
    path: std::path::PathBuf,
    poll_interval: Duration,
    lazy_retry_delay: Duration,
    lazy_retry_attempts: usize,
) -> Arc<WorkerFlowDriver> {
    WorkerFlowDriver::new_with_source_options_for_test(
        repo.clone(),
        SharedCodexAppServer::new_stub(repo.clone()),
        Arc::new(WorkerFlowSink::new(repo)),
        EventBus::new(),
        CodexRolloutFlowSourceOptions::default(),
        ClaudeTranscriptFlowSourceOptions {
            path_override: Some(path),
            poll_interval,
            lazy_retry_delay,
            lazy_retry_attempts,
            cursor_persist_every: 1,
        },
    )
}

async fn flow_item_count(repo: &SqlxRepo, card_id: &str) -> usize {
    repo.worker_flow_item_list_by_card(card_id, 0, 100, false)
        .await
        .unwrap()
        .len()
}

async fn user_message_seen(repo: &SqlxRepo, card_id: &str, expected_text: &str) -> bool {
    repo.worker_flow_item_list_by_card(card_id, 0, 100, false)
        .await
        .unwrap()
        .into_iter()
        .any(|row| {
            let Ok(item) = serde_json::from_str::<WorkerFlowItem>(&row.payload) else {
                return false;
            };
            matches!(
                item,
                WorkerFlowItem::UserMessage { content, .. }
                    if content == vec![MessageBlock::Text {
                        text: expected_text.to_string()
                    }]
            )
        })
}
