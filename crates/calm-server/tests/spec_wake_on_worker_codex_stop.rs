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
    SpecHarness, SpecHarnessParams,
};
use calm_server::ids::{ActorId, CardId, CoveId, WaveId};
use calm_server::model::{CardRole, NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::wave_cove_cache::WaveCoveCache;
use calm_types::event::{ChannelVerdict, ChannelVerdictKind, ReviewSubject};
use serde_json::{Value, json};
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    repo: Arc<dyn Repo>,
    events: EventBus,
    card_role_cache: CardRoleCache,
    wave_cove_cache: WaveCoveCache,
    cove_id: CoveId,
    wave_id: WaveId,
    spec_card_id: CardId,
    worker_card_id: CardId,
    runtime_id: String,
    harness: SpecHarness,
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
    let cove = repo
        .cove_create(NewCove {
            name: "spec-wake-worker-stop".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "spec wake on worker stop".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let spec_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .unwrap();
    let worker_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();

    let card_role_cache = CardRoleCache::new();
    card_role_cache.insert(spec_card.id.clone(), CardRole::Spec, wave.id.clone());
    card_role_cache.insert(worker_card.id.clone(), CardRole::Worker, wave.id.clone());
    let wave_cove_cache = WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();

    let events = EventBus::new();
    let codex = Arc::new(CodexClient::new_stub());
    let daemon = Arc::new(DaemonClient::new_stub());
    let plugin_host = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        repo.clone(),
        std::path::PathBuf::new(),
        std::env::temp_dir().join(format!("calm-plugins-data-spec-wake-{}", new_id())),
        Vec::new(),
        events.clone(),
        calm_server::state::WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone()),
    ));
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        daemon.clone(),
        plugin_host,
        codex.clone(),
        Some(card_role_cache.clone()),
        Some(wave_cove_cache.clone()),
    );
    let app = axum::Router::new()
        .merge(routes::router())
        .layer(axum::middleware::from_fn(actor_middleware))
        .with_state(state);

    let runtime_id = new_id();
    let thread_id = "spec-thread-existing".to_string();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Resumed;
    snapshot.last_thread_id = Some(thread_id.clone());
    let mut tx = repo_sqlx.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: spec_card.id.to_string(),
            kind: WorkerSessionKind::SharedSpec,
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
    let harness = SpecHarness::run(SpecHarnessParams {
        runtime_id: runtime_id.clone(),
        wave_id: wave.id.clone(),
        card_id: spec_card.id.clone(),
        thread_id: Some(thread_id),
        repo: repo.clone(),
        events: events.clone(),
        card_role_cache: card_role_cache.clone(),
        wave_cove_cache: wave_cove_cache.clone(),
        daemon: shared.clone(),
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
        events,
        card_role_cache,
        wave_cove_cache,
        cove_id: cove.id,
        wave_id: wave.id,
        spec_card_id: spec_card.id,
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

fn spawn_dispatcher(boot: &Boot) -> Dispatcher {
    Dispatcher::spawn_with_terminal_renderer_and_harness(
        boot.repo.clone(),
        boot.events.clone(),
        calm_server::state::WriteContext::new(
            boot.card_role_cache.clone(),
            boot.wave_cove_cache.clone(),
        ),
        boot.codex.clone(),
        boot.daemon.clone(),
        boot.renderer.clone(),
        None,
        boot.harness_registry.clone(),
        boot.shared.clone(),
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

async fn wait_for_worker_hook_stop(harness: &SpecHarness) -> Vec<Observation> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let pending = harness.pending_queue_for_test().await;
        if pending
            .iter()
            .any(|obs| matches!(obs, Observation::WorkerHookStop { .. }))
        {
            return pending;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for WorkerHookStop"
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
            let InputItem::Text { text } = &items[0];
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
async fn worker_codex_stop_hook_reaches_spec_harness_observation_queue() {
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

    let pending = wait_for_worker_hook_stop(&boot.harness).await;
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
            wave_id,
            card_id,
            kind,
            idempotency_key,
        } => {
            assert_eq!(wave_id, &boot.wave_id);
            assert_eq!(card_id, &boot.worker_card_id);
            assert_eq!(kind, &HookKind::CodexStop);
            assert!(!idempotency_key.is_empty());
        }
        other => panic!("expected WorkerHookStop, got {other:?}"),
    }

    assert_ne!(boot.worker_card_id, boot.spec_card_id);
    assert!(boot.harness_registry.get(&boot.runtime_id).is_some());
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn live_review_round_event_reaches_spec_harness_and_issues_turn() {
    let boot = boot().await;
    let _dispatcher = spawn_dispatcher(&boot);
    tokio::time::sleep(Duration::from_millis(50)).await;

    boot.repo
        .log_pure_event(
            ActorId::AiSpec(boot.spec_card_id.clone()),
            EventScope::Wave {
                wave: boot.wave_id.clone(),
                cove: boot.cove_id.clone(),
            },
            None,
            &boot.events,
            &boot.card_role_cache,
            &boot.wave_cove_cache,
            Event::ReviewRound {
                wave_id: boot.wave_id.clone(),
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
                idempotency_key: format!("review.round:{}:impl:5b:760:1", boot.wave_id),
            },
        )
        .await
        .expect("persist review.round event");

    let text = wait_for_turn_text_containing(&boot.shared, "Review round 1/8").await;
    assert!(text.contains("Review round 1/8"), "turn text={text}");
    assert!(text.contains("converged=false"), "turn text={text}");
}
