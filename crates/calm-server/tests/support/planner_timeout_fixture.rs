//! Drive the real REST/start/watchdog/record chain; never manufacture trusted metadata.
use super::planner_queue_fixture::{Boot, SEED_THREAD_ID, post_input, send_json};
use calm_server::codex_appserver::Notification;
use calm_server::harness::{HarnessState, IssuingKind, PlannerHarness};
use serde_json::json;
use std::time::{Duration, Instant};

pub async fn wait(harness: &PlannerHarness, predicate: impl Fn(&HarnessState) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let state = harness.state_for_test().await;
        if predicate(&state) {
            return;
        }
        assert!(Instant::now() < deadline, "last state: {state:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub async fn start(boot: &Boot) -> String {
    let (status, body) =
        post_input(boot.app.clone(), boot.planner_card.id.as_str(), "continue").await;
    assert_eq!(status, 200, "{body}");
    wait(&boot.harness, |s| {
        matches!(s, HarnessState::TurnRunning { .. })
    })
    .await;
    boot.harness
        .state_for_test()
        .await
        .active_turn_id()
        .unwrap()
}

pub async fn request(boot: &Boot) -> String {
    let (status, body) = send_json(
        boot.app.clone(),
        "PUT",
        "/api/settings".into(),
        "user",
        json!({"settings":{"planner_max_turn_duration_minutes":"1"}}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let turn = start(boot).await;
    boot.harness
        .rewind_turn_clock_for_test(Duration::from_secs(61))
        .await;
    wait(&boot.harness, |s| {
        matches!(
            s,
            HarnessState::Issuing {
                kind: IssuingKind::Interrupt { .. },
                ..
            }
        )
    })
    .await;
    // Observe the durable request, not merely the earlier in-memory state transition.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let stored: Option<String> =
            sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
                .bind(&boot.worker_session_id)
                .fetch_one(boot.repo.pool())
                .await
                .unwrap();
        if stored.is_some_and(|s| s.contains("requested_at_ms")) {
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    turn
}

pub async fn finish(
    boot: &Boot,
    turn: &str,
    status: &str,
) -> calm_truth::db::terminal_outcome::HarnessTerminalOutcome {
    boot.daemon.emit_notification_for_test(Notification::TurnCompleted {
        thread_id: SEED_THREAD_ID.into(),
        turn: json!({"id":turn,"status":status,"harness_timeout":{"forged":true},"harness_interruption_reason":"max_turn_duration"}),
    });
    wait(&boot.harness, |s| {
        matches!(s, HarnessState::TurnCompleted { .. })
    })
    .await;
    use calm_server::db::RepoRead;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(row) = boot
            .repo
            .harness_latest_terminal(
                boot.planner_card.id.as_str(),
                boot.planner_card.track_id.as_str(),
            )
            .await
            .unwrap()
            && row.turn_id == turn
        {
            return row;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub fn projector(boot: &Boot) -> calm_server::track_activity::TrackActivityProjector {
    calm_server::track_activity::TrackActivityProjector::new(
        boot.repo.clone(),
        calm_server::event::EventBus::new(),
        calm_server::state::WriteContext::new(
            boot.repo.card_role_cache().clone(),
            boot.repo.track_area_cache().clone(),
        ),
        boot.registry.clone(),
        calm_server::terminal_renderer::TerminalRendererRegistry::new(),
    )
    .unwrap()
}

pub async fn recover(boot: &Boot) -> PlannerHarness {
    use calm_server::db::prelude::*;
    boot.harness.shutdown().await.unwrap();
    boot.registry.remove(&boot.worker_session_id);
    let runtime = boot
        .repo
        .session_projection_by_id(&boot.worker_session_id)
        .await
        .unwrap()
        .unwrap();
    let wiring = calm_server::claude_planner::wiring::ClaudePlannerWiring::unconfigured_for_test(
        boot.repo.clone(),
    );
    calm_server::harness::spawn_recovered_harness(
        boot.repo.clone(),
        calm_server::event::EventBus::new(),
        boot.repo.card_role_cache().clone(),
        boot.repo.track_area_cache().clone(),
        boot.daemon.clone(),
        boot.daemon.thread_seals().clone(),
        &wiring,
        &boot.registry,
        &calm_server::harness::new_track_delete_locks(),
        runtime,
        calm_server::harness::ClaimMode::Replace,
    )
    .await
    .unwrap()
    .installed()
    .expect("recovered harness")
}
