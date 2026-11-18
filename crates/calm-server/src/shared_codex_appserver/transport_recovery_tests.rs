//! Scripted app-server connections; production transport, replay and native owner.
use super::*;
mod environment;
mod owner_proxy;
use futures_util::{SinkExt, StreamExt};
use provider::codex::RequestCrossing;
use serde_json::{Value, json};

mod peer;
use peer::fixture;
pub(crate) use peer::{Fixture, peer_for};

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
        seed_cached_thread(&f, thread).await;
    }
    let broken_card = f.daemon.cached_card_for_thread("broken").unwrap();
    f.closed.lock().unwrap().insert("broken".into());
    {
        let mut core = f.daemon.core.lock().await;
        let SupervisorState::Running { replay, .. } = &mut core.state else {
            panic!("running fixture")
        };
        *replay = ConnectionReplay::Pending(ReplayPlan::default());
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
        .forget_threads_for_deleted_cards(&HashSet::from([broken_card]))
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
    seed_cached_thread(&f, "restore").await;
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
    seed_cached_thread(&f, "restore").await;
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

async fn seed_cached_thread(f: &Fixture, thread_id: &str) -> String {
    let card_id = seed_thread_card(f).await;
    f.daemon
        .thread_cache
        .insert(thread_id.to_owned(), card_id.clone());
    card_id
}

async fn seed_thread_card(f: &Fixture) -> String {
    use crate::model::{NewArea, NewCard, NewTrack};
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
    card.id.to_string()
}

async fn seed_cold_owner(f: &Fixture) -> (String, String) {
    use crate::model::new_id;
    use crate::session_projection_repo::{
        WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    let card_id = seed_cached_thread(f, "cold-thread").await;
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
    f.answers.lock().unwrap().insert(
        "thread/read".into(),
        json!({
            "result":{"thread":{"id":"cold-thread","status":{"type":"notLoaded"},"turns":[]}}
        }),
    );
    let mut core = f.daemon.core.lock().await;
    let SupervisorState::Running { replay, .. } = &mut core.state else {
        panic!("running fixture")
    };
    *replay = ConnectionReplay::Pending(ReplayPlan::default());
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
        "a still-cold retry must reuse its committed credentials"
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
    let (client, notifications) = connect_initialized(
        &f.daemon.verify_backend_isolation().await.unwrap(),
        &f.daemon.approval_routes,
    )
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
        ConnectionReplay::Pending(ReplayPlan::default()),
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
        ("idle", json!([{"id":"last","status":"completed"}]), false),
        ("idle", json!([{"id":"cached","status":"completed"}]), true),
        (
            "systemError",
            json!([{"id":"cached","status":"failed"}]),
            true,
        ),
        (
            "idle",
            json!([{"id":"cached","status":"interrupted"}]),
            true,
        ),
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

mod worker_model_defaults;
#[tokio::test]
async fn close_p1_history_read_does_not_reconnect_or_replay_other_threads() {
    let f = fixture().await;
    let runtime = crate::session_projection_repo::WorkerSessionProjection {
        id: "closed-worker".into(),
        card_id: "closed-card".into(),
        kind: crate::session_projection_repo::WorkerSessionKind::SharedPlanner,
        agent_provider: Some(crate::session_projection_repo::AgentProvider::Codex),
        status: crate::session_projection_repo::WorkerSessionState::Failed,
        terminal_run_id: None,
        thread_id: Some("closed-thread".into()),
        session_id: None,
        active_turn_id: None,
        handle_state_json: None,
        created_at_ms: 0,
        updated_at_ms: 0,
        completed_at_ms: None,
        last_turn_completed_ms: None,
    };
    let target = crate::harness::backend::factory::RecoveryTarget::known_failed(&runtime).unwrap();
    let admission = NativePromptAdmission::new(|| Ok(()), || Box::pin(async { Ok(()) }));
    f.daemon
        .with_ready_thread(&target, async |native| {
            native.read(None, &admission).await.map(|_| ())
        })
        .await
        .unwrap();
    f.daemon
        .thread_cache
        .insert("other-thread".into(), "other-card".into());
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
    let before = f.seen.lock().unwrap().clone();
    let refused = f
        .daemon
        .with_ready_thread(&target, async |native| {
            native.read(None, &admission).await.map(|_| ())
        })
        .await;
    assert!(matches!(refused, Err(CalmError::ExecutionUnavailable)));
    assert_eq!(
        *f.seen.lock().unwrap(),
        before,
        "a Close history read must not reconnect, replay or issue another RPC"
    );
    // Explicit ordinary recovery retains its existing responsibility and behavior.
    f.daemon
        .with_exact_thread(&target, async |native| {
            native.read(None, &admission).await.map(|_| ())
        })
        .await
        .unwrap();
    assert!(
        f.seen
            .lock()
            .unwrap()
            .iter()
            .any(|(_, method, thread)| method == "thread/resume" && thread == "other-thread")
    );
}
