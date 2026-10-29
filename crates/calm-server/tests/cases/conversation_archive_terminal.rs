//! The original historical END, held after memory publication and before persistence.
use super::*;
use calm_server::test_seams::{PausePoint, install_pause_for_test};
use std::time::Duration;

pub(super) async fn join_planner(b: &Boot, track: &str) {
    let planner: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=? AND role='planner'")
            .bind(track)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    let runtime = b
        .repo
        .session_projection_active_for_card(&planner)
        .await
        .unwrap()
        .unwrap();
    let handle = b.state.harness.get(&runtime.id).unwrap();
    assert_eq!(handle.pending_len_for_test().await, 0);
    assert!(
        b.state
            .shared_codex_appserver
            .started_turns_for_test()
            .iter()
            .all(|(thread, _)| Some(thread) != runtime.thread_id.as_ref())
    );
    // shutdown awaits the existing run_loop_task, including its startup held sweep.
    // This is preparation before the event baseline, not a fabricated ACK or drain.
    tokio::time::timeout(Duration::from_secs(5), handle.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(handle.is_closing_for_test());
    // Join an already-running scheduler pass on this original track's lock.
    // A queued TrackUpdated poke is read-only too: this fixture has no tasks or deliveries.
    b.state
        .dispatcher
        .scheduler()
        .schedule_track(track.to_owned().into())
        .await;
    assert!(b.repo.tasks_by_track(track).await.unwrap().is_empty());
}

async fn receipt_observation(b: &Boot, track: &str, card: &str, receipt: &Value) {
    let (status, observation) = b
        .request(
            "GET",
            &format!("/api/tracks/{track}/conversations/{card}/initial-input"),
            Some("observation-archive"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{observation}");
    assert_eq!(
        &observation["receipt"], receipt,
        "exact original journal receipt"
    );
    // The fake provider sends no user-item echo. An honest unavailable projection
    // still passed through InspectInput; it must retain the exact original receipt.
    if observation["kind"] == "unavailable" {
        assert!(
            matches!(
                observation["reason"].as_str(),
                Some("runtime-unavailable" | "legacy-projection")
            ),
            "{observation}"
        );
    }
}

/// The historical phase read above does not join END persistence. The existing
/// receipt-bound HTTP inspection runs on that same serial owner after END returns.
pub(super) async fn finish_initial_turn(b: &Boot, track: &str, card: &str, receipt: &Value) {
    finish_turn(b, card).await;
    receipt_observation(b, track, card, receipt).await;
}

#[tokio::test]
async fn receipt_bound_preparation_waits_for_original_end_before_archive_baseline() {
    use calm_server::harness::HarnessPhaseTag;
    use sqlx::Row;
    let b = boot().await;
    let track = b.historical_track("Archived observations").await;
    join_planner(&b, &track).await;
    let (status, row) = b
        .create_internal_conversation(&track, "observation-archive", "first")
        .await;
    assert_eq!(status, StatusCode::CREATED, "{row}");
    assert_eq!(row["initialInput"]["kind"], "known", "{row}");
    let receipt = &row["initialInput"]["receipt"];
    let card = row["id"].as_str().unwrap();
    receipt_observation(&b, &track, card, receipt).await;
    let runtime = b
        .repo
        .session_projection_active_for_card(&card.to_owned())
        .await
        .unwrap()
        .unwrap();
    let handle = b.state.harness.get(&runtime.id).unwrap();
    let cut = PausePoint::default();
    install_pause_for_test("historical-turn-completed-published", card, cut.clone());
    let mut preparation = Box::pin(finish_initial_turn(&b, &track, card, receipt));
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            _ = cut.entered.notified() => {},
            _ = preparation.as_mut() => panic!("preparation returned before original END was released"),
        }
    }).await.unwrap();
    assert_eq!(
        handle.snapshot().await.unwrap().phase,
        HarnessPhaseTag::TurnCompleted
    );
    let turn = handle.snapshot().await.unwrap().last_turn_id.unwrap();
    let last_event: i64 = sqlx::query_scalar("SELECT MAX(id) FROM events")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    let before_end: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), preparation.as_mut())
            .await
            .is_err(),
        "phase-only finish must not complete receipt-bound preparation while END is held"
    );
    cut.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), preparation)
        .await
        .unwrap();
    let settled = b
        .repo
        .session_projection_active_for_card(&card.to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(settled.id, runtime.id);
    assert_eq!(
        settled.handle_state_json.as_ref().unwrap()["phase"],
        "turn_completed"
    );
    let outcome = b
        .repo
        .harness_latest_terminal(card, &track)
        .await
        .unwrap()
        .expect("original terminal outcome is durable before preparation completes");
    assert_eq!(outcome.worker_session_id, runtime.id);
    assert_eq!(outcome.card_id, card);
    assert_eq!(outcome.track_id, track);
    assert_eq!(outcome.thread_id, runtime.thread_id.unwrap());
    assert_eq!(outcome.turn_id, turn);
    assert_eq!(
        serde_json::from_str::<Value>(&outcome.params).unwrap(),
        json!({"id":turn,"status":"completed"})
    );
    let events = sqlx::query("SELECT * FROM events WHERE id>? ORDER BY id")
        .bind(last_event)
        .fetch_all(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        events.len(),
        1,
        "only the selected original END adds an event"
    );
    let event = &events[0];
    assert_eq!(event.get::<String, _>("kind"), "harness.phase.changed");
    assert_eq!(
        serde_json::from_str::<Value>(&event.get::<String, _>("actor")).unwrap(),
        json!({"kind":"Kernel"})
    );
    assert_eq!(event.get::<String, _>("scope_kind"), "card");
    assert_eq!(event.get::<String, _>("scope_card"), card);
    assert_eq!(event.get::<String, _>("scope_track"), track);
    assert_eq!(event.get::<String, _>("scope_area"), b.area_id);
    assert_eq!(
        serde_json::from_str::<Value>(&event.get::<String, _>("payload")).unwrap(),
        json!({"worker_session_id":runtime.id,"card_id":card,"track_id":track,
            "old_phase":"turn_running","new_phase":"turn_completed"})
    );
    let after_end: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(after_end, before_end + 1);
    assert_eq!(
        archive(&b, &track, card, true).await.0,
        StatusCode::NO_CONTENT
    );
    handle
        .observe_for_test(
            calm_server::harness::Observation::UserMessage {
                text: "buffered observation".into(),
            },
            None,
        )
        .await;
    assert_eq!(handle.pending_len_for_test().await, 1);
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        archive(&b, &track, card, true).await.0,
        StatusCode::NO_CONTENT
    );
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        before, after,
        "idempotent assignment emits no duplicate event"
    );
    assert_eq!(handle.pending_len_for_test().await, 1);
}
