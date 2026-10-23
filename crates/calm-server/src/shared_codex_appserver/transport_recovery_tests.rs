//! Scripted app-server connections; production transport, replay and native owner.
use super::*;
use futures_util::{SinkExt, StreamExt};
use provider::codex::RequestCrossing;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::AtomicUsize;
use tokio::net::UnixListener;

type Seen = Arc<StdMutex<Vec<(usize, String, String)>>>;
struct Fixture {
    daemon: Arc<SharedCodexAppServer>,
    seen: Seen,
    closed: Arc<StdMutex<HashSet<String>>>,
    listener: JoinHandle<()>,
    requests: Arc<StdMutex<Vec<Value>>>,
    pause_resume: Arc<AtomicBool>,
    answers: Arc<StdMutex<HashMap<String, Value>>>,
    resume_release: Arc<tokio::sync::Notify>,
    _root: tempfile::TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.listener.abort();
    }
}
async fn fixture() -> Fixture {
    use clap::Parser;
    let root = tempfile::tempdir().unwrap();
    let repo = Arc::new(
        crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap(),
    );
    let cfg = Config::parse_from(["calm-server", "--data-dir", root.path().to_str().unwrap()]);
    let home = Arc::new(SharedCodexHome::new(root.path().join("codex-home")));
    let daemon = SharedCodexAppServer::new(&cfg, home, repo);
    std::fs::create_dir_all(daemon.sock.parent().unwrap()).unwrap();
    let socket = UnixListener::bind(&daemon.sock).unwrap();
    let seen = Seen::default();
    let closed = Arc::new(StdMutex::new(HashSet::new()));
    let count = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(StdMutex::new(Vec::new()));
    let answers = Arc::new(StdMutex::new(HashMap::<String, Value>::new()));
    let pause_resume = Arc::new(AtomicBool::new(false));
    let resume_release = Arc::new(tokio::sync::Notify::new());
    let listener = tokio::spawn({
        let answers = answers.clone();
        let seen = seen.clone();
        let closed = closed.clone();
        let requests = requests.clone();
        let pause_resume = pause_resume.clone();
        let resume_release = resume_release.clone();
        async move {
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                let (stream, _) = socket.accept().await.unwrap();
                let connection = count.fetch_add(1, Ordering::SeqCst);
                let answers = answers.clone();
                let seen = seen.clone();
                let closed = closed.clone();
                let requests = requests.clone();
                let pause_resume = pause_resume.clone();
                let resume_release = resume_release.clone();
                tasks.spawn(async move {
                    let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                    while let Some(Ok(message)) = ws.next().await {
                        let Ok(raw) = message.to_text() else { continue; };
                        let Ok(request) = serde_json::from_str::<Value>(raw) else { continue; };
                        let method = request["method"].as_str().unwrap_or_default().to_owned();
                        let thread = request["params"]["threadId"].as_str().unwrap_or_default().to_owned();
                        seen.lock().unwrap().push((connection, method.clone(), thread.clone()));
                        requests.lock().unwrap().push(request.clone());
                        if method == "thread/resume" && pause_resume.load(Ordering::SeqCst) {
                            resume_release.notified().await;
                        }
                        if closed.lock().unwrap().contains(&method) || (method == "thread/resume" && closed.lock().unwrap().contains(&thread)) {
                            let _ = ws.close(None).await; break;
                        }
                        let Some(id) = request.get("id") else { continue; };
                        let answer = answers.lock().unwrap().get(&method).cloned();
                        if let Some(mut answer) = answer {
                            answer["id"] = id.clone();
                            if ws.send(tokio_tungstenite::tungstenite::Message::Text(answer.to_string())).await.is_err() {break;}
                            continue;
                        }
                        let result = match method.as_str() {
                            "initialize" => json!({"userAgent":"fixture"}),
                            "model/list" => json!({"data":[],"nextCursor":null}),
                            "thread/loaded/list" => json!({"data":[]}),
                            "thread/read" => json!({"thread":{"id":thread,"status":{"type":"idle"},"turns":[]}}),
                            "thread/resume" | "thread/start" => json!({"thread":{"id":if thread.is_empty() {"worker"} else {&thread}}}),
                            "thread/unsubscribe" => json!({"status":"unsubscribed"}),
                            "turn/start" => json!({"turn":{"id":"fresh-turn"}}),
                            _ => json!({}),
                        };
                        if ws.send(tokio_tungstenite::tungstenite::Message::Text(json!({"id":id,"result":result}).to_string())).await.is_err() {break;}
                    }
                });
            }
        }
    });
    let (client, notifications) = connect_initialized(&daemon.sock, &daemon.approval_routes)
        .await
        .unwrap();
    let client = Arc::new(client);
    drop(
        daemon
            .install_client(client.clone(), notifications)
            .await
            .unwrap(),
    );
    native_owner_tests::publish(&daemon, client).await;
    Fixture {
        daemon,
        seen,
        closed,
        listener,
        requests,
        pause_resume,
        answers,
        resume_release,
        _root: root,
    }
}

