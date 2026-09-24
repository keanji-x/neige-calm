use super::*;
use calm_server::event::{Event, EventScope};
use calm_server::harness::run_loop::{
    PlannerHarnessObservationRaceHook, install_planner_harness_observation_race_hook_for_test,
};
use calm_server::ids::ActorId;
use tokio::sync::Notify;

#[tokio::test]
#[allow(deprecated)]
async fn recovery_cannot_commit_an_observation_watermark_without_its_queue_entry() {
    let boot = boot_fake_running().await;
    boot.state.shared_codex_appserver.fail_turn_start_for_test();
    let (card, runtime, thread, retained) = failed_conversation(&boot).await;
    let row = runtime_by_id_tx_snapshot(&boot.repo, &runtime)
        .await
        .unwrap();
    let old = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: runtime.clone(),
        card_id: card.id.clone(),
        track_id: card.track_id.clone(),
        thread_id: Some(thread),
        repo: boot.repo.clone(),
        events: boot.state.events.clone(),
        card_role_cache: boot.state.card_role_cache.clone(),
        track_area_cache: boot.state.track_area_cache.clone(),
        backend: boot.state.shared_codex_appserver.clone().into(),
        config: HarnessConfig::default(),
        snapshot: HarnessSnapshot::from_value_strict(row.handle_state_json.unwrap()),
    });
    boot.state.harness.insert(runtime.clone(), old.clone());

    // This real event can be recovered through normal Planner catch-up. The
    // direct envelope ingress models a delivery already queued before failure,
    // or a producer that resolved the live handle before systemError arrived.
    let track = boot.repo.track_get(&boot.track_id).await.unwrap().unwrap();
    let task_key = format!("{}:recovery-observation", track.id);
    let result = json!({"message":"completion must reach the original conversation"});
    let event_id = boot
        .repo
        .log_pure_event(
            ActorId::Kernel,
            EventScope::Track {
                track: track.id.clone(),
                area: track.area_id,
            },
            None,
            &boot.state.events,
            &boot.state.card_role_cache,
            &boot.state.track_area_cache,
            Event::TaskCompleted {
                idempotency_key: task_key.clone(),
                result: result.clone(),
                artifacts: vec![],
                agent_message: None,
            },
        )
        .await
        .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    install_planner_harness_observation_race_hook_for_test(
        &runtime,
        PlannerHarnessObservationRaceHook {
            entered: entered.clone(),
            release: release.clone(),
        },
    );
    old.observe_envelope(
        Observation::TaskCompleted {
            idempotency_key: task_key,
            result,
        },
        event_id,
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    let paused = old.snapshot().await;
    assert_eq!(
        paused.push_watermark, event_id,
        "the real delivery advanced its watermark"
    );
    assert!(
        paused
            .pending_entries()
            .iter()
            .all(|entry| entry.envelope_id() != Some(event_id))
    );

    let uri = format!("/api/cards/{}/planner/input", card.id);
    let app = boot.app.clone();
    let mut recovery = tokio::spawn(async move {
        post_json(
            app,
            &uri,
            json!({"text":"resume without losing the completion"}),
        )
        .await
    });
    // An external snapshot/abort finishes while the delivery is paused. A
    // run-loop quiescence barrier waits, so release it after this bounded
    // negative observation and check the actual durable result in both cases.
    let early = tokio::time::timeout(Duration::from_secs(2), &mut recovery).await;
    release.notify_one();
    let (status, body) = match early {
        Ok(result) => result.unwrap(),
        Err(_) => tokio::time::timeout(Duration::from_secs(5), recovery)
            .await
            .unwrap()
            .unwrap(),
    };
    assert_eq!(status, StatusCode::OK, "{body}");
    let recovered = runtime_by_id_tx_snapshot(&boot.repo, &runtime)
        .await
        .unwrap();
    let snapshot = HarnessSnapshot::from_value_strict(recovered.handle_state_json.unwrap());
    boot.state
        .harness
        .remove(&runtime)
        .unwrap()
        .shutdown()
        .await
        .unwrap();
    assert!(
        snapshot.pending_entries().contains(&retained),
        "existing input remains unchanged"
    );
    assert_eq!(snapshot.push_watermark, event_id);
    assert_eq!(
        snapshot
            .pending_entries()
            .iter()
            .filter(|entry| entry.envelope_id() == Some(event_id))
            .count(),
        1,
        "quiescence must retain the event whose watermark it commits; catch-up cannot replay past that watermark",
    );
}
