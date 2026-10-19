//! Accepted provider results must commit before ordered registry release.
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use calm_server::codex_appserver::InputItem;
use calm_server::db::prelude::*;
use calm_server::harness::{HarnessSnapshot, HarnessState};
use calm_server::shared_codex_appserver::TurnStartReturnHook;
use calm_server::test_seams::{PausePoint, install_pause_for_test};
use serde_json::json;
use tokio::sync::Notify;

use crate::planner_repoint_restart_lock::{joined, wait_until};
use crate::support::planner_queue_fixture::{
    Boot, Issuance, boot_with_issuance, idle_snapshot, post_input, send_json,
};

const TEXT: &str = "release commit must not deliver this sentence twice";

async fn stored_ids(boot: &Boot) -> Vec<String> {
    let row = boot
        .repo
        .session_projection_by_id(&boot.worker_session_id)
        .await
        .unwrap()
        .unwrap();
    HarnessSnapshot::from_value_strict(row.handle_state_json.unwrap())
        .pending_entries()
        .iter()
        .filter_map(|entry| entry.id().map(|id| id.as_str().to_owned()))
        .collect()
}

async fn enqueue(boot: &Boot, text: &str) -> String {
    let (status, body) = post_input(boot.app.clone(), boot.planner_card.id.as_str(), text).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["entry_id"].as_str().unwrap().to_owned()
}

fn delivery_count(boot: &Boot) -> usize {
    let starts = boot.daemon.started_turns_for_test();
    let steers = boot.daemon.steered_turns_for_test();
    starts
        .iter()
        .map(|(_, items)| items)
        .chain(steers.iter().map(|(_, _, items, _)| items))
        .flatten()
        .filter_map(|item| match item {
            InputItem::Text { text } => Some(text.matches(TEXT).count()),
            _ => None,
        })
        .sum()
}

async fn shutdown_during_return(
    boot: &Boot,
) -> tokio::task::JoinHandle<calm_server::error::Result<Option<calm_server::harness::PlannerHarness>>>
{
    let registry = boot.registry.clone();
    let runtime = boot.worker_session_id.clone();
    let shutdown = tokio::spawn(async move { registry.shutdown_and_remove(&runtime).await });
    wait_until("shutdown closing ingress", || {
        boot.harness.is_closing_for_test()
    })
    .await;
    assert!(
        !shutdown.is_finished(),
        "release must wait for the provider result"
    );
    assert!(boot.registry.get(&boot.worker_session_id).is_some());
    shutdown
}

async fn assert_no_redelivery_after_restart(boot: &Boot, delivered_id: &str) {
    let before_restart = stored_ids(boot).await;
    let starts_before = boot.daemon.turn_start_count_for_test();
    let (status, body) = send_json(
        boot.app.clone(),
        "POST",
        format!("/api/cards/{}/planner/restart", boot.planner_card.id),
        "user",
        json!(null),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "fresh start: {body}");
    let next = boot
        .repo
        .session_projection_active_for_card(&boot.planner_card.id.to_string())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(next.id, boot.worker_session_id);
    assert!(boot.registry.get(&next.id).is_some());
    // Send a distinct barrier through the successor's production durable ingress.
    // Its provider start proves any inherited pending ahead of it was observed.
    enqueue(boot, "restart barrier").await;
    wait_until("the successor provider start", || {
        boot.daemon.turn_start_count_for_test() > starts_before
    })
    .await;
    boot.registry.shutdown_and_remove(&next.id).await.unwrap();
    assert!(
        !before_restart.iter().any(|id| id == delivered_id),
        "successful release left delivered id in durable pending: {before_restart:?}; provider copies after real restart={}",
        delivery_count(boot)
    );
    assert_eq!(
        delivery_count(boot),
        1,
        "fresh start must not inherit delivered input"
    );
}

async fn issuance(fault: CheckpointFault) {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    boot.daemon
        .install_turn_start_return_hook_for_test(TurnStartReturnHook {
            entered: entered.clone(),
            release: release.clone(),
        });
    let id = enqueue(&boot, TEXT).await;
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    assert_eq!(stored_ids(&boot).await, vec![id.clone()]);
    assert_eq!(delivery_count(&boot), 1);
    fault.install(&boot).await;
    let shutdown = fault.release_after_result(&boot, &release).await;
    fault.finish_release(&boot, &id, shutdown).await;
    assert!(boot.registry.get(&boot.worker_session_id).is_none());
    assert_no_redelivery_after_restart(&boot, &id).await;
}

