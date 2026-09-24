#![cfg(feature = "fixtures")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::actor::actor_middleware;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::codex_appserver::InputItem;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::dispatcher::Dispatcher;
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessRegistry, HarnessSnapshot, HookKind, Observation,
    PlannerHarness, PlannerHarnessParams,
};
use calm_server::ids::{ActorId, AreaId, CardId, TrackId};
use calm_server::model::{
    CardRole, NewArea, NewCard, NewTrack, Task, TaskKind, TaskStatus, new_id, now_ms,
};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::track_area_cache::TrackAreaCache;
use calm_types::event::{ChannelVerdict, ChannelVerdictKind, ReviewSubject};
use serde_json::{Value, json};
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    repo: Arc<dyn Repo>,
    repo_sqlx: Arc<SqlxRepo>,
    events: EventBus,
    card_role_cache: CardRoleCache,
    track_area_cache: TrackAreaCache,
    area_id: AreaId,
    track_id: TrackId,
    planner_card_id: CardId,
    worker_card_id: CardId,
    runtime_id: String,
    harness: PlannerHarness,
    harness_registry: HarnessRegistry,
    codex: Arc<CodexClient>,
    daemon: Arc<DaemonClient>,
    renderer: Arc<TerminalRendererRegistry>,
    shared: Arc<SharedCodexAppServer>,
}

async fn boot() -> Boot {
    let repo_sqlx = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let repo: Arc<dyn Repo> = repo_sqlx.clone();
    let area = repo
        .area_create(NewArea {
            name: "planner-wake-worker-stop".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "planner wake on worker stop".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let planner_card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .unwrap();
    let worker_card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();

    let card_role_cache = CardRoleCache::new();
    card_role_cache.insert(planner_card.id.clone(), CardRole::Planner, track.id.clone());
    crate::support::mcp::set_persisted_card_role(
        repo.as_ref(),
        planner_card.id.as_str(),
        CardRole::Planner,
    )
    .await;
    card_role_cache.insert(worker_card.id.clone(), CardRole::Worker, track.id.clone());
    let track_area_cache = TrackAreaCache::new();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();

    let events = EventBus::new();
    let codex = Arc::new(CodexClient::new_stub());
    let daemon = Arc::new(DaemonClient::new_stub());
    let plugin_host = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        repo.clone(),
        std::path::PathBuf::new(),
        std::env::temp_dir().join(format!("calm-plugins-data-planner-wake-{}", new_id())),
        Vec::new(),
        events.clone(),
        calm_server::state::WriteContext::new(card_role_cache.clone(), track_area_cache.clone()),
    ));
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        daemon.clone(),
        plugin_host,
        codex.clone(),
        Some(card_role_cache.clone()),
        Some(track_area_cache.clone()),
    );
    let app = axum::Router::new()
        .merge(routes::router())
        .layer(axum::middleware::from_fn(actor_middleware))
        .with_state(state);

    let runtime_id = new_id();
    let thread_id = "planner-thread-existing".to_string();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Resumed;
    snapshot.last_thread_id = Some(thread_id.clone());
    let mut tx = repo_sqlx.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: planner_card.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(thread_id.clone()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let shared = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let harness = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: runtime_id.clone(),
        track_id: track.id.clone(),
        card_id: planner_card.id.clone(),
        thread_id: Some(thread_id),
        repo: repo.clone(),
        events: events.clone(),
        card_role_cache: card_role_cache.clone(),
        track_area_cache: track_area_cache.clone(),
        backend: shared.clone().into(),
        config: HarnessConfig::default(),
        snapshot,
    });
    let harness_registry = HarnessRegistry::new();
    harness_registry.insert(runtime_id.clone(), harness.clone());

    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let renderer = TerminalRendererRegistry::new_with_repo(route_repo);

    Boot {
        app,
        repo,
        repo_sqlx,
        events,
        card_role_cache,
        track_area_cache,
        area_id: area.id,
        track_id: track.id,
        planner_card_id: planner_card.id,
        worker_card_id: worker_card.id,
        runtime_id,
        harness,
        harness_registry,
        codex,
        daemon,
        renderer,
        shared,
    }
}

