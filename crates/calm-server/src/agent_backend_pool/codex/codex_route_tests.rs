use super::CodexBackends;
use crate::agent_backend_pool::identity::BackendIsolation;
use crate::db::RepoRead;
use crate::db::{Repo, sqlite::SqlxRepo};
use crate::shared_codex_appserver::native_owner_tests::{
    assert_no_request, connect_with_reader, request, send,
};
use clap::Parser;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// Build real pool instances; only the native endpoints are controlled by fixtures.
pub(crate) async fn pool_for_repo(repo: Arc<SqlxRepo>) -> (tempfile::TempDir, Arc<CodexBackends>) {
    let root = tempfile::tempdir().unwrap();
    let mut cfg = crate::config::Config::parse_from(["calm-server", "--codex-bin", "/bin/false"]);
    cfg.data_dir = Some(root.path().into());
    let config = root.path().join("backends.json");
    std::fs::write(&config, json!({
        "cooldown_after_concurrency_limit_secs":60,"cooldown_default_secs":300,"cooldown_max_secs":3600,
        "backends": (["default", "other"].map(|id| json!({
            "provider":"codex", "id":id, "weight":1, "accepts_new_sessions":true,
            "credentials":{"kind":"codex_home", "path":root.path().join(if id == "default" { "codex-home" } else { id })}
        })))
    }).to_string()).unwrap();
    cfg.agent_backend_config = Some(config);
    let repo: Arc<dyn Repo> = repo;
    let isolation = BackendIsolation::prepare(&cfg, &repo).await.unwrap();
    let pending = Arc::new(
        crate::pending_codex_threads::PendingThreadStartRegistry::new(
            repo.clone(),
            crate::event::EventBus::new(),
        ),
    );
    (root, CodexBackends::build(&cfg, repo, pending, isolation))
}

pub(crate) async fn bind(repo: &SqlxRepo, session: &str, thread: Option<&str>) {
    let mut tx = repo.pool().begin().await.unwrap();
    sqlx::query("UPDATE worker_sessions SET thread_id=NULL WHERE id=?")
        .bind(session)
        .execute(&mut *tx)
        .await
        .unwrap();
    calm_truth::db::sqlite::session_backend::insert_tx(&mut tx, session, "codex", "other")
        .await
        .unwrap();
    sqlx::query("UPDATE worker_sessions SET thread_id=? WHERE id=?")
        .bind(thread)
        .bind(session)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn agent_backend_single_backend_resolution_skips_binding_query() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let shared = crate::shared_codex_appserver::SharedCodexAppServer::new_stub(repo.clone());
    let pool = CodexBackends::single(shared.clone());
    // Closing the database makes any attempted query fail.
    repo.pool().close().await;
    assert!(Arc::ptr_eq(
        pool.for_session("absent").await.unwrap(),
        &shared
    ));
    assert!(Arc::ptr_eq(
        pool.for_thread("absent").await.unwrap(),
        &shared
    ));
}

#[tokio::test]
async fn agent_backend_bound_missing_instance_never_falls_back_to_default() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let (_root, pool) = pool_for_repo(repo.clone()).await;
    assert!(pool.for_session("absent").await.is_err());
    assert!(pool.for_thread("absent").await.is_err());
    let (card, session) = seed(&repo).await;
    bind(&repo, &session, Some("thread")).await;
    let missing = CodexBackends {
        instances: vec![pool.default_only().clone()],
        routing: pool.routing.clone(),
    };
    assert!(missing.for_session(&session).await.is_err());
    assert!(missing.for_thread("thread").await.is_err());
    let _ = card;
}

