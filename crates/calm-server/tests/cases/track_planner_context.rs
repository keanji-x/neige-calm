//! The production context projector, durable sessions and task lifecycle.
use super::track_activity_fixture::{Fx, fx};
use calm_server::{
    db::sqlite::{begin_immediate_tx, session_set_handle_state_tx},
    harness::HarnessSnapshot,
    model::CardRole,
    session_projection_repo::{WorkerSessionKind, WorkerSessionState},
    track_planner_context::{PLANNER_CONTEXT_OVERLAY_KIND, TrackPlannerContextProjector},
};
use serde_json::{Value, json};

fn projector(fx: &Fx) -> TrackPlannerContextProjector {
    TrackPlannerContextProjector::new(fx.repo_dyn.clone(), fx.events.clone(), fx.write.clone())
        .unwrap()
}

async fn payload(fx: &Fx, track: &str) -> Value {
    fx.repo_dyn
        .overlays_for("track", track)
        .await
        .unwrap()
        .into_iter()
        .find(|o| {
            o.plugin_id == calm_types::plugin::KERNEL_OVERLAY_PLUGIN_ID
                && o.kind == PLANNER_CONTEXT_OVERLAY_KIND
        })
        .unwrap()
        .payload
}

async fn persist(fx: &Fx, value: Value) {
    let mut tx = begin_immediate_tx(fx.db()).await.unwrap();
    assert!(
        session_set_handle_state_tx(&mut tx, &"context-session".to_string(), Some(value))
            .await
            .unwrap()
    );
    tx.commit().await.unwrap();
}

fn reading() -> Value {
    let mut value = serde_json::to_value(HarnessSnapshot::initial(0, vec![])).unwrap();
    value["token_usage"] = json!({"used_tokens": 62000, "context_window": 112000,
        "total_tokens": 65000000, "at_ms": 1790035200000_i64});
    value
}

async fn session(fx: &Fx, track: &str, value: Value) {
    let card = fx
        .card(track, "context-planner", "codex", CardRole::Planner)
        .await;
    fx.session(
        &card,
        "context-session",
        WorkerSessionKind::SharedPlanner,
        WorkerSessionState::Running,
        Some("context-thread"),
        Some(value),
        1,
    )
    .await;
}

#[tokio::test]
async fn planner_context_only_writes_changes_and_retains_sleeping_observation() {
    let fx = fx().await;
    let track = fx.track("context").await;
    session(&fx, &track, reading()).await;
    let projector = projector(&fx);
    assert!(projector.recompute_track(&track).await.unwrap());
    let first = payload(&fx, &track).await;
    assert_eq!(
        first["snapshot"]["observedAt"].as_f64(),
        Some(1790035200000.0)
    );
    let items = &first["cell"]["items"];
    assert_eq!(items[0]["value"]["amount"], 50.0);
    assert_eq!(items[1]["value"]["amount"], 62000.0);
    assert_eq!(items[2]["value"]["amount"], 112000.0);
    assert_eq!(items[3]["value"]["text"], "false");
    let mut events = fx.events.subscribe();
    assert!(!projector.recompute_track(&track).await.unwrap());
    assert!(
        events.try_recv().is_err(),
        "unchanged observations emit no event"
    );
    let mut newer = reading();
    newer["token_usage"]["used_tokens"] = json!(82000);
    newer["token_usage"]["at_ms"] = json!(1790035200100_i64);
    persist(&fx, newer).await;
    assert!(projector.recompute_track(&track).await.unwrap());
    let last = payload(&fx, &track).await;
    assert_eq!(last["cell"]["items"][0]["value"]["amount"], 70.0);
    fx.exit_session("context-session", WorkerSessionState::Exited, 100)
        .await;
    assert!(!projector.recompute_track(&track).await.unwrap());
    assert_eq!(payload(&fx, &track).await, last);
}

#[tokio::test]
async fn planner_context_unknown_and_compaction_invalidation() {
    let fx = fx().await;
    let track = fx.track("unknown").await;
    let projector = projector(&fx);
    assert!(projector.recompute_track(&track).await.unwrap());
    let empty = payload(&fx, &track).await;
    assert!(empty["snapshot"]["observedAt"].is_null());
    for item in empty["cell"]["items"].as_array().unwrap().iter().take(4) {
        assert_eq!(item["value"]["state"], "unknown");
    }
    let mut invalid = reading();
    invalid["context_usage_invalidated"] = json!(true);
    session(&fx, &track, invalid).await;
    assert!(projector.recompute_track(&track).await.unwrap());
    let invalid = payload(&fx, &track).await;
    for item in invalid["cell"]["items"].as_array().unwrap().iter().take(3) {
        assert_eq!(item["value"]["state"], "unknown");
    }
    assert_eq!(invalid["cell"]["items"][3]["value"]["text"], "true");
}