#[tokio::test]
async fn next_request_reconnects_without_resending_unknown_turn() {
    let f = fixture().await;
    f.closed.lock().unwrap().insert("turn/start".into());
    let original = f.daemon.connected_client().await.unwrap();
    let failure = original
        .turn_start_attempt_gated(
            "thread",
            vec![InputItem::text("one input")],
            &TurnModelSelection::inherit(),
            TurnApprovals::Explicit(crate::planner_permission_mode::PlannerPermissionMode::Never),
            None,
            &provider::send_boundary::PreSendGate::new(|| Box::pin(async { Ok(()) })),
        )
        .await
        .unwrap_err();
    assert_eq!(failure.crossing(), RequestCrossing::AfterSend);
    native_owner_tests::reader_finished(&original).await;
    f.closed.lock().unwrap().clear();
    // Concurrent callers must share one newly initialized connection.
    let (a, b) = tokio::join!(f.daemon.connected_client(), f.daemon.connected_client());
    let (a, b) = (a.unwrap(), b.unwrap());
    assert!(
        !Arc::ptr_eq(&a, &original),
        "a dead client must be replaced"
    );
    assert!(Arc::ptr_eq(&a, &b));
    f.daemon
        .model_list(tokio::time::Instant::now() + Duration::from_secs(2))
        .await
        .unwrap();
    let seen = f.seen.lock().unwrap();
    assert_eq!(
        seen.iter().filter(|(_, m, _)| m == "turn/start").count(),
        1,
        "unknown input must never be resent"
    );
    assert_eq!(seen.iter().filter(|(_, m, _)| m == "initialize").count(), 2);
    assert_eq!(
        f.daemon.status_snapshot().restart_count,
        0,
        "reconnect must preserve the process"
    );
}

#[tokio::test]
async fn failed_startup_resume_does_not_block_other_threads_or_new_work() {
    let f = fixture().await;
    for thread in ["before", "broken", "after"] {
        f.daemon
            .thread_cache
            .insert(thread.into(), format!("card-{thread}"));
    }
    f.closed.lock().unwrap().insert("broken".into());
    {
        let mut core = f.daemon.core.lock().await;
        let SupervisorState::Running { replay, .. } = &mut core.state else {
            panic!("running fixture")
        };
        *replay = ConnectionReplay::Pending(ReplayPlan::new(ResumeMode::HotTakeover));
    }
    f.daemon.resume_cached_threads().await;
    f.daemon
        .model_list(tokio::time::Instant::now() + Duration::from_secs(2))
        .await
        .expect("catalog remains usable after one broken startup history");
    let client = f.daemon.connected_client().await.unwrap();
    client
        .thread_start(None)
        .await
        .expect("worker mint remains usable");
    // The deletion cleanup uses the recovered connection and drops only its mapping.
    f.daemon
        .forget_threads_for_deleted_cards(&HashSet::from(["card-broken".into()]))
        .await;
    assert!(!f.daemon.thread_cache.contains_key("broken"));
    let seen = f.seen.lock().unwrap();
    let final_connection = seen.last().unwrap().0;
    for thread in ["before", "after"] {
        assert!(
            seen.iter()
                .any(|(c, m, t)| *c == final_connection && m == "thread/resume" && t == thread),
            "all healthy subscriptions must be restored on the final connection: {seen:?}"
        );
    }
    assert_eq!(
        seen.iter()
            .filter(|(_, m, t)| m == "thread/resume" && t == "broken")
            .count(),
        1,
        "automatic recovery must not loop on a broken history"
    );
}

