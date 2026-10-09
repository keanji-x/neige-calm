use std::sync::Arc;
use std::time::Duration;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::{
    SqlxRepo, session_set_status_tx, session_start_runtime_tx, session_supersede_and_start_tx,
};
use calm_server::event::{Event, EventBus};
use calm_server::ids::ActorId;
use calm_server::model::now_ms;
use calm_server::session_projection_repo::{WorkerSessionInit, WorkerSessionState};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::test_seams::WorkerFlowPoint;
use calm_server::worker_flow::WorkerFlowDriver;
use calm_server::worker_flow::claude_transcript::ClaudeTranscriptFlowSourceOptions;
use calm_server::worker_flow::codex_rollout::CodexRolloutFlowSourceOptions;
use calm_truth::capture_test_seam::{CapturePoint, install};
use calm_truth::worker_flow_sink::WorkerFlowSink;

use crate::support::worker_flow as wf;

async fn terminal_identity(
    claude: bool,
    status: WorkerSessionState,
    superseded_event: bool,
    card: &str,
) {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let seed = if claude {
        wf::seed_claude_card_and_runtime(&repo, card, "identity-session", "/tmp").await
    } else {
        wf::seed_card_and_runtime(&repo, card, Some("identity-thread")).await
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture.jsonl");
    wf::write_rollout(
        &path,
        &[if claude {
            wf::claude_system("init", "/tmp")
        } else {
            wf::session_meta("identity-thread")
        }],
    );
    let events = EventBus::new();
    let driver = WorkerFlowDriver::new_with_source_options_for_test(
        repo.clone(),
        SharedCodexAppServer::new_stub(repo.clone()),
        Arc::new(WorkerFlowSink::new(repo.clone())),
        events.clone(),
        CodexRolloutFlowSourceOptions {
            path_override: Some(path.clone()),
            poll_interval: Duration::from_millis(20),
            ..Default::default()
        },
        ClaudeTranscriptFlowSourceOptions {
            path_override: Some(path.clone()),
            poll_interval: Duration::from_millis(20),
            ..Default::default()
        },
    );
    let idle = install(card, 1, WorkerFlowPoint::Idle);
    driver.start_on_boot().await.unwrap();
    tokio::time::timeout(wf::LIVENESS_BUDGET, idle.entered.notified())
        .await
        .unwrap();
    let old_stop = driver.task_stop_tokens_for_test().await.pop().unwrap();
    let replacement = {
        let mut tx = repo.pool().begin().await.unwrap();
        let init = WorkerSessionInit {
            id: format!("replacement-{card}"),
            card_id: card.into(),
            kind: seed.runtime.kind.clone(),
            agent_provider: seed.runtime.agent_provider.clone(),
            status: WorkerSessionState::Running,
            terminal_run_id: None,
            thread_id: seed.runtime.thread_id.clone(),
            session_id: seed.runtime.session_id.clone(),
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        };
        let runtime = if status == WorkerSessionState::Superseded {
            session_supersede_and_start_tx(&mut tx, &seed.runtime.id, init)
                .await
                .unwrap()
        } else {
            session_set_status_tx(&mut tx, &seed.runtime.id, status)
                .await
                .unwrap();
            session_start_runtime_tx(&mut tx, init).await.unwrap()
        };
        tx.commit().await.unwrap();
        runtime
    };
    events.emit(
        ActorId::Kernel,
        Event::WorkerSessionStarted {
            worker_session_id: replacement.id.clone(),
            card_id: card.into(),
            kind: replacement.kind.clone(),
            agent_provider: replacement.agent_provider.clone(),
            status: replacement.status,
        },
    );
    tokio::time::timeout(wf::LIVENESS_BUDGET, old_stop.cancelled())
        .await
        .unwrap();
    idle.release.notify_one();
    wf::wait_until(wf::LIVENESS_BUDGET, || async {
        driver.events_handled_for_test() == 1
    })
    .await;
    assert!(old_stop.is_cancelled());
    assert_eq!(driver.tasks_alive_for_test().await, 1);
    let replacement_stop = driver.task_stop_tokens_for_test().await.pop().unwrap();
    // Event completion is the barrier: append only after the stale event has been handled.
    events.emit(
        ActorId::Kernel,
        if superseded_event {
            Event::WorkerSessionSuperseded {
                old_worker_session_id: seed.runtime.id.clone(),
                new_worker_session_id: replacement.id.clone(),
                card_id: card.into(),
            }
        } else {
            Event::WorkerSessionStatusChanged {
                worker_session_id: seed.runtime.id.clone(),
                card_id: card.into(),
                old_status: WorkerSessionState::Running,
                new_status: status,
            }
        },
    );
    wf::wait_until(wf::LIVENESS_BUDGET, || async {
        driver.events_handled_for_test() == 2
    })
    .await;
    wf::append_rollout(
        &path,
        &[if claude {
            wf::claude_user_string("after-stale", "replacement capture continues")
        } else {
            wf::user_message("after-stale", "replacement capture continues")
        }],
    );
    wf::wait_until(wf::LIVENESS_BUDGET, || async {
        replacement_stop.is_cancelled()
            || wf::cat_conversation(&repo, &seed.card)
                .await
                .contains("replacement capture continues")
    })
    .await;
    assert!(
        wf::cat_conversation(&repo, &seed.card)
            .await
            .contains("replacement capture continues"),
        "stale event must preserve actual replacement transcript capture"
    );
    assert!(
        !replacement_stop.is_cancelled(),
        "stale event must preserve task identity"
    );
    let rows = repo
        .worker_flow_item_list_by_card(card, 0, 100, false)
        .await
        .unwrap();
    let captured = rows
        .iter()
        .find(|r| r.payload.contains("replacement capture continues"))
        .unwrap();
    assert_eq!(
        captured.worker_session_id.as_deref(),
        Some(replacement.id.as_str())
    );
    assert_eq!(
        captured.captured_session_id.as_deref(),
        Some(replacement.id.as_str())
    );

    // Pause an actual committed capture before acknowledgement; current-session cancellation
    // must await the owned source, not merely remove the task from the map.
    let committed = install(card, 3, CapturePoint::Committed);
    wf::append_rollout(
        &path,
        &[if claude {
            wf::claude_user_string("settle", "capture settled")
        } else {
            wf::user_message("settle", "capture settled")
        }],
    );
    tokio::time::timeout(wf::LIVENESS_BUDGET, committed.entered.notified())
        .await
        .unwrap();
    events.emit(
        ActorId::Kernel,
        Event::WorkerSessionStatusChanged {
            worker_session_id: replacement.id.clone(),
            card_id: card.into(),
            old_status: WorkerSessionState::Running,
            new_status: status,
        },
    );
    wf::wait_until(wf::LIVENESS_BUDGET, || async {
        replacement_stop.is_cancelled()
    })
    .await;
    assert_eq!(
        driver.events_handled_for_test(),
        2,
        "terminal handling awaits capture settlement"
    );
    committed.release.notify_one();
    wf::wait_until(wf::LIVENESS_BUDGET, || async {
        driver.events_handled_for_test() == 3
    })
    .await;
    assert!(driver.task_stop_tokens_for_test().await.is_empty());
    assert_eq!(driver.tasks_alive_for_test().await, 0);
    assert!(
        wf::cat_conversation(&repo, &seed.card)
            .await
            .contains("capture settled")
    );
}

#[tokio::test]
async fn codex_exited_preserves_replacement_capture_and_settles_current() {
    terminal_identity(
        false,
        WorkerSessionState::Exited,
        false,
        "identity-codex-exited",
    )
    .await;
}

#[tokio::test]
async fn codex_failed_preserves_replacement_capture_and_settles_current() {
    terminal_identity(
        false,
        WorkerSessionState::Failed,
        false,
        "identity-codex-failed",
    )
    .await;
}

#[tokio::test]
async fn codex_superseded_preserves_replacement_capture_and_settles_current() {
    terminal_identity(
        false,
        WorkerSessionState::Superseded,
        false,
        "identity-codex-superseded",
    )
    .await;
}

#[tokio::test]
async fn codex_superseded_event_preserves_replacement_capture_and_settles_current() {
    terminal_identity(
        false,
        WorkerSessionState::Superseded,
        true,
        "identity-codex-superseded_event",
    )
    .await;
}

#[tokio::test]
async fn claude_exited_preserves_replacement_capture_and_settles_current() {
    terminal_identity(
        true,
        WorkerSessionState::Exited,
        false,
        "identity-claude-exited",
    )
    .await;
}

#[tokio::test]
async fn claude_failed_preserves_replacement_capture_and_settles_current() {
    terminal_identity(
        true,
        WorkerSessionState::Failed,
        false,
        "identity-claude-failed",
    )
    .await;
}

#[tokio::test]
async fn claude_superseded_preserves_replacement_capture_and_settles_current() {
    terminal_identity(
        true,
        WorkerSessionState::Superseded,
        false,
        "identity-claude-superseded",
    )
    .await;
}

#[tokio::test]
async fn claude_superseded_event_preserves_replacement_capture_and_settles_current() {
    terminal_identity(
        true,
        WorkerSessionState::Superseded,
        true,
        "identity-claude-superseded_event",
    )
    .await;
}
