//! C1b: real clean and deletion against an already idle Planner's next turn.
use super::*;
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessSnapshot, Observation, PlannerHarness,
    PlannerHarnessParams,
};

async fn planner(b: &Boot, track: &str, rewind: bool) -> (String, PlannerHarness) {
    let (runtime, card, thread): (String, String, String) = sqlx::query_as(
        "SELECT id, card_id, thread_id FROM worker_sessions WHERE card_id IN \
         (SELECT id FROM cards WHERE track_id=?) LIMIT 1",
    )
    .bind(track)
    .fetch_one(b.repo.pool())
    .await
    .unwrap();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread.clone());
    if rewind {
        snapshot.pending_rewind = Some(
            serde_json::from_value(json!({
                "provider":"codex", "before_turn_id":"old-turn", "reverted":false
            }))
            .unwrap(),
        );
    }
    sqlx::query("UPDATE worker_sessions SET state='idle', completed_at_ms=NULL, handle_state_json=? WHERE id=?")
        .bind(serde_json::to_string(&snapshot).unwrap()).bind(&runtime)
        .execute(b.repo.pool()).await.unwrap();
    let handle = PlannerHarness::run(PlannerHarnessParams {
        track_delete_locks: b.state.operation_runtime.track_locks_for_test().clone(),
        worker_session_id: runtime.clone(),
        track_id: track.to_owned().into(),
        card_id: card.into(),
        thread_id: Some(thread),
        repo: b.repo.clone(),
        events: b.state.events.clone(),
        card_role_cache: b.state.card_role_cache.clone(),
        track_area_cache: b.state.track_area_cache.clone(),
        backend: b.state.shared_codex_appserver.clone().into(),
        live_replies: b.state.harness.live_replies().clone(),
        config: HarnessConfig {
            debounce_min_idle: Duration::ZERO,
            debounce_max_wait: Duration::ZERO,
            ..HarnessConfig::default()
        },
        snapshot,
    });
    b.state.harness.insert(runtime.clone(), handle.clone());
    (runtime, handle)
}
async fn status(b: &Boot, runtime: &str) -> String {
    sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id=?")
        .bind(runtime)
        .fetch_one(b.repo.pool())
        .await
        .unwrap()
}
async fn issued(b: &Boot, before: u64) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while b.state.shared_codex_appserver.turn_start_count_for_test() == before {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Planner must issue after clean releases");
}
async fn enqueue(handle: &PlannerHarness, automatic: bool) {
    if automatic {
        handle
            .observe(Observation::TrackGoal {
                text: "observe new goal".into(),
            })
            .unwrap();
    } else {
        handle
            .observe_user_message_durable(
                "next message".into(),
                vec![],
                calm_server::harness::SendKey::unique_for_test(),
            )
            .await
            .unwrap();
    }
}
#[tokio::test]
async fn c1_fence_planner_issuance_wins() {
    let (b, track, path) = fixture().await;
    let (runtime, handle) = planner(&b, &track, false).await;
    let before = b.state.shared_codex_appserver.turn_start_count_for_test();
    let pause = Pause::arm("c1-clean-sampled", &track);
    let cleaning = clean(b.clone());
    pause.entered().await;
    enqueue(&handle, false).await;
    issued(&b, before).await;
    assert_eq!(status(&b, &runtime).await, "turn_pending");
    drop(pause);
    let (count, history) = cleaning.await.unwrap();
    assert_eq!(count, 0);
    assert!(history.is_empty());
    assert!(path.join("target/ignored").exists());
    b.shutdown_harnesses().await;
}
async fn clean_wins(automatic: bool, rewind: bool) {
    let (b, track, path) = fixture().await;
    let (runtime, handle) = planner(&b, &track, rewind).await;
    let before = b.state.shared_codex_appserver.turn_start_count_for_test();
    let pause = Pause::arm("c1-clean-fenced", &track);
    let cleaning = clean(b.clone());
    pause.entered().await;
    let arrival = Pause::arm("key-lock-waiting", &track);
    enqueue(&handle, automatic).await;
    arrival.entered().await;
    assert_eq!(status(&b, &runtime).await, "idle");
    assert_eq!(
        b.state.shared_codex_appserver.turn_start_count_for_test(),
        before
    );
    assert_eq!(handle.snapshot().await.phase, HarnessPhaseTag::Idle);
    drop(arrival);
    drop(pause);
    assert_eq!(cleaning.await.unwrap().0, 1);
    assert!(!path.join("target/ignored").exists());
    issued(&b, before).await;
    assert_eq!(status(&b, &runtime).await, "turn_pending");
    if rewind {
        assert_eq!(
            b.state
                .shared_codex_appserver
                .reverted_threads_for_test()
                .len(),
            1
        );
    }
    b.shutdown_harnesses().await;
}
#[tokio::test]
async fn c1_fence_planner_clean_wins_message() {
    clean_wins(false, false).await;
}
#[tokio::test]
async fn c1_fence_planner_clean_wins_auto_observe() {
    clean_wins(true, false).await;
}
#[tokio::test]
async fn c1_fence_planner_clean_wins_pending_rewind() {
    clean_wins(false, true).await;
}
#[tokio::test]
async fn c1_fence_planner_delete_while_waiting() {
    let (b, track, _) = fixture().await;
    let (_, handle) = planner(&b, &track, false).await;
    let before = b.state.shared_codex_appserver.turn_start_count_for_test();
    use calm_server::routes::tracks::{
        TrackDeleteTeardownHook, install_track_delete_teardown_hook_for_test,
    };
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    install_track_delete_teardown_hook_for_test(
        &track,
        TrackDeleteTeardownHook {
            entered: entered.clone(),
            release: release.clone(),
        },
    );
    let deleting_boot = b.clone();
    let endpoint = format!("/api/tracks/{track}");
    let deleting = tokio::spawn(async move {
        deleting_boot
            .as_user("DELETE", &endpoint, Value::Null)
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let arrival = Pause::arm("key-lock-waiting", &track);
    enqueue(&handle, false).await;
    arrival.entered().await;
    drop(arrival);
    // A durable caller can already hold ingress while awaiting this blocked loop.
    let queued = handle.observe_user_message_durable(
        "also accepted".into(),
        vec![],
        calm_server::harness::SendKey::unique_for_test(),
    );
    tokio::pin!(queued);
    tokio::select! {
        biased;
        result = &mut queued => panic!("command passed blocked owner: {result:?}"),
        _ = std::future::ready(()) => {}
    }
    release.notify_one();
    let (deleted, accepted) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(deleting, queued)
    })
    .await
    .expect("delete must not deadlock on issuance or durable ingress");
    let (code, body) = deleted.unwrap();
    accepted.unwrap();
    assert!(code.is_success(), "{body}");
    assert_eq!(
        b.state.shared_codex_appserver.turn_start_count_for_test(),
        before
    );
    assert_eq!(handle.snapshot().await.pending_entries().len(), 2);
}