/// A gated tasks row on the boot track, bound to `worker_card_id` when given, in `status`.
async fn seed_task(
    boot: &Boot,
    key: &str,
    worker_card_id: Option<&CardId>,
    status: TaskStatus,
) -> Task {
    let task = Task {
        id: format!("{}:{key}", boot.track_id),
        track_id: boot.track_id.to_string(),
        key: key.into(),
        kind: TaskKind::Codex,
        goal: "g".into(),
        context_json: "null".into(),
        acceptance_criteria: None,
        cwd: None,
        depends_on_json: "[]".into(),
        priority: 0,
        gate_json: Some("{\"steps\":[{\"name\":\"t\",\"cmd\":\"true\"}]}".into()),
        status,
        status_detail: None,
        worker_card_id: worker_card_id.map(ToString::to_string),
        gate_result_json: None,
        gate_attempt: 1,
        gate_pid: None,
        gate_pid_starttime: None,
        gate_pid_boot_id: None,
        running_deadline_ms: None,
        context_stale_at_ms: None,
        declared_by: calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR.into(),
        spawn: calm_types::task_recovery::TASK_IN_TRACK_ROUTE.into(),
        created_at_ms: 1,
        updated_at_ms: 1,
        finished_at_ms: None,
    };
    let mut tx = boot.repo_sqlx.pool().begin().await.unwrap();
    crate::support::task::insert_task_tx(&mut tx, &task)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    task
}

/// The positive control for the negative tests below: a `task.gate_result` is always a wake.
async fn emit_gate_result(boot: &Boot, task: &Task) {
    boot.repo
        .log_pure_event(
            ActorId::KernelDispatcher,
            EventScope::Track {
                track: boot.track_id.clone(),
                area: boot.area_id.clone(),
            },
            None,
            &boot.events,
            &boot.card_role_cache,
            &boot.track_area_cache,
            Event::TaskGateResult {
                task_id: task.id.clone(),
                idempotency_key: task.id.clone(),
                passed: true,
                failing_step: None,
                exit_code: Some(0),
                log_tail: "ok\n".into(),
                log_path: "/tmp/gate.log".into(),
                attempt: 1,
                agent_message: None,
                status_detail: None,
                target: None,
            },
        )
        .await
        .expect("persist task.gate_result event");
}

/// A worker card on the boot track, role `Worker` in the dispatcher's cache.
async fn add_worker_card(boot: &Boot) -> CardId {
    let card = boot
        .repo
        .card_create(NewCard {
            track_id: boot.track_id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();
    boot.card_role_cache
        .insert(card.id.clone(), CardRole::Worker, boot.track_id.clone());
    card.id
}

fn stop_hook_payload(session_id: &str) -> Value {
    json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "transcript_path": "/tmp/x.jsonl",
        "transcript_size_bytes": 0,
    })
}

fn spawn_dispatcher(boot: &Boot) -> Dispatcher {
    Dispatcher::spawn_with_terminal_renderer_and_harness(
        boot.repo.clone(),
        boot.events.clone(),
        calm_server::state::WriteContext::new(
            boot.card_role_cache.clone(),
            boot.track_area_cache.clone(),
        ),
        boot.codex.clone(),
        boot.daemon.clone(),
        boot.renderer.clone(),
        None,
        boot.harness_registry.clone(),
        boot.shared.clone(),
        // Attached fixtures: materialization on lease is a no-op.
        std::env::temp_dir().join("neige-calm-test-unused-workspace-root"),
        4,
    )
}

