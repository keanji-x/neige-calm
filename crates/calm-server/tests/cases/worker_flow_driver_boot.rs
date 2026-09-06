use crate::support;

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
use calm_server::session_projection_repo::{AgentProvider, ThreadAttribution, WorkerSessionState};
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

    wf::wait_until(wf::LIVENESS_BUDGET, || {
        let driver = driver.clone();
        async move { driver.tasks_alive_for_test().await == 2 }
    })
    .await;
}

#[tokio::test]
async fn plain_chat_is_excluded_at_all_worker_flow_attach_entries() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let seed = wf::seed_card_and_runtime(
        &repo,
        "card-plain-chat-worker-flow",
        Some("thread-plain-chat-worker-flow"),
    )
    .await;
    let mut tx = repo.pool().begin().await.unwrap();
    let card = card_update_tx(
        &mut tx,
        seed.card.id.as_str(),
        CardPatch {
            payload: Some(json!({"schemaVersion": 1, "harness_profile": "plain_chat"})),
            ..CardPatch::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let state = wf::app_state(repo.clone(), events.clone());
    state.worker_flow.start_on_boot().await.unwrap();
    assert_eq!(state.worker_flow.tasks_alive_for_test().await, 0, "boot");

    let entries = [
        Event::WorkerSessionStarted {
            worker_session_id: seed.runtime.id.clone(),
            card_id: seed.runtime.card_id.clone(),
            kind: seed.runtime.kind.clone(),
            agent_provider: seed.runtime.agent_provider.clone(),
            status: WorkerSessionState::Running,
        },
        Event::WorkerSessionStatusChanged {
            worker_session_id: seed.runtime.id.clone(),
            card_id: seed.runtime.card_id.clone(),
            old_status: WorkerSessionState::Starting,
            new_status: WorkerSessionState::Running,
        },
        Event::CardAdded(card),
        Event::WorkerSessionSuperseded {
            old_worker_session_id: "old-plain-chat-runtime".into(),
            new_worker_session_id: seed.runtime.id.clone(),
            card_id: seed.runtime.card_id.clone(),
        },
    ];
    for (index, event) in entries.into_iter().enumerate() {
        events.emit(ActorId::Kernel, event);
        wf::wait_until(wf::LIVENESS_BUDGET, || {
            let driver = state.worker_flow.clone();
            async move { driver.events_handled_for_test() > index as u64 }
        })
        .await;
        assert_eq!(
            state.worker_flow.tasks_alive_for_test().await,
            0,
            "plain chat event entry {index}"
        );
    }
    let items = repo
        .worker_flow_item_list_by_card(seed.card.id.as_str(), 0, 100, false)
        .await
        .unwrap();
    assert!(
        items.is_empty(),
        "PlainChat must not persist worker-flow items"
    );
}

#[tokio::test]
async fn worker_flow_driver_attaches_when_thread_arrives_on_running_status() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let seed = wf::seed_card_and_runtime_with_status(
        &repo,
        "card-status-attach",
        None,
        WorkerSessionState::Starting,
    )
    .await;

    let state = wf::app_state(repo.clone(), events.clone());
    state.worker_flow.start_on_boot().await.unwrap();
    events.emit(
        ActorId::Kernel,
        Event::WorkerSessionStarted {
            worker_session_id: seed.runtime.id.clone(),
            card_id: seed.runtime.card_id.clone(),
            kind: seed.runtime.kind.clone(),
            agent_provider: seed.runtime.agent_provider.clone(),
            status: WorkerSessionState::Starting,
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
            worker_session_id: seed.runtime.id.clone(),
            provider: AgentProvider::Codex,
            thread_id: Some(thread_id.to_string()),
            session_id: Some(format!("sess-{thread_id}")),
            active_turn_id: None,
        },
    )
    .await
    .unwrap();
    session_set_status_tx(&mut tx, &seed.runtime.id, WorkerSessionState::Running)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    events.emit(
        ActorId::Kernel,
        Event::WorkerSessionStatusChanged {
            worker_session_id: seed.runtime.id.clone(),
            card_id: seed.runtime.card_id.clone(),
            old_status: WorkerSessionState::Starting,
            new_status: WorkerSessionState::Running,
        },
    );
    wf::wait_until(wf::LIVENESS_BUDGET, || {
        let driver = state.worker_flow.clone();
        async move { driver.tasks_alive_for_test().await == 1 }
    })
    .await;

    events.emit(
        ActorId::Kernel,
        Event::WorkerSessionStatusChanged {
            worker_session_id: seed.runtime.id.clone(),
            card_id: seed.runtime.card_id.clone(),
            old_status: WorkerSessionState::Running,
            new_status: WorkerSessionState::TurnPending,
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
            title: None,
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

    let runtime = wf::seed_claude_runtime_for_card_with_status(
        &repo,
        &card,
        session_id,
        WorkerSessionState::Running,
    )
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

    wf::wait_until(wf::LIVENESS_BUDGET, || {
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

#[tokio::test]
async fn isolated_missing_or_malformed_receipt_stops_tail_without_shared_fallback() {
    use calm_server::isolated_codex::{OPERATION_KIND, WorkerPayload, WorkerVersion};
    use calm_server::operation::{OperationKey, OperationRepo, SqlxOperationRepo, TxOutput};

    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let seed = wf::seed_card_and_runtime(
        &repo,
        "card-isolated-flow-no-fallback",
        Some("thread-isolated-flow-no-fallback"),
    )
    .await;
    let shared = SharedCodexAppServer::new_stub(repo.clone());
    let path = wf::rollout_path(
        shared.codex_home_path(),
        seed.runtime.thread_id.as_ref().unwrap(),
    );
    wf::write_rollout(
        &path,
        &[
            wf::session_meta(seed.runtime.thread_id.as_ref().unwrap()),
            wf::user_message("before-isolated", "existing legacy tail positive control"),
        ],
    );
    let driver = WorkerFlowDriver::new_with_flow_options_for_test(
        repo.clone(),
        shared,
        Arc::new(WorkerFlowSink::new(repo.clone())),
        EventBus::new(),
        CodexRolloutFlowSourceOptions {
            path_override: None,
            poll_interval: Duration::from_millis(20),
            lazy_retry_delay: Duration::from_millis(10),
            lazy_retry_attempts: 1,
            cursor_persist_every: 1,
        },
    );
    driver
        .attach_runtime_for_test(seed.runtime.clone())
        .await
        .unwrap();
    wf::wait_until(wf::LIVENESS_BUDGET, || {
        let repo = repo.clone();
        let card = seed.card.id.to_string();
        async move { item_count(&repo, &card).await == 1 }
    })
    .await;
    let old_stop = driver.task_stop_tokens_for_test().await.pop().unwrap();
    assert!(!old_stop.is_cancelled());

    // Persist the actual Operation kind/target and canonical session spawn binding.
    // The absent/invalid receipt is the defect input; no lookup implementation is copied.
    let task_id = "task-isolated-flow-no-fallback";
    let payload = serde_json::to_value(WorkerPayload {
        version: WorkerVersion::V1,
        actor: ActorId::KernelDispatcher,
        track_id: seed.card.track_id.to_string(),
        task_id: task_id.into(),
        idempotency_key: task_id.into(),
    })
    .unwrap();
    let operation = SqlxOperationRepo::new(repo.pool().clone())
        .insert_operation(
            OPERATION_KIND,
            OperationKey {
                operation_key: format!("{OPERATION_KIND}:{task_id}"),
                idempotency_key: Some(task_id.into()),
                payload_hash: calm_server::routes::terminal_cards::stable_payload_hash(&payload)
                    .unwrap(),
            },
            payload,
        )
        .await
        .unwrap();
    let mut tx = repo.pool().begin().await.unwrap();
    assert_eq!(
        sqlx::query(
            "UPDATE operations SET target_type='card',target_id=?1,target_json=?2 WHERE id=?3"
        )
        .bind(seed.card.id.as_str())
        .bind(json!({"type":"card","id":seed.card.id}).to_string())
        .bind(&operation)
        .execute(&mut *tx)
        .await
        .unwrap()
        .rows_affected(),
        1
    );
    assert_eq!(
        sqlx::query("UPDATE worker_sessions SET spawn_op_id=?1 WHERE id=?2 AND card_id=?3")
            .bind(&operation)
            .bind(&seed.runtime.id)
            .bind(seed.card.id.as_str())
            .execute(&mut *tx)
            .await
            .unwrap()
            .rows_affected(),
        1
    );
    tx.commit().await.unwrap();

    let missing = driver.attach_runtime_for_test(seed.runtime.clone()).await;
    let stopped_after_missing = old_stop.is_cancelled();
    let mut invalid = TxOutput::new("card", Some(seed.card.id.to_string()), json!(null));
    invalid.data = json!({"isolated_execution":"malformed private receipt"});
    assert_eq!(
        sqlx::query("UPDATE operations SET tx_output_json=?1 WHERE id=?2")
            .bind(serde_json::to_string(&invalid).unwrap())
            .bind(&operation)
            .execute(repo.pool())
            .await
            .unwrap()
            .rows_affected(),
        1
    );
    let malformed = driver.attach_runtime_for_test(seed.runtime.clone()).await;

    const SHARED_MARKER: &str = "ISOLATED_MUST_NEVER_INGEST_THIS_SHARED_HOME_MARKER";
    wf::append_rollout(&path, &[wf::user_message("after-isolated", SHARED_MARKER)]);
    // Negative observation over several existing tail polls; cancellation is also
    // asserted directly so this does not rely only on absence within a time window.
    tokio::time::sleep(Duration::from_millis(150)).await;
    let items = repo
        .worker_flow_item_list_by_card(seed.card.id.as_str(), 0, 100, false)
        .await
        .unwrap();
    assert!(
        missing
            .as_ref()
            .is_err_and(|error| error.to_string().contains("isolated preparation receipt")),
        "missing receipt must fail through the authoritative lookup: {missing:?}"
    );
    assert!(
        stopped_after_missing,
        "failed lookup must cancel the existing tail before dedup returns"
    );
    assert!(
        malformed.is_err(),
        "malformed isolated receipt must not select shared home"
    );
    assert_eq!(driver.tasks_alive_for_test().await, 0);
    assert!(
        !items
            .iter()
            .any(|item| item.payload.contains(SHARED_MARKER))
    );
    assert_eq!(
        items.len(),
        1,
        "only the earlier legacy positive-control item remains"
    );
}