pub(crate) async fn seed(repo: &SqlxRepo) -> (String, String) {
    use crate::db::prelude::*;
    let area = repo
        .area_create(
            calm_types::organization::LEGACY_OWNER_USER_ID,
            crate::model::NewArea {
                name: "a".into(),
                color: "#abc".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let track = repo
        .track_create(crate::model::NewTrack {
            area_id: area.id,
            title: "t".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let mut tx = repo.pool().begin().await.unwrap();
    let card = calm_truth::db::sqlite::card_create_with_id_tx(
        &mut tx,
        uuid::Uuid::new_v4().to_string(),
        crate::model::NewCard {
            track_id: track.id,
            kind: "codex".into(),
            title: None,
            payload: json!({"task":"task"}),
            sort: None,
        },
        crate::model::CardRole::Worker,
        true,
        repo.card_role_cache(),
    )
    .await
    .unwrap();
    let session = uuid::Uuid::new_v4().to_string();
    use crate::session_projection_repo::{
        AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    calm_truth::db::sqlite::session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: session.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Running,
            terminal_run_id: None,
            thread_id: Some("thread".into()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: 0,
        },
    )
    .await
    .unwrap();
    calm_truth::db::sqlite::session_record_activity_tx(
        &mut tx,
        &calm_types::worker::WorkerSessionId::from(session.as_str()),
        0,
        "idle",
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (card.id.to_string(), session)
}

#[tokio::test]
async fn agent_backend_reaper_default_down_does_not_kill_bound_session() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let (_root, pool) = pool_for_repo(repo.clone()).await;
    let (_, session) = seed(&repo).await;
    bind(&repo, &session, Some("thread")).await;
    let other = pool.instances()[1].clone();
    let (_client, mut peer, _forwarder) = connect_with_reader(&other).await;
    assert!(!pool.default_only().is_running());
    let registry = crate::provider_registry::WorkerProviderRegistry::for_backends(
        &crate::state::DaemonClient::new_stub(),
        pool.clone(),
        crate::harness::HarnessRegistry::new(),
    );
    let provider = registry
        .get(calm_types::worker::WorkerProviderKind::Codex)
        .unwrap();
    let verdict = provider.confirm_durable_death("thread", i64::MAX, 0);
    let serve = async {
        let frame = request(&mut peer).await;
        assert_eq!(frame["method"], "thread/read");
        send(&mut peer, json!({"id":frame["id"],"result":{"thread":{"id":"thread","status":{"type":"active","activeFlags":[]},"turns":[]}}})).await;
        let loaded = request(&mut peer).await;
        assert_eq!(loaded["method"], "thread/loaded/list");
        send(
            &mut peer,
            json!({"id":loaded["id"],"result":{"data":["thread"]}}),
        )
        .await;
    };
    let (verdict, _) = tokio::join!(verdict, serve);
    assert_eq!(verdict, calm_types::worker::DeathVerdict::Alive);
    use crate::db::prelude::*;
    let worker = repo
        .session_get(&session.as_str().into())
        .await
        .unwrap()
        .unwrap();
    other.set_active_turn_for_test("thread", "turn");
    assert!(matches!(
        provider
            .probe_liveness(&worker, &calm_exec::SpawnCtx::new(42))
            .await
            .unwrap(),
        calm_types::worker::Liveness::Alive {
            active_turn_id: Some(_)
        }
    ));
}

#[tokio::test]
async fn agent_backend_idle_probe_reads_bound_instance() {
    use crate::harness::backend::factory::{PlannerFactory, WorkerIdleWake};
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let (_root, pool) = pool_for_repo(repo.clone()).await;
    let (card, session) = seed(&repo).await;
    bind(&repo, &session, Some("thread")).await;
    let (_a, mut default, _fa) = connect_with_reader(pool.default_only()).await;
    let (_b, mut other, _fb) = connect_with_reader(&pool.instances()[1]).await;
    sqlx::query("INSERT INTO tasks
        (id, track_id, key, kind, goal, context_json, depends_on_json, status, created_at_ms,updated_at_ms)
        SELECT 'task',track_id,'task','codex','goal','{}','[]','running',0,0 FROM cards WHERE id=?")
        .bind(&card).execute(repo.pool()).await.unwrap();
    let task = repo.task_get("task").await.unwrap().unwrap();
    let idle = WorkerIdleWake::with_route(pool.clone(), Duration::ZERO, Duration::from_secs(5));
    let observe = PlannerFactory::worker_idle_observation(&idle, repo.pool(), &task, &card);
    let serve = async {
        let frame = request(&mut other).await;
        assert_eq!(frame["method"], "thread/read");
        send(&mut other,json!({"id":frame["id"],"result":{"thread":{"id":"thread","status":{"type":"idle"},"turns":[{"id":"turn","status":"completed","completedAt":1,"items":[]}]}}})).await;
        let loaded = request(&mut other).await;
        assert_eq!(loaded["method"], "thread/loaded/list");
        send(
            &mut other,
            json!({"id":loaded["id"],"result":{"data":["thread"]}}),
        )
        .await;
    };
    let (end, _) = tokio::join!(observe, serve);
    assert!(end.is_some());
    assert_no_request(&mut default).await;
    let missing = Arc::new(CodexBackends {
        instances: vec![pool.default_only().clone()],
        routing: pool.routing.clone(),
    });
    let unresolved = WorkerIdleWake::with_route(missing, Duration::ZERO, Duration::from_secs(5));
    assert!(
        PlannerFactory::worker_idle_observation(&unresolved, repo.pool(), &task, &card)
            .await
            .is_none()
    );
    assert_no_request(&mut default).await;
    assert_no_request(&mut other).await;
}

#[tokio::test]
async fn agent_backend_scoped_feeders_isolate_both_directions() {
    use crate::codex_appserver::Notification;
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let (_root, pool) = pool_for_repo(repo.clone()).await;
    let (_, a) = seed(&repo).await;
    sqlx::query("UPDATE worker_sessions SET thread_id='a',last_activity_ms=1,last_turn_completed_ms=1 WHERE id=?").bind(&a).execute(repo.pool()).await.unwrap();
    let (_, b) = seed(&repo).await;
    bind(&repo, &b, Some("b")).await;
    sqlx::query(
        "UPDATE worker_sessions SET last_activity_ms=1,last_turn_completed_ms=1 WHERE id=?",
    )
    .bind(&b)
    .execute(repo.pool())
    .await
    .unwrap();
    for instance in pool.instances() {
        let (tx, rx) = tokio::sync::broadcast::channel(8);
        let handle = crate::liveness_feeder::spawn_liveness_feeder(
            repo.clone(),
            rx,
            instance.liveness_scope(),
        );
        for thread in ["a", "b"] {
            tx.send(Notification::TurnCompleted {
                thread_id: thread.into(),
                turn: json!({"id":"turn","status":"completed"}),
            })
            .unwrap();
        }
        drop(tx);
        handle.await.unwrap();
        let values: Vec<(String, i64, String, i64)> = sqlx::query_as("SELECT id,last_activity_ms,last_thread_status,last_turn_completed_ms FROM worker_sessions ORDER BY id")
            .fetch_all(repo.pool()).await.unwrap();
        for (id, at, status, done) in values {
            let owned = (instance.backend_id().is_default() && id == a)
                || (!instance.backend_id().is_default() && id == b);
            if owned {
                assert!(at > 1);
                assert_eq!(at, done);
                assert_eq!(status, "idle");
            } else {
                assert_eq!((at, status.as_str(), done), (1, "idle", 1));
            }
        }
        sqlx::query("UPDATE worker_sessions SET last_activity_ms=1,last_turn_completed_ms=1")
            .execute(repo.pool())
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn agent_backend_delete_quiesce_interrupts_only_bound_instance() {
    use crate::state::{AppState, CodexClient, CodexShellState, DaemonClient, RouteState};
    use axum::extract::FromRef;
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let (_root, pool) = pool_for_repo(repo.clone()).await;
    let (card_id, session) = seed(&repo).await;
    bind(&repo, &session, Some("thread")).await;
    let card = repo.card_get(&card_id).await.unwrap().unwrap();
    let (_a, mut default, _fa) = connect_with_reader(pool.default_only()).await;
    let other = pool.instances()[1].clone();
    let (_b, mut peer, _fb) = connect_with_reader(&other).await;
    other.set_active_turn_for_test("thread", "turn");
    let wiring = crate::harness::wiring::PlannerWiring::unconfigured_for_test(repo.clone());
    let state = AppState::from_parts(
        repo.clone(),
        crate::event::EventBus::new(),
        Arc::new(DaemonClient::new_stub()),
        wiring.plugin,
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    )
    .with_shared_codex_appserver(pool.default_only().clone());
    let mut cs = CodexShellState::from_ref(&state);
    cs.codex_backends = pool.clone();
    let route = RouteState::from_ref(&state);
    let delete =
        crate::routes::planner_cards::quiesce_shared_codex_card_active_turn(&route, &cs, &card);
    let serve = async {
        let frame = request(&mut peer).await;
        assert_eq!(frame["method"], "turn/interrupt");
        assert_eq!(frame["params"]["turnId"], "turn");
        send(&mut peer, json!({"id":frame["id"],"result":{}})).await;
    };
    let (result, _) = tokio::join!(delete, serve);
    assert_eq!(result.unwrap(), Some("thread".into()));
    assert_no_request(&mut default).await;
    other.set_active_turn_for_test("thread", "teardown-turn");
    let teardown = crate::routes::planner_cards::interrupt_shared_codex_card_active_turn(
        route.repo.as_ref(),
        &cs,
        &card,
    );
    let serve = async {
        let frame = request(&mut peer).await;
        assert_eq!(frame["method"], "turn/interrupt");
        assert_eq!(frame["params"]["turnId"], "teardown-turn");
        send(&mut peer, json!({"id":frame["id"],"result":{}})).await;
    };
    tokio::join!(teardown, serve);
    assert_no_request(&mut default).await;
    // Strict deletion propagates lookup failure instead of addressing default.
    sqlx::query("DELETE FROM worker_session_backends WHERE worker_session_id=?")
        .bind(&session)
        .execute(repo.pool())
        .await
        .unwrap();
    // A removed instance is still persisted; the resolver cannot silently choose another.
    cs.codex_backends = Arc::new(CodexBackends {
        instances: vec![pool.default_only().clone()],
        routing: pool.routing.clone(),
    });
    // Restore the fixture binding before testing the unavailable configured instance.
    bind(&repo, &session, Some("thread")).await;
    assert!(
        crate::routes::planner_cards::quiesce_shared_codex_card_active_turn(&route, &cs, &card)
            .await
            .is_err()
    );
    assert_no_request(&mut default).await;
}

#[tokio::test]
async fn agent_backend_rollout_thread_path_reads_bound_instance() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let (_root, pool) = pool_for_repo(repo.clone()).await;
    let (_card, session) = seed(&repo).await;
    bind(&repo, &session, Some("thread")).await;
    let (_a, mut default, _fa) = connect_with_reader(pool.default_only()).await;
    let (_b, mut peer, _fb) = connect_with_reader(&pool.instances()[1]).await;
    let driver = crate::worker_flow::WorkerFlowDriver::from_backend_parts(
        repo.clone(),
        pool,
        crate::event::EventBus::new(),
    );
    driver.start_on_boot().await.unwrap();
    let frame = request(&mut peer).await;
    assert_eq!(frame["method"], "thread/read");
    assert_eq!(frame["params"]["threadId"], "thread");
    // A null path closes the real capture source after it reads the selected instance.
    send(
        &mut peer,
        json!({"id":frame["id"],"result":{"thread":{"status":{"type":"idle"},"path":null}}}),
    )
    .await;
    assert_no_request(&mut default).await;
    for stop in driver.task_stop_tokens_for_test().await {
        stop.cancel();
    }
}

#[tokio::test]
async fn agent_backend_teardown_lookup_failure_never_interrupts_default() {
    use crate::state::{AppState, CodexClient, CodexShellState, DaemonClient};
    use axum::extract::FromRef;
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let (_root, pool) = pool_for_repo(repo.clone()).await;
    let (card_id, session) = seed(&repo).await;
    bind(&repo, &session, Some("thread")).await;
    let mut card = repo.card_get(&card_id).await.unwrap().unwrap();
    // Exercise the legacy discriminator fallback when the runtime lookup fails.
    card.payload["codex_source"] = json!("shared");
    let (_a, mut default, _fa) = connect_with_reader(pool.default_only()).await;
    let other = pool.instances()[1].clone();
    let (_b, mut peer, _fb) = connect_with_reader(&other).await;
    other.set_active_turn_for_test("thread", "bound-turn");
    // A colliding native thread makes a wrong-default interrupt observable on the wire.
    pool.default_only()
        .set_active_turn_for_test("thread", "default-turn");
    let wiring = crate::harness::wiring::PlannerWiring::unconfigured_for_test(repo.clone());
    let state = AppState::from_parts(
        repo.clone(),
        crate::event::EventBus::new(),
        Arc::new(DaemonClient::new_stub()),
        wiring.plugin,
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    );
    let mut cs = CodexShellState::from_ref(&state);
    cs.codex_backends = pool.clone();
    // Fail only the entry-point lookup; the instances retain the healthy repository,
    // so legacy teardown would resolve the bound thread again and interrupt default.
    let failing_repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    failing_repo.pool().close().await;
    let teardown = crate::routes::planner_cards::interrupt_shared_codex_card_active_turn(
        &failing_repo,
        &cs,
        &card,
    );
    tokio::time::timeout(Duration::from_secs(5), teardown)
        .await
        .expect("card teardown must finish without a native interrupt reply");
    assert_no_request(&mut default).await;
    assert_no_request(&mut peer).await;
    assert_eq!(
        other.active_turn_id_for_thread("thread").as_deref(),
        Some("bound-turn")
    );
}
