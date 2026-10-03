//! Calendar wakes: entries come from the production tool and REST entry points, the scan runs at a
//! fixed instant, and a real Dispatcher delivers the event to a live Planner harness.
use super::*;
use crate::builtin_plugins::calendar::wake::scan;
use crate::dispatcher::Dispatcher;
use crate::event::Event;
use crate::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessRegistry, HarnessSnapshot, Observation, PlannerHarness,
    PlannerHarnessParams,
};
use crate::shared_codex_appserver::SharedCodexAppServer;
use chrono::{DateTime, Utc};
use std::time::{Duration, Instant};

struct Planner {
    identity: ToolCallIdentity,
    harness: PlannerHarness,
    _dispatcher: Dispatcher,
}

/// A Planner identity with a live harness in the registry a spawned Dispatcher consults.
async fn planner(fx: &Fixture) -> Planner {
    let persisted = serde_json::to_value(HarnessSnapshot::initial(0, vec![])).unwrap();
    let identity = fx
        .identity_with(
            CardRole::Planner,
            WorkerSessionKind::SharedPlanner,
            Some(persisted),
        )
        .await;
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(identity.thread_id.clone());
    let repo: Arc<dyn crate::db::Repo> = fx.repo.clone();
    let harness = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: identity.session_id.clone(),
        track_id: identity.track_id.as_deref().unwrap().into(),
        card_id: identity.card_id.as_str().into(),
        thread_id: Some(identity.thread_id.clone()),
        repo: repo.clone(),
        events: fx.ctx.events.clone(),
        card_role_cache: fx.roles.clone(),
        track_area_cache: fx.areas.clone(),
        backend: SharedCodexAppServer::new_stub(repo.clone()).into(),
        config: HarnessConfig::default(),
        snapshot,
    });
    // Keep delivered observations queued instead of issuing a turn on the stub backend.
    harness
        .force_phase_for_dev(HarnessPhaseTag::TurnRunning)
        .await
        .unwrap();
    let registry = HarnessRegistry::new();
    registry.insert(identity.session_id.clone(), harness.clone());
    let dispatcher = Dispatcher::spawn_with_terminal_renderer_and_harness(
        repo.clone(),
        fx.ctx.events.clone(),
        fx.ctx.write.clone(),
        Arc::new(crate::state::CodexClient::new_stub()),
        Arc::new(crate::state::DaemonClient::new_stub()),
        crate::terminal_renderer::TerminalRendererRegistry::new_with_repo(repo.clone()),
        None,
        registry,
        SharedCodexAppServer::new_stub(repo),
        fx._dir.path().join("workspaces"),
        1,
    );
    Planner {
        identity,
        harness,
        _dispatcher: dispatcher,
    }
}

fn timed(start: &str, end: &str) -> serde_json::Value {
    json!({"title":"Pre-market research","description":"Read the news","schedule":{
        "kind":"timed","start":start,"end":end,"timezone":"Asia/Shanghai"
    }})
}

async fn create(fx: &Fixture, who: &ToolCallIdentity, key: &str, task: serde_json::Value) -> Entry {
    let registry = crate::mcp_server::build_default_registry();
    let create = registry.lookup("calm.calendar.create").unwrap();
    let result = create(
        fx.ctx.clone(),
        who.clone(),
        json!({"idempotency_key": key, "task": task}),
    )
    .await
    .unwrap();
    serde_json::from_value(serde_json::to_value(result).unwrap()["structuredContent"].clone())
        .unwrap()
}

async fn update(
    fx: &Fixture,
    who: &ToolCallIdentity,
    entry: &Entry,
    task: serde_json::Value,
    cancelled: bool,
) -> Entry {
    let registry = crate::mcp_server::build_default_registry();
    let update = registry.lookup("calm.calendar.update").unwrap();
    let result = update(
        fx.ctx.clone(),
        who.clone(),
        json!({"id": entry.id, "expected_version": entry.version, "task": task, "cancelled": cancelled}),
    )
    .await
    .unwrap();
    serde_json::from_value(serde_json::to_value(result).unwrap()["structuredContent"].clone())
        .unwrap()
}

fn at(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value).unwrap().to_utc()
}