async fn accepted_steer(fault: CheckpointFault) {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    enqueue(&boot, "first turn").await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while !matches!(
            boot.harness.state_for_test().await,
            HarnessState::TurnRunning { .. }
        ) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    boot.daemon
        .install_turn_steer_return_hook_for_test(TurnStartReturnHook {
            entered: entered.clone(),
            release: release.clone(),
        });
    let id = enqueue(&boot, TEXT).await;
    let steer = tokio::spawn(send_json(
        boot.app.clone(),
        "POST",
        format!(
            "/api/cards/{}/planner/input/{id}/steer",
            boot.planner_card.id
        ),
        "user",
        json!({"if_entry_rev": 0}),
    ));
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    assert_eq!(stored_ids(&boot).await, vec![id.clone()]);
    assert_eq!(delivery_count(&boot), 1);
    fault.install(&boot).await;
    let shutdown = fault.release_after_result(&boot, &release).await;
    fault.finish_release(&boot, &id, shutdown).await;
    let (status, body) = joined(steer, "accepted steer").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(boot.registry.get(&boot.worker_session_id).is_none());
    assert_no_redelivery_after_restart(&boot, &id).await;
}

#[derive(Clone, Copy)]
enum CheckpointFault {
    None,
    UpdateError,
    Unwritten,
}

impl CheckpointFault {
    async fn release_after_result(
        self,
        boot: &Boot,
        backend_release: &Notify,
    ) -> tokio::task::JoinHandle<
        calm_server::error::Result<Option<calm_server::harness::PlannerHarness>>,
    > {
        if matches!(self, Self::None) {
            let shutdown = shutdown_during_return(boot).await;
            backend_release.notify_one();
            return shutdown;
        }
        // The owner is already parked in the backend hook. Observe the first
        // real UPDATE result before closing, without requiring the processing
        // future to finish: it may retain responsibility and retry that commit.
        // Closing first would let the old no-op hide the injected storage fault.
        let completed = PausePoint {
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        };
        install_pause_for_test(
            "conversation-input-checkpoint-attempted",
            boot.planner_card.id.as_str(),
            completed.clone(),
        );
        backend_release.notify_one();
        tokio::time::timeout(Duration::from_secs(10), completed.entered.notified())
            .await
            .unwrap();
        assert!(!boot.harness.is_closing_for_test());
        assert_eq!(delivery_count(boot), 1);
        let shutdown = shutdown_during_return(boot).await;
        completed.release.notify_one();
        shutdown
    }

    async fn install(self, boot: &Boot) {
        let refusal = match self {
            Self::None => return,
            Self::UpdateError => "RAISE(ABORT, 'result checkpoint unavailable')",
            Self::Unwritten => "RAISE(IGNORE)",
        };
        sqlx::query(&format!(
            "CREATE TRIGGER refuse_result_checkpoint BEFORE UPDATE OF handle_state_json \
             ON worker_sessions WHEN OLD.id = '{}' BEGIN SELECT {refusal}; END",
            boot.worker_session_id,
        ))
        .execute(boot.repo.db().writer_pool())
        .await
        .unwrap();
    }

