//! V1 intake preserves existing queue identities and optional acknowledgements.
//! Historical data is loaded by the recovery owner, never assigned fabricated IDs.
use super::*;
use crate::harness::{MAX_PENDING_QUEUE_LEN, Observation, PlannerHarness};
use crate::state::{CodexShellState, RouteState};
use axum::extract::FromRef;

struct LegacyQueue {
    _fixture: Fixture,
    state: AppState,
    app: axum::Router,
    cookie: String,
    card: String,
    worker: String,
    harness: PlannerHarness,
}

impl LegacyQueue {
    async fn with_tail(reclaimable_slot: bool, addressable_tail: bool) -> Self {
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application_with_historical_native(true).await;
        let area = post(
            &app,
            &cookie,
            "/api/areas",
            "area",
            json!({"name":"Legacy addressability","color":"#123456"}),
        )
        .await;
        let track = crate::test_support::historical_track(
            &state,
            json!({
                "area_id":area["id"],"planner_provider":"codex","title":"",
                "theme":{"fg":[255,255,255],"bg":[0,0,0]}
            }),
            Some("historical"),
        )
        .await;
        let (card, worker): (String, String) =
            sqlx::query_as("SELECT id,session_id FROM cards WHERE track_id=?1 AND role='planner'")
                .bind(track["id"].as_str().unwrap())
                .fetch_one(fixture.repo.pool())
                .await
                .unwrap();
        state
            .harness
            .remove(&worker)
            .unwrap()
            .shutdown()
            .await
            .unwrap();
        let runtime = fixture
            .repo
            .session_projection_by_id(&worker)
            .await
            .unwrap()
            .unwrap();
        // Begin with the actual server-produced frame. Removing entry metadata
        // represents the released pre-ID V1 format; the real codec owns loading.
        let mut stored = runtime.handle_state_json.unwrap();
        let mut observations = (0..MAX_PENDING_QUEUE_LEN)
            .map(|index| Observation::UserMessage {
                text: format!("Original historical message {index}"),
            })
            .collect::<Vec<_>>();
        if reclaimable_slot {
            observations[0] = Observation::TrackGoal {
                text: "Original replaceable kernel context".into(),
            };
        }
        stored["pending_queue"] = serde_json::to_value(observations).unwrap();
        stored.as_object_mut().unwrap().remove("pending_entry_meta");
        if addressable_tail {
            let mut frame = crate::harness::HarnessSnapshot::from_value_strict(stored);
            let mut entries = frame.pending_entries();
            *entries.last_mut().unwrap() = crate::harness::QueueEntry::user_message(
                "Existing addressable tail".into(),
                None,
                vec![],
            );
            frame.set_pending_entries(entries);
            stored = serde_json::to_value(frame).unwrap();
        }
        let runtime_id = worker.clone();
        crate::db::write_in_tx_typed(fixture.repo.as_ref(), move |tx| {
            Box::pin(async move {
                assert!(
                    crate::db::sqlite::session_set_handle_state_tx(tx, &runtime_id, Some(stored))
                        .await?
                );
                Ok(())
            })
        })
        .await
        .unwrap();
        let runtime = fixture
            .repo
            .session_projection_by_id(&worker)
            .await
            .unwrap()
            .unwrap();
        // Existing kernel recovery services accepted historical work. On this
        // current-thread runtime the returned handle is paused before yielding.
        let harness = crate::planner_hibernation::WakeContext::new(
            RouteState::from_ref(&state),
            fixture.repo.clone(),
            CodexShellState::from_ref(&state),
        )
        .install(runtime)
        .await
        .unwrap();
        harness.pause_issuance_for_dev();
        let loaded = harness.snapshot().await.unwrap().pending_entries();
        assert_eq!(loaded.len(), MAX_PENDING_QUEUE_LEN);
        assert_eq!(loaded.last().unwrap().id().is_some(), addressable_tail);
        if !addressable_tail {
            assert!(
                matches!(
                    loaded.last().unwrap(),
                    crate::harness::QueueEntry::LegacyUser { .. }
                ),
                "the actual old codec loads an unaddressable LegacyUser tail"
            );
        }
        Self {
            _fixture: fixture,
            state,
            app,
            cookie,
            card,
            worker,
            harness,
        }
    }

    fn uri(&self) -> String {
        format!("/api/cards/{}/planner/input", self.card)
    }

    async fn close(self) {
        self.state
            .harness
            .remove(&self.worker)
            .unwrap()
            .shutdown()
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn verified_public_v1_fold_can_keep_an_existing_real_user_tail_id() {
    let fx = LegacyQueue::with_tail(false, true).await;
    let before = fx.harness.snapshot().await.unwrap().pending_entries();
    let tail = before.last().unwrap().user_view().unwrap();
    let accepted = post(
        &fx.app,
        &fx.cookie,
        &fx.uri(),
        "real-tail",
        json!({"text":"Append using the addressable target"}),
    )
    .await;
    assert_eq!(accepted["entry_id"], tail.id.as_str());
    let after = fx.harness.snapshot().await.unwrap().pending_entries();
    assert_eq!(after.len(), MAX_PENDING_QUEUE_LEN);
    let folded = after.last().unwrap().user_view().unwrap();
    assert_eq!(folded.id, tail.id);
    assert_eq!(folded.rev, tail.rev + 1);
    assert!(folded.text.contains(tail.text));
    assert!(folded.text.contains("Append using the addressable target"));
    fx.close().await;
}
