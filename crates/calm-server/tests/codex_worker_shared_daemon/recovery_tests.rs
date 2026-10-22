//! Exercise actual dispatch and deletion control after a connection-only failure.
use crate::*;

#[tokio::test]
async fn worker_dispatch_and_deletion_control_recover_without_restarting_daemon() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot(true).await;
    let before = boot.shared.status_snapshot();
    let socket = PathBuf::from(&before.sock);
    std::fs::write(socket.with_extension("disconnect-once"), "model/list").unwrap();
    assert!(
        boot.shared
            .model_list(tokio::time::Instant::now() + Duration::from_secs(2))
            .await
            .is_err()
    );
    // No explicit recovery or healthy model call: the real worker path must recover.
    let _dispatcher = spawn_dispatcher(&boot);
    let key = "worker-after-connection-loss";
    let idem = task_id(&boot, key);
    write_codex_task_block(&boot, key, "worker after transport failure").await;
    let card = wait_for(Duration::from_secs(10), || async {
        let cards = boot
            .repo
            .cards_by_track(boot.track_id.as_str())
            .await
            .unwrap();
        for card in cards {
            if card.payload["idempotency_key"].as_str() != Some(idem.as_str()) {
                continue;
            }
            let runtime = boot
                .repo
                .session_projection_projectable_for_card(&card.id.to_string())
                .await
                .unwrap();
            if runtime.as_ref().is_some_and(|runtime| {
                runtime.thread_id.as_deref() == Some("fake-thread-0001")
                    && runtime.active_turn_id.is_some()
            }) {
                let methods =
                    std::fs::read_to_string(socket.with_extension("methods")).unwrap_or_default();
                if methods.lines().any(|method| method == "turn/start") {
                    return Some(card);
                }
            }
        }
        None
    })
    .await
    .expect("worker must actually start through the recovered connection");
    assert!(
        matches!(
            boot.shared
                .interrupt_active_outcome("fake-thread-0001")
                .await,
            provider::control::NativeInterruptOutcome::NativeAck
        ),
        "deletion quiesce must receive an actual interrupt acknowledgement"
    );
    boot.shared
        .forget_threads_for_deleted_cards(&std::collections::HashSet::from([card.id.to_string()]))
        .await;
    assert!(
        !boot
            .shared
            .resume_candidates_for_test()
            .iter()
            .any(|(_, owner)| owner == card.id.as_str())
    );
    let after = boot.shared.status_snapshot();
    assert_eq!(before.runtime.unwrap().pid, after.runtime.unwrap().pid);
    assert_eq!(before.restart_count, after.restart_count);
    let methods = std::fs::read_to_string(socket.with_extension("methods")).unwrap();
    assert_eq!(
        methods
            .lines()
            .filter(|method| *method == "initialize")
            .count(),
        2
    );
    assert_eq!(
        methods
            .lines()
            .filter(|method| *method == "turn/start")
            .count(),
        1
    );
}