#[tokio::test]
async fn planner_context_canonical_matches_historical_reading() {
    let fx = fx().await;
    let track = fx.track("canonical").await;
    session(&fx, &track, reading()).await;
    let projector = projector(&fx);
    projector.recompute_track(&track).await.unwrap();
    let historical = payload(&fx, &track).await;
    let mut canonical =
        serde_json::to_value(calm_types::harness::snapshot::CanonicalHarnessSnapshot::dormant(0))
            .unwrap();
    canonical["token_usage"] = reading()["token_usage"].clone();
    persist(&fx, canonical).await;
    assert!(!projector.recompute_track(&track).await.unwrap());
    assert_eq!(payload(&fx, &track).await, historical);
}

#[tokio::test]
async fn planner_context_counts_only_completed_compact_tasks_on_this_track() {
    let fx = fx().await;
    let track = fx.track("compact").await;
    let other = fx.track("other").await;
    fx.plan_tasks(
        &track,
        &[(
            "ordinary",
            "codex",
            calm_types::task_recovery::TASK_IN_TRACK_ROUTE,
            None,
        )],
    )
    .await;
    fx.plan_tasks(&other, &[]).await;
    for (track_id, keys) in [
        (&track, vec!["compact-done", "compact-pending"]),
        (&other, vec!["compact-other"]),
    ] {
        let (track, card, current) =
            calm_server::track_report::resolve_report_for_track(fx.repo_dyn.as_ref(), track_id)
                .await
                .unwrap();
        let mut body = current.body.clone();
        for key in keys {
            body.push_str(&calm_types::report_blocks::render_fence(
                "task",
                &json!({
                    "key": key, "kind": "compact", "ready": true,
                    "declared_by": calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR,
                }),
            ));
            body.push_str("\n\n");
        }
        let next = calm_types::track_report::TrackReportPayload::new("compact tasks", body);
        calm_server::track_report::persist_report(
            fx.repo_dyn.as_ref(),
            &fx.events,
            &fx.write,
            calm_server::ids::ActorId::KernelDispatcher,
            calm_server::event::EditAuthor::Planner,
            track,
            card,
            current.clone(),
            next,
            current.doc_rev,
            None,
        )
        .await
        .unwrap();
    }
    let card = fx
        .card(&track, "context-worker", "codex", CardRole::Worker)
        .await;
    let other_card = fx
        .card(&other, "other-worker", "codex", CardRole::Worker)
        .await;
    for (track, key, card) in [
        (&track, "compact-done", &card),
        (&track, "ordinary", &card),
        (&other, "compact-other", &other_card),
    ] {
        fx.claim(track, key, 10).await;
        fx.mark_running(track, key, card, 20).await;
        fx.complete(track, key, card, 30).await;
    }
    let projector = projector(&fx);
    assert!(projector.recompute_track(&track).await.unwrap());
    assert_eq!(
        payload(&fx, &track).await["cell"]["items"][4]["value"]["amount"],
        1.0
    );
}

#[tokio::test]
async fn planner_context_boot_and_event_wake_publish_without_reconcile_delay() {
    use calm_server::{
        event::{Event, EventScope},
        harness::HarnessPhaseTag,
    };
    let fx = fx().await;
    let track = fx.track("event-context").await;
    session(&fx, &track, reading()).await;
    let mut events = fx.events.subscribe();
    let task = tokio::spawn(projector(&fx).run());
    async fn next_context(
        events: &mut tokio::sync::broadcast::Receiver<calm_server::event::BroadcastEnvelope>,
    ) -> Value {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Event::OverlaySet(overlay) = events.recv().await.unwrap().event
                    && overlay.kind == PLANNER_CONTEXT_OVERLAY_KIND
                {
                    break overlay.payload;
                }
            }
        })
        .await
        .expect("event wake must beat the 30 s reconcile")
    }
    let first = next_context(&mut events).await;
    assert_eq!(first["cell"]["items"][0]["value"]["amount"], 50.0);
    let mut newer = reading();
    newer["token_usage"]["used_tokens"] = json!(82000);
    persist(&fx, newer).await;
    fx.events.emit_envelope(Fx::envelope(
        EventScope::System,
        Event::HarnessPhaseChanged {
            worker_session_id: "context-session".into(),
            card_id: "context-planner".into(),
            track_id: track.into(),
            old_phase: HarnessPhaseTag::TurnRunning,
            new_phase: HarnessPhaseTag::Idle,
        },
    ));
    let newer = next_context(&mut events).await;
    assert_eq!(newer["cell"]["items"][0]["value"]["amount"], 70.0);
    task.abort();
}
