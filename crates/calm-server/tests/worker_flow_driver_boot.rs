mod support;

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::{
    SqlxRepo, card_update_tx, session_bind_attribution_tx, session_set_status_tx,
    terminal_create_tx,
};
use calm_server::event::{Event, EventBus};
use calm_server::ids::ActorId;
use calm_server::model::{CardPatch, NewTerminal, RequestTheme};
use calm_server::runtime_repo::{AgentProvider, RunStatus, ThreadAttribution};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::worker_flow::WorkerFlowDriver;
use calm_server::worker_flow::claude_transcript::ClaudeTranscriptFlowSourceOptions;
use calm_server::worker_flow::claude_transcript::slug_for_projects;
use calm_server::worker_flow::codex_rollout::CodexRolloutFlowSourceOptions;
use calm_truth::worker_flow_sink::WorkerFlowSink;
use serde_json::json;

use support::worker_flow as wf;

#[tokio::test]
async fn worker_flow_driver_boot_enumerates_active_codex_and_claude_runtimes() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    wf::seed_card_and_runtime(&repo, "card-driver-live", Some("thread-driver-live")).await;
    wf::seed_card_and_runtime(&repo, "card-driver-no-thread", None).await;
    wf::seed_claude_card_and_runtime(
        &repo,
        "card-driver-claude-live",
        "session-driver-claude-live",
        "/tmp/driver-claude",
    )
    .await;

    let codex_home = tempfile::tempdir().unwrap();
    let codex_path = wf::rollout_path(codex_home.path(), "thread-driver-live");
    wf::write_rollout(&codex_path, &[wf::session_meta("thread-driver-live")]);
    let transcript_dir = tempfile::tempdir().unwrap();
    let transcript_path = transcript_dir
        .path()
        .join("session-driver-claude-live.jsonl");
    wf::write_transcript(
        &transcript_path,
        &[wf::claude_system("sys-driver", "/tmp/driver-claude")],
    );

    let driver = WorkerFlowDriver::new_with_source_options_for_test(
        repo.clone(),
        SharedCodexAppServer::new_stub(repo.clone()),
        Arc::new(WorkerFlowSink::new(repo)),
        EventBus::new(),
        CodexRolloutFlowSourceOptions {
            path_override: Some(codex_path),
            poll_interval: Duration::from_millis(20),
            lazy_retry_delay: Duration::from_millis(10),
            lazy_retry_attempts: 3,
            cursor_persist_every: 1,
        },
        ClaudeTranscriptFlowSourceOptions {
            path_override: Some(transcript_path),
            poll_interval: Duration::from_millis(20),
            lazy_retry_delay: Duration::from_millis(10),
            lazy_retry_attempts: 3,
            cursor_persist_every: 1,
        },
    );
    driver.start_on_boot().await.unwrap();

    wf::wait_until(Duration::from_secs(1), || {
        let driver = driver.clone();
        async move { driver.tasks_alive_for_test().await == 2 }
    })
    .await;
}

#[tokio::test]
async fn worker_flow_driver_attaches_when_thread_arrives_on_running_status() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let seed = wf::seed_card_and_runtime_with_status(
        &repo,
        "card-status-attach",
        None,
        RunStatus::Starting,
    )
    .await;

    let state = wf::app_state(repo.clone(), events.clone());
    state.worker_flow.start_on_boot().await.unwrap();
    events.emit(
        ActorId::Kernel,
        Event::RuntimeStarted {
            runtime_id: seed.runtime.id.clone(),
            card_id: seed.runtime.card_id.clone(),
            kind: seed.runtime.kind.clone(),
            agent_provider: seed.runtime.agent_provider.clone(),
            status: RunStatus::Starting,
        },
    );
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(state.worker_flow.tasks_alive_for_test().await, 0);

    let thread_id = "thread-status-attach";
    let path = wf::rollout_path(state.shared_codex_appserver.codex_home_path(), thread_id);
    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::user_message("u-status", "attached after thread bind"),
        ],
    );
    let mut tx = repo.pool().begin().await.unwrap();
    session_bind_attribution_tx(
        &mut tx,
        &seed.runtime.id,
        ThreadAttribution {
            runtime_id: seed.runtime.id.clone(),
            provider: AgentProvider::Codex,
            thread_id: Some(thread_id.to_string()),
            session_id: Some(format!("sess-{thread_id}")),
            active_turn_id: None,
        },
    )
    .await
    .unwrap();
    session_set_status_tx(&mut tx, &seed.runtime.id, RunStatus::Running)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    events.emit(
        ActorId::Kernel,
        Event::RuntimeStatusChanged {
            runtime_id: seed.runtime.id.clone(),
            card_id: seed.runtime.card_id.clone(),
            old_status: RunStatus::Starting,
            new_status: RunStatus::Running,
        },
    );
    wf::wait_until(Duration::from_secs(1), || {
        let driver = state.worker_flow.clone();
        async move { driver.tasks_alive_for_test().await == 1 }
    })
    .await;

    events.emit(
        ActorId::Kernel,
        Event::RuntimeStatusChanged {
            runtime_id: seed.runtime.id.clone(),
            card_id: seed.runtime.card_id.clone(),
            old_status: RunStatus::Running,
            new_status: RunStatus::TurnPending,
        },
    );
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(state.worker_flow.tasks_alive_for_test().await, 1);
}