#[tokio::test]
async fn cancelled_subscription_recovery_is_finished_by_the_next_caller() {
    let f = fixture().await;
    let original = f.daemon.connected_client().await.unwrap();
    f.closed.lock().unwrap().insert("model/list".into());
    assert!(
        original
            .model_list(None, tokio::time::Instant::now() + Duration::from_secs(2))
            .await
            .is_err()
    );
    native_owner_tests::reader_finished(&original).await;
    f.closed.lock().unwrap().clear();
    f.daemon
        .thread_cache
        .insert("restore".into(), "card-restore".into());
    let fence = f.daemon.resume_replay_serial.lock().await;
    let daemon = f.daemon.clone();
    let recovering = tokio::spawn(async move { daemon.connected_client().await });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let current = f.daemon.running_client().await;
            if current
                .as_ref()
                .is_some_and(|client| !Arc::ptr_eq(client, &original))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    recovering.abort();
    assert!(matches!(recovering.await, Err(error) if error.is_cancelled()));
    let daemon = f.daemon.clone();
    let resumed = tokio::spawn(async move { daemon.connected_client().await });
    drop(fence);
    let recovered = tokio::time::timeout(Duration::from_secs(5), resumed)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!Arc::ptr_eq(&recovered, &original));
    assert!(
        f.seen
            .lock()
            .unwrap()
            .iter()
            .any(|(_, method, thread)| method == "thread/resume" && thread == "restore"),
        "a cancelled reconnect must not strand subscriptions"
    );
}

#[tokio::test]
async fn exact_thread_settlement_recovers_before_entering_lifecycle_locks() {
    let f = fixture().await;
    let original = f.daemon.connected_client().await.unwrap();
    f.closed.lock().unwrap().insert("model/list".into());
    assert!(
        original
            .model_list(None, tokio::time::Instant::now() + Duration::from_secs(2))
            .await
            .is_err()
    );
    native_owner_tests::reader_finished(&original).await;
    f.closed.lock().unwrap().clear();
    let backend = crate::harness::backend::PlannerBackend::codex(
        f.daemon.clone(),
        "card",
        "track",
        "runtime",
    )
    .unwrap();
    let admission = NativePromptAdmission::new(|| Ok(()), || Box::pin(async { Ok(()) }));
    tokio::time::timeout(
        Duration::from_secs(2),
        backend.settled_activity("card", "thread", &admission),
    )
    .await
    .expect("exact-thread recovery must not re-enter its lifecycle locks")
    .expect("settlement must use the recovered source");
    assert_eq!(
        f.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, m, _)| m == "initialize")
            .count(),
        2
    );
    assert_eq!(
        f.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, m, t)| m == "thread/read" && t == "thread")
            .count(),
        1
    );
}

