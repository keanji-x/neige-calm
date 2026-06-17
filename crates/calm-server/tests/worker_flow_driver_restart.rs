mod support;

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::sqlite::{SqlxRepo, session_set_status_tx, session_start_runtime_tx};
use calm_server::event::{Event, EventBus};
use calm_server::ids::ActorId;
use calm_server::model::now_ms;
use calm_server::runtime_repo::{AgentProvider, RunStatus, RuntimeInit, RuntimeKind};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::worker_flow::WorkerFlowDriver;
use calm_server::worker_flow::claude_transcript::ClaudeTranscriptFlowSourceOptions;
use calm_server::worker_flow::codex_rollout::CodexRolloutFlowSourceOptions;
use calm_truth::worker_flow_sink::WorkerFlowSink;

use support::worker_flow as wf;

#[tokio::test]
async fn worker_flow_driver_replaces_stale_claude_tail_task_when_runtime_id_changes() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let seed = wf::seed_claude_card_and_runtime(
        &repo,
        "card-driver-claude-restart",
        "session-driver-claude-restart-a",
        "/tmp/driver-claude-restart",
    )
    .await;

    let transcript_dir = tempfile::tempdir().unwrap();
    let transcript_path = transcript_dir
        .path()
        .join("session-driver-claude-restart.jsonl");
    wf::write_transcript(
        &transcript_path,
        &[wf::claude_system(
            "sys-driver-restart",
            "/tmp/driver-claude-restart",
        )],
    );

    let driver = WorkerFlowDriver::new_with_source_options_for_test(
        repo.clone(),
        SharedCodexAppServer::new_stub(repo.clone()),
        Arc::new(WorkerFlowSink::new(repo.clone())),
        events.clone(),
        CodexRolloutFlowSourceOptions::default(),
        ClaudeTranscriptFlowSourceOptions {
            path_override: Some(transcript_path),
            poll_interval: Duration::from_secs(5),
            lazy_retry_delay: Duration::from_millis(10),
            lazy_retry_attempts: 3,
            cursor_persist_every: 1,
        },
    );
    driver.start_on_boot().await.unwrap();

    wf::wait_until(Duration::from_secs(1), || {
        let driver = driver.clone();
        async move { driver.tasks_alive_for_test().await == 1 }
    })
    .await;
    assert_eq!(
        driver.task_runtime_ids_for_test().await,
        vec![seed.runtime.id.clone()]
    );
    let mut old_stops = driver.task_stop_tokens_for_test().await;
    assert_eq!(old_stops.len(), 1);
    let old_stop = old_stops.pop().unwrap();

    let replacement = {
        let mut tx = repo.pool().begin().await.unwrap();
        session_set_status_tx(&mut tx, &seed.runtime.id, RunStatus::Exited)
            .await
            .unwrap();
        let replacement = session_start_runtime_tx(
            &mut tx,
            RuntimeInit {
                id: "rt-card-driver-claude-restart-b".to_string(),
                card_id: seed.runtime.card_id.clone(),
                kind: RuntimeKind::ClaudeCard,
                agent_provider: Some(AgentProvider::Claude),
                status: RunStatus::Starting,
                terminal_run_id: None,
                thread_id: None,
                session_id: Some("session-driver-claude-restart-b".to_string()),
                active_turn_id: None,
                handle_state_json: None,
                lease_owner: None,
                lease_until_ms: None,
                spawn_op_id: None,
                now_ms: now_ms(),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        replacement
    };

    tokio::time::sleep(Duration::from_millis(20)).await;
    events.emit(
        ActorId::Kernel,
        Event::RuntimeStarted {
            runtime_id: replacement.id.clone(),
            card_id: replacement.card_id.clone(),
            kind: replacement.kind.clone(),
            agent_provider: replacement.agent_provider.clone(),
            status: replacement.status.clone(),
        },
    );

    wf::wait_until(Duration::from_millis(200), || {
        let driver = driver.clone();
        let old_stop = old_stop.clone();
        let replacement_id = replacement.id.clone();
        async move {
            old_stop.is_cancelled()
                && driver.tasks_alive_for_test().await == 1
                && driver.task_runtime_ids_for_test().await == vec![replacement_id]
        }
    })
    .await;

    let mut replacement_stops = driver.task_stop_tokens_for_test().await;
    assert_eq!(replacement_stops.len(), 1);
    let replacement_stop = replacement_stops.pop().unwrap();

    events.emit(
        ActorId::Kernel,
        Event::RuntimeStarted {
            runtime_id: replacement.id.clone(),
            card_id: replacement.card_id.clone(),
            kind: replacement.kind.clone(),
            agent_provider: replacement.agent_provider.clone(),
            status: replacement.status.clone(),
        },
    );
    tokio::time::sleep(Duration::from_millis(60)).await;

    assert_eq!(driver.tasks_alive_for_test().await, 1);
    assert_eq!(
        driver.task_runtime_ids_for_test().await,
        vec![replacement.id.clone()]
    );
    assert!(!replacement_stop.is_cancelled());
}