async fn wake_events(fx: &Fixture) -> Vec<Event> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT kind, payload FROM events WHERE kind = 'track.wake_requested' ORDER BY id",
    )
    .fetch_all(fx.repo.pool())
    .await
    .unwrap();
    rows.into_iter()
        .map(|(kind, payload)| {
            Event::from_kind_and_payload(&kind, serde_json::from_str(&payload).unwrap()).unwrap()
        })
        .collect()
}

async fn cursor(fx: &Fixture, entry: &Entry) -> Option<serde_json::Value> {
    fx.repo
        .plugin_kv_get(PLUGIN_ID, &format!("fired:{}", entry.id))
        .await
        .unwrap()
}

fn start_ms(value: &str) -> serde_json::Value {
    json!(at(value).timestamp_millis())
}

async fn queued_wakes(harness: &PlannerHarness) -> Vec<Observation> {
    harness
        .pending_queue_for_test()
        .await
        .into_iter()
        .filter(|observation| matches!(observation, Observation::TrackWake { .. }))
        .collect()
}

/// Wait until the Dispatcher delivered `count` wakes, then give a stray extra delivery time to land.
async fn delivered_wakes(harness: &PlannerHarness, count: usize) -> Vec<Observation> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while queued_wakes(harness).await.len() < count {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {count} wakes"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    queued_wakes(harness).await
}