#[tokio::test]
async fn connection_replay_fences_process_replacement() {
    let f = fixture().await;
    let original = f.daemon.connected_client().await.unwrap();
    f.closed.lock().unwrap().insert("model/list".into());
    assert!(
        original
            .model_list(None, tokio::time::Instant::now() + Duration::from_secs(2))
            .await
            .is_err()
    );
    native_owner_tests::reader_finished(&original).await;
    f.closed.lock().unwrap().clear();
    f.daemon
        .thread_cache
        .insert("restore".into(), "card-restore".into());
    let replay = f.daemon.resume_replay_serial.lock().await;
    let daemon = f.daemon.clone();
    let recovering = tokio::spawn(async move { daemon.connected_client().await });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if f.daemon
                .running_client()
                .await
                .is_some_and(|client| !Arc::ptr_eq(&client, &original))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let serial = f.daemon.transition_serial.clone();
    let mut replacing = tokio::spawn(async move { serial.lock_owned().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut replacing)
            .await
            .is_err(),
        "process replacement must wait until the current replay releases its mode and credential boundary"
    );
    drop(replay);
    tokio::time::timeout(Duration::from_secs(2), recovering)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(
        tokio::time::timeout(Duration::from_secs(2), replacing)
            .await
            .unwrap()
            .unwrap(),
    );
}