async fn post_hook(
    app: &axum::Router,
    card_id: &CardId,
    payload: Value,
) -> axum::response::Response {
    let uri = format!("/internal/codex/hook?card_id={card_id}");
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn wait_for_worker_hook_stop(harness: &PlannerHarness, card: &CardId) -> Vec<Observation> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let pending = harness.pending_queue_for_test().await;
        if pending.iter().any(
            |obs| matches!(obs, Observation::WorkerHookStop { card_id, .. } if card_id == card),
        ) {
            return pending;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for WorkerHookStop of {card}; pending={pending:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_turn_text_containing(shared: &SharedCodexAppServer, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let turns = shared.started_turns_for_test();
        for (_thread_id, items) in &turns {
            assert_eq!(items.len(), 1);
            // A non-text item here would mean the wake path grew one — not silently stepped over.
            let InputItem::Text { text } = &items[0] else {
                panic!("the wake path issues one text item, got {:?}", items[0]);
            };
            if text.contains(needle) {
                return text.clone();
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for started turn containing {needle:?}; turns={turns:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn worker_codex_stop_hook_reaches_planner_harness_observation_queue() {
    let boot = boot().await;
    let _dispatcher = spawn_dispatcher(&boot);

    let payload = json!({
        "hook_event_name": "Stop",
        "session_id": "worker-session",
        "transcript_path": "/tmp/x.jsonl",
        "transcript_size_bytes": 0,
    });
    let resp = post_hook(&boot.app, &boot.worker_card_id, payload).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let pending = wait_for_worker_hook_stop(&boot.harness, &boot.worker_card_id).await;
    let worker_stop_observations = pending
        .iter()
        .filter(|obs| matches!(obs, Observation::WorkerHookStop { .. }))
        .collect::<Vec<_>>();
    assert_eq!(
        worker_stop_observations.len(),
        1,
        "expected exactly one WorkerHookStop, pending_queue={pending:?}"
    );

    match worker_stop_observations[0] {
        Observation::WorkerHookStop {
            track_id,
            card_id,
            kind,
            idempotency_key,
        } => {
            assert_eq!(track_id, &boot.track_id);
            assert_eq!(card_id, &boot.worker_card_id);
            assert_eq!(kind, &HookKind::CodexStop);
            assert!(!idempotency_key.is_empty());
        }
        other => panic!("expected WorkerHookStop, got {other:?}"),
    }

    assert_ne!(boot.worker_card_id, boot.planner_card_id);
    assert!(boot.harness_registry.get(&boot.runtime_id).is_some());
    boot.harness.shutdown().await.unwrap();
}

/// The wait is the harness's own `debounce_max_wait` plus a margin — had the event been
/// queued, the hard-fire turn would have been issued well inside it.
#[tokio::test]
async fn live_review_round_event_does_not_reach_the_planner_harness_or_issue_a_turn() {
    let boot = boot().await;
    let _dispatcher = spawn_dispatcher(&boot);
    tokio::time::sleep(Duration::from_millis(50)).await;

    boot.repo
        .log_pure_event(
            ActorId::AiPlanner(boot.planner_card_id.clone()),
            EventScope::Track {
                track: boot.track_id.clone(),
                area: boot.area_id.clone(),
            },
            None,
            &boot.events,
            &boot.card_role_cache,
            &boot.track_area_cache,
            Event::ReviewRound {
                track_id: boot.track_id.clone(),
                subject: ReviewSubject {
                    phase: "impl".into(),
                    slice_id: "5b".into(),
                    pr_number: Some(760),
                },
                head_sha: Some("head-sha".into()),
                n: 1,
                cap: 8,
                converged: false,
                channels: vec![
                    ChannelVerdict {
                        role: "design-correctness".into(),
                        verdict: ChannelVerdictKind::ChangesRequested,
                    },
                    ChannelVerdict {
                        role: "failure-path".into(),
                        verdict: ChannelVerdictKind::Approved,
                    },
                ],
                root_cause: Some("tests failing".into()),
                idempotency_key: format!("review.round:{}:impl:5b:760:1", boot.track_id),
            },
        )
        .await
        .expect("persist review.round event");

    tokio::time::sleep(HarnessConfig::default().debounce_max_wait + Duration::from_secs(1)).await;
    let pending = boot.harness.pending_queue_for_test().await;
    assert!(
        !pending
            .iter()
            .any(|obs| matches!(obs, Observation::ReviewRound { .. })),
        "review.round must not be queued; pending={pending:?}"
    );
    assert!(
        boot.shared.started_turns_for_test().is_empty(),
        "review.round must not issue a turn; turns={:?}",
        boot.shared.started_turns_for_test()
    );

    // Positive control.
    let task = seed_task(&boot, "gate", None, TaskStatus::Verifying).await;
    emit_gate_result(&boot, &task).await;
    let text = wait_for_turn_text_containing(&boot.shared, "gate passed").await;
    assert!(!text.contains("Review round"), "turn text={text}");
    boot.harness.shutdown().await.unwrap();
}

/// A worker stop hook is a wake only while the card's tasks row is still `dispatched | running`;
/// a `verifying` row means the gate result is the wake.
#[tokio::test]
async fn live_worker_stop_hook_past_running_does_not_reach_the_planner_harness() {
    let boot = boot().await;
    let _dispatcher = spawn_dispatcher(&boot);

    let stale_worker = boot.worker_card_id.clone();
    seed_task(&boot, "verify", Some(&stale_worker), TaskStatus::Verifying).await;
    let running_worker = add_worker_card(&boot).await;
    seed_task(&boot, "run", Some(&running_worker), TaskStatus::Running).await;

    let resp = post_hook(&boot.app, &stale_worker, stop_hook_payload("stale-session")).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let resp = post_hook(
        &boot.app,
        &running_worker,
        stop_hook_payload("running-session"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let pending = wait_for_worker_hook_stop(&boot.harness, &running_worker).await;
    // Give the stale hook a further grace window before concluding it was suppressed, not late.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let pending_after = boot.harness.pending_queue_for_test().await;
    for queue in [&pending, &pending_after] {
        assert!(
            !queue.iter().any(
                |obs| matches!(obs, Observation::WorkerHookStop { card_id, .. } if card_id == &stale_worker)
            ),
            "stop hook of a verifying task's worker must not be queued; pending={queue:?}"
        );
        assert_eq!(
            queue
                .iter()
                .filter(|obs| matches!(obs, Observation::WorkerHookStop { .. }))
                .count(),
            1,
            "exactly the running worker's stop hook is queued; pending={queue:?}"
        );
    }
    boot.harness.shutdown().await.unwrap();
}