#[tokio::test]
async fn due_track_entry_wakes_its_planner_exactly_once() {
    let fx = Fixture::new().await;
    let planner = planner(&fx).await;
    let entry = create(
        &fx,
        &planner.identity,
        "review",
        timed("2026-10-02T09:00", "2026-10-02T10:00"),
    )
    .await;

    assert_eq!(
        scan(&fx.ctx, at("2026-10-02T08:59:59+08:00"))
            .await
            .unwrap(),
        0
    );
    assert_eq!(cursor(&fx, &entry).await, None, "nothing is due yet");
    assert_eq!(
        scan(&fx.ctx, at("2026-10-02T09:00:20+08:00"))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        scan(&fx.ctx, at("2026-10-02T09:01:00+08:00"))
            .await
            .unwrap(),
        0
    );

    let track = planner.identity.track_id.clone().unwrap();
    let text = "Calendar entry \"Pre-market research\" started at 2026-10-02 09:00 \
                Asia/Shanghai and ends at 2026-10-02 10:00 (on time). \
                Do what this Track scheduled it for.";
    match wake_events(&fx).await.as_slice() {
        [
            Event::TrackWakeRequested {
                track_id,
                source,
                key,
                text: written,
            },
        ] => {
            assert_eq!(
                (track_id.as_str(), source.as_str(), key, written.as_str()),
                (track.as_str(), PLUGIN_ID, &entry.id, text)
            );
        }
        other => panic!("expected exactly one wake event, got {other:?}"),
    }
    let delivered = delivered_wakes(&planner.harness, 1).await;
    assert_eq!(
        delivered.len(),
        1,
        "the Planner is woken once: {delivered:?}"
    );
    assert_eq!(
        delivered[0].to_turn_text(),
        format!("Wake from {PLUGIN_ID} ({}): {text}", entry.id)
    );
    assert!(delivered[0].is_hard_fire());
    // Boot catch-up replays the same single wake for a harness that missed the live push.
    let replayed = crate::harness::catch_up::observations_since(
        fx.repo.as_ref(),
        &track.as_str().into(),
        0,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        replayed
            .into_iter()
            .map(|(_, observation)| observation)
            .collect::<Vec<_>>(),
        delivered
    );
    planner.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn missed_wake_fires_late_only_before_its_end() {
    let fx = Fixture::new().await;
    let planner = planner(&fx).await;
    let open = create(
        &fx,
        &planner.identity,
        "open",
        timed("2026-10-02T09:00", "2026-10-02T10:00"),
    )
    .await;
    let ended = create(
        &fx,
        &planner.identity,
        "ended",
        timed("2026-10-02T07:00", "2026-10-02T08:00"),
    )
    .await;

    // The first scan after a restart: one entry is still running, the other already ended.
    assert_eq!(
        scan(&fx.ctx, at("2026-10-02T09:40:30+08:00"))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        scan(&fx.ctx, at("2026-10-02T09:41:00+08:00"))
            .await
            .unwrap(),
        0
    );
    match wake_events(&fx).await.as_slice() {
        [Event::TrackWakeRequested { key, text, .. }] => {
            assert_eq!(key, &open.id);
            assert!(text.contains("(40 min late)"), "{text}");
        }
        other => panic!("expected only the running entry to wake, got {other:?}"),
    }
    assert_eq!(
        cursor(&fx, &ended).await,
        Some(start_ms("2026-10-02T07:00:00+08:00")),
        "an ended occurrence is handled without a wake"
    );
    assert_eq!(delivered_wakes(&planner.harness, 1).await.len(), 1);
    planner.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn human_cancelled_all_day_and_closed_track_entries_never_wake() {
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let fx = Fixture::new().await;
    let planner = planner(&fx).await;
    let response = fx
        .http_app()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/calendar/tasks")
                .header("content-type", "application/json")
                .header("x-calm-actor", "user")
                .body(Body::from(
                    serde_json::to_vec(&json!({"idempotency_key":"human","task":timed(
                        "2026-10-02T09:00", "2026-10-02T10:00"
                    )}))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let human: Entry =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(human.source_track_id.is_none());
    let cancelled = create(
        &fx,
        &planner.identity,
        "cancelled",
        timed("2026-10-02T09:00", "2026-10-02T10:00"),
    )
    .await;
    let cancelled_task = serde_json::to_value(&cancelled.task).unwrap();
    update(&fx, &planner.identity, &cancelled, cancelled_task, true).await;
    let all_day = create(
        &fx,
        &planner.identity,
        "all-day",
        json!({"title":"All day","description":"","schedule":{"kind":"all_day","date":"2026-10-02"}}),
    )
    .await;
    let other = fx.identity(CardRole::Assistant).await;
    let closed = create(
        &fx,
        &other,
        "closed",
        timed("2026-10-02T09:00", "2026-10-02T10:00"),
    )
    .await;
    sqlx::query("UPDATE tracks SET closed_at = 1 WHERE id = ?")
        .bind(other.track_id.as_deref().unwrap())
        .execute(fx.repo.pool())
        .await
        .unwrap();

    assert_eq!(
        scan(&fx.ctx, at("2026-10-02T09:30:00+08:00"))
            .await
            .unwrap(),
        0
    );
    assert!(wake_events(&fx).await.is_empty());
    assert_eq!(
        cursor(&fx, &human).await,
        None,
        "a human entry is never a wake subject"
    );
    assert_eq!(cursor(&fx, &cancelled).await, None);
    assert_eq!(cursor(&fx, &all_day).await, None);
    assert_eq!(
        cursor(&fx, &closed).await,
        Some(start_ms("2026-10-02T09:00:00+08:00")),
        "a closed Track's occurrence is handled without a wake"
    );
    assert_eq!(queued_wakes(&planner.harness).await, Vec::new());
    planner.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn rescheduled_entry_wakes_again_at_its_new_start() {
    let fx = Fixture::new().await;
    let planner = planner(&fx).await;
    let entry = create(
        &fx,
        &planner.identity,
        "moving",
        timed("2026-10-02T09:00", "2026-10-02T10:00"),
    )
    .await;
    assert_eq!(
        scan(&fx.ctx, at("2026-10-02T09:00:10+08:00"))
            .await
            .unwrap(),
        1
    );
    update(
        &fx,
        &planner.identity,
        &entry,
        timed("2026-10-02T14:00", "2026-10-02T15:00"),
        false,
    )
    .await;
    assert_eq!(
        scan(&fx.ctx, at("2026-10-02T13:59:00+08:00"))
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        scan(&fx.ctx, at("2026-10-02T14:00:10+08:00"))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        scan(&fx.ctx, at("2026-10-02T14:01:00+08:00"))
            .await
            .unwrap(),
        0
    );
    let starts: Vec<_> = wake_events(&fx)
        .await
        .into_iter()
        .map(|event| match event {
            Event::TrackWakeRequested { text, .. } => text,
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(starts.len(), 2, "{starts:?}");
    assert!(
        starts[1].contains("started at 2026-10-02 14:00"),
        "{starts:?}"
    );
    assert_eq!(delivered_wakes(&planner.harness, 2).await.len(), 2);

    // Stopping Calendar pauses scanning; a due entry waits for it to run again.
    let later = create(
        &fx,
        &planner.identity,
        "later",
        timed("2026-10-02T16:00", "2026-10-02T17:00"),
    )
    .await;
    fx.host.stop(PLUGIN_ID).await.unwrap();
    assert_eq!(
        scan(&fx.ctx, at("2026-10-02T16:00:10+08:00"))
            .await
            .unwrap(),
        0
    );
    assert_eq!(cursor(&fx, &later).await, None);
    planner.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn edit_landing_after_the_scan_read_wins_over_the_stale_entry() {
    let fx = Fixture::new().await;
    let planner = fx.identity(CardRole::Planner).await;
    let read_by_scan = create(
        &fx,
        &planner,
        "raced",
        timed("2026-10-02T09:00", "2026-10-02T10:00"),
    )
    .await;
    // The owner cancels after the scan listed version 1 but before its wake transaction.
    let task = serde_json::to_value(&read_by_scan.task).unwrap();
    update(&fx, &planner, &read_by_scan, task, true).await;
    assert!(
        !crate::builtin_plugins::calendar::wake::handle(
            &fx.ctx,
            read_by_scan.clone(),
            at("2026-10-02T09:00:10+08:00"),
        )
        .await
        .unwrap()
    );
    assert!(wake_events(&fx).await.is_empty());
    assert_eq!(cursor(&fx, &read_by_scan).await, None);
}

#[tokio::test]
async fn entry_shorter_than_the_scan_interval_wakes_once() {
    let fx = Fixture::new().await;
    let planner = fx.identity(CardRole::Planner).await;
    let brief = create(
        &fx,
        &planner,
        "brief",
        timed("2026-10-02T09:00:05+08:00", "2026-10-02T09:00:15+08:00"),
    )
    .await;
    for (now, woken) in [
        ("2026-10-02T09:00:00+08:00", 0),
        ("2026-10-02T09:00:30+08:00", 1),
        ("2026-10-02T09:01:00+08:00", 0),
    ] {
        assert_eq!(
            scan(&fx.ctx, at(now)).await.unwrap(),
            woken,
            "scan at {now}"
        );
    }
    match wake_events(&fx).await.as_slice() {
        [Event::TrackWakeRequested { key, .. }] => assert_eq!(key, &brief.id),
        other => panic!("expected exactly one wake, got {other:?}"),
    }
}

#[tokio::test]
async fn end_extended_after_the_scan_read_is_not_recorded_as_expired() {
    let fx = Fixture::new().await;
    let planner = fx.identity(CardRole::Planner).await;
    let read_by_scan = create(
        &fx,
        &planner,
        "extended",
        timed("2026-10-02T09:00", "2026-10-02T10:00"),
    )
    .await;
    // The owner extends the entry after the scan listed version 1 but before its cursor write.
    update(
        &fx,
        &planner,
        &read_by_scan,
        timed("2026-10-02T09:00", "2026-10-02T12:00"),
        false,
    )
    .await;
    let now = at("2026-10-02T10:30:00+08:00");
    assert!(
        !crate::builtin_plugins::calendar::wake::handle(&fx.ctx, read_by_scan.clone(), now)
            .await
            .unwrap()
    );
    assert_eq!(cursor(&fx, &read_by_scan).await, None);
    assert_eq!(
        scan(&fx.ctx, now).await.unwrap(),
        1,
        "the extended entry still wakes"
    );
    assert_eq!(scan(&fx.ctx, now).await.unwrap(), 0);
    assert_eq!(wake_events(&fx).await.len(), 1);
}

#[tokio::test]
async fn overlapping_handles_of_one_stale_read_wake_once() {
    let fx = Fixture::new().await;
    let planner = fx.identity(CardRole::Planner).await;
    let read_by_scan = create(
        &fx,
        &planner,
        "overlap",
        timed("2026-10-02T09:00", "2026-10-02T10:00"),
    )
    .await;
    let now = at("2026-10-02T09:00:10+08:00");
    // Both read the unhandled cursor before either transaction commits.
    let (first, second) = tokio::join!(
        crate::builtin_plugins::calendar::wake::handle(&fx.ctx, read_by_scan.clone(), now),
        crate::builtin_plugins::calendar::wake::handle(&fx.ctx, read_by_scan.clone(), now),
    );
    assert_eq!(
        [first.unwrap(), second.unwrap()]
            .iter()
            .filter(|woke| **woke)
            .count(),
        1
    );
    assert_eq!(wake_events(&fx).await.len(), 1);
}