async fn seed_cold_owner(f: &Fixture) -> (String, String) {
    use crate::model::{NewArea, NewCard, NewTrack, new_id};
    use crate::session_projection_repo::{
        WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    let area = f
        .daemon
        .repo
        .area_create(
            calm_types::organization::LEGACY_OWNER_USER_ID,
            NewArea {
                name: "cold-recovery".into(),
                color: "#abc".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let track = f
        .daemon
        .repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "cold-recovery".into(),
            sort: None,
            cwd: f._root.path().display().to_string(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card=f.daemon.repo.card_create(NewCard {
        track_id:track.id,title:None,kind:"terminal".into(),sort:None,
        payload:json!({"terminal_presentation":{"source":"unrecorded","in_workbench":false},"codex_source":"shared"}),
    }).await.unwrap();
    let card_id = card.id.to_string();
    let runtime = new_id();
    let init = WorkerSessionInit {
        id: runtime.clone(),
        card_id: card_id.clone(),
        kind: WorkerSessionKind::SharedPlanner,
        agent_provider: Some(AgentProvider::Codex),
        status: WorkerSessionState::Running,
        terminal_run_id: None,
        thread_id: Some("cold-thread".into()),
        session_id: None,
        active_turn_id: None,
        handle_state_json: None,
        spawn_op_id: None,
        now_ms: now_ms(),
    };
    write_in_tx_typed(f.daemon.repo.as_ref(), move |tx| {
        Box::pin(async move {
            crate::db::sqlite::session_start_runtime_tx(tx, init).await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    f.daemon
        .thread_cache
        .insert("cold-thread".into(), card_id.clone());
    let mut core = f.daemon.core.lock().await;
    let SupervisorState::Running { replay, .. } = &mut core.state else {
        panic!("running fixture")
    };
    *replay = ConnectionReplay::Pending(ReplayPlan::new(ResumeMode::ColdRespawn));
    (card_id, runtime)
}

async fn cancel_after_cold_resume_send(f: &Fixture) {
    f.pause_resume.store(true, Ordering::SeqCst);
    let daemon = f.daemon.clone();
    let recovering = tokio::spawn(async move { daemon.connected_client().await });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if f.requests
                .lock()
                .unwrap()
                .iter()
                .any(|request| request["method"] == "thread/resume")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    recovering.abort();
    assert!(matches!(recovering.await,Err(error) if error.is_cancelled()));
    f.pause_resume.store(false, Ordering::SeqCst);
    f.resume_release.notify_one();
}

fn resume_token_hashes(f: &Fixture) -> Vec<String> {
    f.requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request["method"] == "thread/resume")
        .map(|request| {
            crate::mcp_server::auth::hash_token(
                request
                    .pointer("/params/config/shell_environment_policy/set/NEIGE_MCP_TOKEN")
                    .and_then(Value::as_str)
                    .expect("cold resume must carry its authorized MCP token"),
            )
        })
        .collect()
}

#[tokio::test]
async fn cancelled_cold_replay_reuses_prepared_credentials_and_completes_once() {
    let f = fixture().await;
    seed_cold_owner(&f).await;
    cancel_after_cold_resume_send(&f).await;
    f.daemon.connected_client().await.unwrap();
    f.daemon.resume_cached_threads().await;
    let hashes = resume_token_hashes(&f);
    assert_eq!(
        hashes.len(),
        2,
        "startup owner must not repeat an already completed cold pass"
    );
    assert_eq!(
        hashes[0], hashes[1],
        "cancellation must not rotate credentials of an already loaded thread"
    );
}

#[tokio::test]
async fn cancelled_cold_replay_rechecks_retired_authority() {
    let f = fixture().await;
    let (_, runtime) = seed_cold_owner(&f).await;
    cancel_after_cold_resume_send(&f).await;
    write_in_tx_typed(f.daemon.repo.as_ref(), move |tx| {
        Box::pin(async move {
            crate::db::sqlite::session_fail_if_active_runtime_tx(tx, &runtime).await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    f.daemon.connected_client().await.unwrap();
    assert_eq!(
        resume_token_hashes(&f).len(),
        1,
        "a retired runtime must not reuse cached cold credentials"
    );
}

#[tokio::test]
async fn fresh_process_discards_cancelled_cold_credentials() {
    let f = fixture().await;
    seed_cold_owner(&f).await;
    cancel_after_cold_resume_send(&f).await;
    let (client, notifications) = connect_initialized(&f.daemon.sock, &f.daemon.approval_routes)
        .await
        .unwrap();
    let client = Arc::new(client);
    let transition = f.daemon.transition_serial.lock().await;
    drop(
        f.daemon
            .install_client(client.clone(), notifications)
            .await
            .unwrap(),
    );
    native_owner_tests::publish_replay(
        &f.daemon,
        client,
        ConnectionReplay::Pending(ReplayPlan::new(ResumeMode::ColdRespawn)),
    )
    .await;
    drop(transition);
    // A normal caller must drive the NEW process's cold obligation, before its startup caller arrives.
    f.daemon.connected_client().await.unwrap();
    f.daemon.resume_cached_threads().await;
    let hashes = resume_token_hashes(&f);
    assert_eq!(hashes.len(), 2);
    assert_ne!(
        hashes[0], hashes[1],
        "a new process must not inherit previous prepared credentials"
    );
}

fn timeout_runtime() -> crate::session_projection_repo::WorkerSessionProjection {
    use crate::session_projection_repo::{AgentProvider, WorkerSessionKind, WorkerSessionState};
    crate::session_projection_repo::WorkerSessionProjection {
        id: "runtime".into(),
        card_id: "card".into(),
        kind: WorkerSessionKind::CodexCard,
        agent_provider: Some(AgentProvider::Codex),
        status: WorkerSessionState::Running,
        terminal_run_id: None,
        thread_id: Some("thread".into()),
        session_id: None,
        active_turn_id: Some("persisted".into()),
        handle_state_json: None,
        created_at_ms: 0,
        updated_at_ms: 0,
        completed_at_ms: None,
        last_turn_completed_ms: None,
    }
}

#[tokio::test]
async fn timeout_interrupt_uses_cached_turn_or_persisted_fallback_on_exact_thread() {
    use crate::harness::backend::factory::PlannerFactory;
    for cached in [false, true] {
        let f = fixture().await;
        f.daemon.set_active_turn_for_test("decoy", "decoy-turn");
        if cached {
            f.daemon.set_active_turn_for_test("thread", "cached");
        }
        PlannerFactory::stop_timed_out_worker(&timeout_runtime(), Some(&f.daemon))
            .await
            .unwrap();
        let requests = f.requests.lock().unwrap();
        let interrupts: Vec<_> = requests
            .iter()
            .filter(|r| r["method"] == "turn/interrupt")
            .collect();
        assert_eq!(interrupts.len(), 1);
        assert_eq!(interrupts[0]["params"]["threadId"], "thread");
        assert_eq!(
            interrupts[0]["params"]["turnId"],
            if cached { "cached" } else { "persisted" }
        );
        assert_eq!(
            f.daemon.active_turn_id_for_thread("decoy").as_deref(),
            Some("decoy-turn")
        );
    }
}

#[tokio::test]
async fn timeout_interrupt_leaves_non_codex_and_missing_provider_untouched() {
    use crate::harness::backend::factory::PlannerFactory;
    use crate::session_projection_repo::AgentProvider;
    let f = fixture().await;
    for provider in [
        Some(AgentProvider::Claude),
        Some(AgentProvider::OpenCode),
        Some(AgentProvider::Grok),
        None,
    ] {
        let mut runtime = timeout_runtime();
        runtime.agent_provider = provider;
        f.daemon.set_active_turn_for_test("thread", "cached");
        PlannerFactory::stop_timed_out_worker(&runtime, Some(&f.daemon))
            .await
            .unwrap();
        assert_eq!(
            f.daemon.active_turn_id_for_thread("thread").as_deref(),
            Some("cached")
        );
    }
    assert!(
        !f.requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r["method"] == "turn/interrupt")
    );
}

#[tokio::test]
async fn timeout_interrupt_refusal_requires_native_stop_evidence() {
    use crate::harness::backend::factory::PlannerFactory;
    let f = fixture().await;
    f.answers.lock().unwrap().insert(
        "turn/interrupt".into(),
        json!({"error":{"code":-32600,"message":"turn not found"}}),
    );
    let reads = [
        ("active", json!([]), false),
        ("idle", json!([]), false),
        (
            "idle",
            json!([{ "status":"completed" }, { "status":"inProgress" }]),
            false,
        ),
        ("idle", json!([{ "status":"unknown" }]), false),
        ("notLoaded", json!([]), true),
        ("idle", json!([{"id":"last","status":"completed"}]), true),
        (
            "systemError",
            json!([{"id":"last","status":"failed"}]),
            true,
        ),
        ("idle", json!([{"id":"last","status":"interrupted"}]), true),
    ];
    for (status, turns, stopped) in reads {
        f.answers.lock().unwrap().insert(
            "thread/read".into(),
            json!({"result":{"thread":{"id":"thread","status":{"type":status,"activeFlags":[]},"turns":turns}}}),
        );
        f.daemon.set_active_turn_for_test("thread", "cached");
        let result =
            PlannerFactory::stop_timed_out_worker(&timeout_runtime(), Some(&f.daemon)).await;
        assert_eq!(result.is_ok(), stopped, "{status}: {result:?}");
    }
    f.answers.lock().unwrap().insert(
        "thread/read".into(),
        json!({"error":{"code":-32600,"message":"cannot read"}}),
    );
    assert!(
        PlannerFactory::stop_timed_out_worker(&timeout_runtime(), Some(&f.daemon))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn timeout_interrupt_transport_uncertainty_retries_until_daemon_down() {
    use crate::harness::backend::factory::PlannerFactory;
    let f = fixture().await;
    f.daemon.set_active_turn_for_test("thread", "cached");
    f.closed.lock().unwrap().insert("turn/interrupt".into());
    f.answers.lock().unwrap().insert(
        "thread/read".into(),
        json!({"result":{"thread":{"status":{"type":"notLoaded"},"turns":[]}}}),
    );
    assert!(
        PlannerFactory::stop_timed_out_worker(&timeout_runtime(), Some(&f.daemon))
            .await
            .is_err()
    );
    assert!(
        !f.requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r["method"] == "thread/read")
    );
    f.daemon.connected_client().await.unwrap();
    f.daemon.set_active_turn_for_test("thread", "cached");
    f.daemon.publish_readiness_for_test(1, false);
    PlannerFactory::stop_timed_out_worker(&timeout_runtime(), Some(&f.daemon))
        .await
        .unwrap();
}