    async fn finish_release(
        self,
        boot: &Boot,
        id: &str,
        mut shutdown: tokio::task::JoinHandle<
            calm_server::error::Result<Option<calm_server::harness::PlannerHarness>>,
        >,
    ) {
        if matches!(self, Self::None) {
            joined(shutdown, "ordered release after provider acceptance")
                .await
                .unwrap();
            return;
        }
        // Either a typed refusal or a still-owned processing future can retain
        // responsibility. A successful release with the old queue cannot.
        let finished = tokio::time::timeout(Duration::from_millis(300), &mut shutdown).await;
        let retry_shutdown = match finished {
            Ok(result) => {
                assert!(
                    result.unwrap().is_err(),
                    "release reported settlement despite a refused result checkpoint; durable={:?}, discoverable={}, provider copies={}",
                    stored_ids(boot).await,
                    boot.registry.get(&boot.worker_session_id).is_some(),
                    delivery_count(boot)
                );
                true
            }
            Err(_) => false,
        };
        assert!(
            boot.registry.get(&boot.worker_session_id).is_some(),
            "retain the input owner"
        );
        assert_eq!(
            stored_ids(boot).await,
            vec![id.to_owned()],
            "retain the original source/id"
        );
        assert_eq!(delivery_count(boot), 1, "do not retry provider delivery");
        sqlx::query("DROP TRIGGER refuse_result_checkpoint")
            .execute(boot.repo.db().writer_pool())
            .await
            .unwrap();
        if retry_shutdown {
            boot.registry
                .shutdown_and_remove(&boot.worker_session_id)
                .await
                .unwrap();
        } else {
            joined(shutdown, "same owner commits after checkpoint recovery")
                .await
                .unwrap();
        }
        assert!(
            stored_ids(boot).await.is_empty(),
            "retry commits the already accepted result"
        );
        assert_eq!(delivery_count(boot), 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn release_commit_successful_issuance_before_shutdown() {
    issuance(CheckpointFault::None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn release_commit_accepted_steer_before_shutdown() {
    accepted_steer(CheckpointFault::None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn release_commit_issuance_checkpoint_error_retains_retry() {
    issuance(CheckpointFault::UpdateError).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn release_commit_issuance_unwritten_checkpoint_retains_retry() {
    issuance(CheckpointFault::Unwritten).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn release_commit_steer_checkpoint_error_retains_retry() {
    accepted_steer(CheckpointFault::UpdateError).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn release_commit_steer_unwritten_checkpoint_retains_retry() {
    accepted_steer(CheckpointFault::Unwritten).await;
}

// The command passed ingress admission before closing; its ACK still owes a commit.
async fn queued_mutation(delete: bool, fault: CheckpointFault) {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Paused).await;
    let id = enqueue(&boot, TEXT).await;
    let paused = PausePoint {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    install_pause_for_test(
        "conversation-command-dequeued",
        boot.planner_card.id.as_str(),
        paused.clone(),
    );
    let request = tokio::spawn(send_json(
        boot.app.clone(),
        if delete { "DELETE" } else { "PATCH" },
        format!("/api/cards/{}/planner/input/{id}", boot.planner_card.id),
        "user",
        if delete {
            json!({"if_entry_rev": 0})
        } else {
            json!({"if_entry_rev": 0, "text": "edited before release"})
        },
    ));
    tokio::time::timeout(Duration::from_secs(10), paused.entered.notified())
        .await
        .unwrap();
    fault.install(&boot).await;
    let shutdown = shutdown_during_return(&boot).await;
    paused.release.notify_one();
    let (status, body) = joined(request, "queued mutation").await;
    let row = boot
        .repo
        .session_projection_by_id(&boot.worker_session_id)
        .await
        .unwrap()
        .unwrap();
    let entries =
        HarnessSnapshot::from_value_strict(row.handle_state_json.unwrap()).pending_entries();
    if matches!(fault, CheckpointFault::None) {
        assert_eq!(status, StatusCode::OK, "{body}");
        if delete {
            assert!(entries.is_empty());
        } else {
            assert_eq!(entries.len(), 1);
            assert!(format!("{entries:?}").contains("edited before release"));
        }
    } else {
        assert_ne!(
            status,
            StatusCode::OK,
            "refused checkpoint must not ACK: {body}"
        );
        assert_eq!(entries.len(), 1);
        assert!(format!("{entries:?}").contains(TEXT));
        let pending = boot.harness.pending_entries_for_test().await;
        assert_eq!(pending, entries, "rollback canonical queue");
        sqlx::query("DROP TRIGGER refuse_result_checkpoint")
            .execute(boot.repo.db().writer_pool())
            .await
            .unwrap();
    }
    joined(shutdown, "release queued mutation").await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn release_commit_queued_edit_before_shutdown() {
    queued_mutation(false, CheckpointFault::None).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn release_commit_queued_delete_before_shutdown() {
    queued_mutation(true, CheckpointFault::None).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn release_commit_queued_edit_checkpoint_error_rolls_back() {
    queued_mutation(false, CheckpointFault::UpdateError).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn release_commit_queued_delete_unwritten_rolls_back() {
    queued_mutation(true, CheckpointFault::Unwritten).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn release_commit_tick_retries_issuance_without_provider_redelivery() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    boot.daemon
        .install_turn_start_return_hook_for_test(TurnStartReturnHook {
            entered: entered.clone(),
            release: release.clone(),
        });
    let id = enqueue(&boot, TEXT).await;
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    CheckpointFault::UpdateError.install(&boot).await;
    let attempted = PausePoint {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    install_pause_for_test(
        "conversation-input-checkpoint-attempted",
        boot.planner_card.id.as_str(),
        attempted.clone(),
    );
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), attempted.entered.notified())
        .await
        .unwrap();
    assert_eq!(stored_ids(&boot).await, vec![id.clone()]);
    attempted.release.notify_one();
    // Drain the real TurnStarted checkpoint while the fault still holds. Otherwise that
    // incidental notification write could masquerade as the tick's commit retry.
    tokio::time::timeout(Duration::from_secs(10), async {
        while !matches!(
            boot.harness.state_for_test().await,
            HarnessState::TurnRunning { .. }
        ) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (status, body) = send_json(
        boot.app.clone(),
        "PATCH",
        format!("/api/cards/{}/planner/input/{id}", boot.planner_card.id),
        "user",
        json!({"if_entry_rev":0,"text":"barrier must not apply"}),
    )
    .await;
    assert_ne!(status, StatusCode::OK, "{body}");
    assert_eq!(stored_ids(&boot).await, vec![id]);
    sqlx::query("DROP TRIGGER refuse_result_checkpoint")
        .execute(boot.repo.db().writer_pool())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !stored_ids(&boot).await.is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(delivery_count(&boot), 1);
    assert!(!boot.harness.is_closing_for_test());
    boot.registry
        .shutdown_and_remove(&boot.worker_session_id)
        .await
        .unwrap();
}

async fn cancelled_steer_terminal(fault: CheckpointFault) {
    use crate::support::planner_queue_fixture::SEED_THREAD_ID;
    use calm_server::codex_appserver::Notification;

    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    enqueue(&boot, "first turn").await;
    let turn = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let HarnessState::TurnRunning { turn_id, .. } = boot.harness.state_for_test().await {
                break turn_id;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    boot.daemon
        .install_turn_steer_return_hook_for_test(TurnStartReturnHook {
            entered: entered.clone(),
            release: release.clone(),
        });
    let id = enqueue(&boot, TEXT).await;
    let request = tokio::spawn(send_json(
        boot.app.clone(),
        "POST",
        format!(
            "/api/cards/{}/planner/input/{id}/steer",
            boot.planner_card.id
        ),
        "user",
        json!({"if_entry_rev": 0}),
    ));
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    fault.install(&boot).await;
    let shutdown = fault.release_after_result(&boot, &release).await;
    assert!(
        joined(shutdown, "refused cancelled steer commit")
            .await
            .is_err()
    );
    assert!(boot.registry.get(&boot.worker_session_id).is_some());
    boot.daemon
        .emit_notification_for_test(Notification::ThreadStatusChanged {
            thread_id: SEED_THREAD_ID.into(),
            status: json!({"type":"systemError"}),
        });
    boot.daemon
        .emit_notification_for_test(Notification::TurnCompleted {
            thread_id: SEED_THREAD_ID.into(),
            turn: json!({"id":turn, "status":"failed"}),
        });
    // As in the restoration fixture, fake notifications bypass the daemon reducer.
    boot.daemon.clear_active_turn_for_test(SEED_THREAD_ID);
    tokio::time::timeout(Duration::from_secs(10), async {
        while boot
            .harness
            .snapshot()
            .await
            .pending_entries()
            .first()
            .and_then(|e| e.user_view())
            .is_none_or(|e| e.rev != 1)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // Observe a real retry after terminal restoration while the same fault still holds.
    let terminal_attempt = PausePoint {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    install_pause_for_test(
        "conversation-input-checkpoint-attempted",
        boot.planner_card.id.as_str(),
        terminal_attempt.clone(),
    );
    tokio::time::timeout(Duration::from_secs(10), terminal_attempt.entered.notified())
        .await
        .unwrap();
    assert_eq!(stored_ids(&boot).await, vec![id.clone()]);
    sqlx::query("DROP TRIGGER refuse_result_checkpoint")
        .execute(boot.repo.db().writer_pool())
        .await
        .unwrap();
    terminal_attempt.release.notify_one();
    boot.registry
        .shutdown_and_remove(&boot.worker_session_id)
        .await
        .unwrap();
    let row = boot
        .repo
        .session_projection_by_id(&boot.worker_session_id)
        .await
        .unwrap()
        .unwrap();
    let entries =
        HarnessSnapshot::from_value_strict(row.handle_state_json.unwrap()).pending_entries();
    assert_eq!(entries.len(), 1);
    let restored = entries[0].user_view().unwrap();
    assert_eq!(restored.id.as_str(), id);
    assert_eq!(
        restored.rev, 1,
        "commit retry must not overwrite terminal restoration with the accepted snapshot"
    );
    assert_eq!(restored.text, TEXT);
    assert_eq!(delivery_count(&boot), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn release_commit_cancelled_steer_retains_latest_terminal_input() {
    cancelled_steer_terminal(CheckpointFault::UpdateError).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn release_commit_cancelled_steer_unwritten_retains_latest_terminal_input() {
    cancelled_steer_terminal(CheckpointFault::Unwritten).await;
}

#[path = "planner_release_commit/completion.rs"]
mod completion;