#[tokio::test]
async fn worker_flow_driver_uses_terminal_row_cwd_for_legacy_claude_card() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-driver-legacy-claude-cwd";
    let session_id = "session-driver-legacy-claude-cwd";
    let terminal_cwd = "/path/from/terminal";
    let card = wf::seed_claude_card(&repo, card_id, "/server/default").await;

    let mut tx = repo.pool().begin().await.unwrap();
    let term = terminal_create_tx(
        &mut tx,
        NewTerminal {
            card_id: card.id.clone(),
            program: "claude".into(),
            cwd: terminal_cwd.into(),
            env: json!({}),
            theme: RequestTheme::default_dark(),
        },
    )
    .await
    .unwrap();
    let card = card_update_tx(
        &mut tx,
        card.id.as_ref(),
        CardPatch {
            payload: Some(json!({
                "schemaVersion": 1,
                "terminal_id": term.id,
                "claude_session_id": session_id
            })),
            ..CardPatch::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let runtime =
        wf::seed_claude_runtime_for_card_with_status(&repo, &card, session_id, RunStatus::Running)
            .await;
    let transcript_root = tempfile::tempdir().unwrap();
    let expected_path = transcript_path(
        transcript_root.path(),
        &slug_for_projects(terminal_cwd),
        session_id,
    );
    let stale_card_cwd_path = transcript_path(
        transcript_root.path(),
        &slug_for_projects("/server/default"),
        session_id,
    );
    assert_ne!(expected_path, stale_card_cwd_path);
    wf::write_transcript(
        &expected_path,
        &[wf::claude_system("sys-driver-legacy-cwd", terminal_cwd)],
    );

    let driver = WorkerFlowDriver::new_with_source_options_for_test(
        repo.clone(),
        SharedCodexAppServer::new_stub(repo.clone()),
        Arc::new(WorkerFlowSink::new(repo.clone())),
        EventBus::new(),
        CodexRolloutFlowSourceOptions {
            path_override: None,
            poll_interval: Duration::from_millis(20),
            lazy_retry_delay: Duration::from_millis(10),
            lazy_retry_attempts: 1,
            cursor_persist_every: 1,
        },
        ClaudeTranscriptFlowSourceOptions {
            path_override: Some(expected_path.clone()),
            poll_interval: Duration::from_millis(20),
            lazy_retry_delay: Duration::from_millis(10),
            lazy_retry_attempts: 1,
            cursor_persist_every: 1,
        },
    );
    driver.attach_runtime_for_test(runtime).await.unwrap();

    wf::wait_until(Duration::from_secs(1), || {
        let repo = repo.clone();
        async move { item_count(&repo, card_id).await == 1 }
    })
    .await;
    for stop in driver.task_stop_tokens_for_test().await {
        stop.cancel();
    }

    assert_eq!(
        expected_path,
        transcript_path(
            transcript_root.path(),
            &slug_for_projects(terminal_cwd),
            session_id
        )
    );
    assert_eq!(item_count(&repo, card_id).await, 1);
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
