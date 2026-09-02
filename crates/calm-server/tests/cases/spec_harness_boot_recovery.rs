use std::sync::Arc;
use std::time::Duration;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_create_with_id_tx, session_prepare_deferred_spec_tx, session_start_runtime_tx,
};
use calm_server::error::CalmError;
use calm_server::event::{EditAuthor, Event, EventBus, EventScope};
use calm_server::harness::{
    ClaimMode, DeferredRecoveryParams, HarnessConfig, HarnessPhaseTag, HarnessRegistry,
    HarnessSnapshot, Observation, SpecHarness, SpecHarnessParams, recover_harnesses_deferred,
    recover_harnesses_on_boot, spawn_recovered_harness,
};
use calm_server::ids::{ActorId, CardId, WaveId};
use calm_server::model::{CardRole, NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::operation::TxOutput;
use calm_server::operation::spec_harness_start_adapter::SpecHarnessStartOperationPayload;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use serde_json::json;
use tempfile::TempDir;

fn app_state_for_boot_test_with_role_cache(
    repo: Arc<SqlxRepo>,
    role_cache: calm_server::card_role_cache::CardRoleCache,
) -> AppState {
    let events = EventBus::new();
    let cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo,
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            events,
            WriteContext::new(role_cache.clone(), cove_cache.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(role_cache),
        Some(cove_cache),
    )
}

fn app_state_for_boot_test(repo: Arc<SqlxRepo>) -> AppState {
    app_state_for_boot_test_with_role_cache(
        repo,
        calm_server::card_role_cache::CardRoleCache::new(),
    )
}

/// A wave's spec card, minted with the role production gives it.
///
/// `Repo::card_create` defaults to `CardRole::Worker`, which used to be
/// invisible in the replay tests below: `spawn_recovered_harness` replayed the
/// spec push stream regardless of role. #1189 gated that replay on
/// `CardRole::Spec` — the same role the live pusher resolves
/// (`Dispatcher::resolve_spec_card`) — so a Worker-role row no longer stands in
/// for a spec card, and these fixtures have to write the role they mean.
///
/// The payload comes from `routes::waves::spec_harness_card_payload`, the same
/// function the mint route calls, rather than a hand-written
/// `{"schemaVersion": 1}`: the production shape also carries `codex_source` and
/// `spec_harness`, and a fixture that omits them would be silently unlike every
/// real spec card the moment anything backend-side starts reading either key.
async fn seed_spec_card_row(repo: &SqlxRepo, wave_id: &WaveId) -> calm_server::model::Card {
    let mut tx = repo.pool().begin().await.unwrap();
    let card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            wave_id: wave_id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: calm_server::routes::waves::spec_harness_card_payload(None),
        },
        CardRole::Spec,
        false,
        repo.card_role_cache(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    card
}

fn sqlite_url(tmp: &TempDir, name: &str) -> String {
    format!("sqlite://{}?mode=rwc", tmp.path().join(name).display())
}

#[tokio::test]
async fn boot_recovery_includes_marked_plain_chat_but_excludes_pty_codex() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "plain-chat-recovery".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            template_input: None,
            cove_id: cove.id,
            title: "plain chat recovery".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE waves SET purpose = 'cove-chat' WHERE id = ?1")
        .bind(wave.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    let chat = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "harness_profile": "plain_chat"}),
        })
        .await
        .unwrap();
    let pty = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .unwrap();
    let chat_runtime_id = new_id();
    let pty_runtime_id = new_id();
    let snapshot = HarnessSnapshot::initial(0, vec![]);
    let mut tx = repo.pool().begin().await.unwrap();
    for (runtime_id, card_id) in [
        (&chat_runtime_id, chat.id.as_str()),
        (&pty_runtime_id, pty.id.as_str()),
    ] {
        session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: runtime_id.clone(),
                card_id: card_id.to_string(),
                kind: WorkerSessionKind::CodexCard,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Idle,
                terminal_run_id: None,
                thread_id: Some(format!("thread-{card_id}")),
                session_id: None,
                active_turn_id: None,
                handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
                spawn_op_id: None,
                now_ms: now_ms(),
            },
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();

    let recovered = repo
        .session_projection_recover_harnesses_on_boot()
        .await
        .unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].id, chat_runtime_id);
    assert_eq!(recovered[0].kind, WorkerSessionKind::CodexCard);
    let registry = HarnessRegistry::new();
    let outcome = spawn_recovered_harness(
        repo.clone(),
        EventBus::new(),
        repo.card_role_cache().clone(),
        repo.wave_cove_cache().clone(),
        SharedCodexAppServer::new_stub(repo.clone()),
        &registry,
        recovered.into_iter().next().unwrap(),
        ClaimMode::Replace,
    )
    .await
    .expect("marked Worker plain-chat runtime must pass the cove-chat recovery fence");
    let handle = outcome
        .installed()
        .expect("plain-chat runtime must install a harness");
    assert!(registry.get(&chat_runtime_id).is_some());
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn direct_recovery_boundary_rejects_cove_chat_spec_runtime() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "chat-spec-recovery-fence".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            template_input: None,
            cove_id: cove.id,
            title: "chat spec".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE waves SET purpose = 'cove-chat' WHERE id = ?1")
        .bind(wave.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some("thread-chat-spec-fence".into());
    let mut tx = repo.pool().begin().await.unwrap();
    let card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            wave_id: wave.id,
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        },
        CardRole::Spec,
        false,
        repo.card_role_cache(),
    )
    .await
    .unwrap();
    let runtime_id = new_id();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some("thread-chat-spec-fence".into()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(snapshot).unwrap()),
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let card_id = card.id.to_string();
    let runtime = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .unwrap();
    let registry = HarnessRegistry::new();
    let result = spawn_recovered_harness(
        repo.clone(),
        EventBus::new(),
        repo.card_role_cache().clone(),
        repo.wave_cove_cache().clone(),
        SharedCodexAppServer::new_stub(repo.clone()),
        &registry,
        runtime,
        ClaimMode::Replace,
    )
    .await;
    assert!(matches!(
        result,
        Ok(calm_server::harness::RecoveryOutcome::Skipped)
    ));
    assert!(registry.get(&runtime_id).is_none());
}

/// Seed a cove/wave/card + recoverable SharedSpec runtime row; returns the
/// runtime id.
async fn seed_recoverable_runtime(repo: &Arc<SqlxRepo>, tag: &str, thread_id: &str) -> String {
    let cove = repo
        .cove_create(NewCove {
            name: tag.into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            template_input: None,
            cove_id: cove.id,
            title: tag.into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id,
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .unwrap();
    let runtime_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_id.into());
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(thread_id.into()),
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
    runtime_id
}

#[tokio::test]
async fn boot_recovery_skips_cove_chat_spec_and_recovers_later_valid_runtime() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let declined_id = seed_recoverable_runtime(&repo, "declined-first", "thread-declined").await;
    let declined = repo
        .session_projection_by_id(&declined_id)
        .await
        .unwrap()
        .unwrap();
    let declined_card = repo.card_get(&declined.card_id).await.unwrap().unwrap();
    sqlx::query("UPDATE cards SET role = 'spec' WHERE id = ?1")
        .bind(declined_card.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    repo.card_role_cache().insert(
        declined_card.id.clone(),
        CardRole::Spec,
        declined_card.wave_id.clone(),
    );
    sqlx::query("UPDATE waves SET purpose = 'cove-chat' WHERE id = ?1")
        .bind(declined_card.wave_id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    let valid_id = seed_recoverable_runtime(&repo, "valid-second", "thread-valid").await;
    let registry = HarnessRegistry::new();

    let recovered = recover_harnesses_on_boot(
        repo.clone(),
        EventBus::new(),
        repo.card_role_cache().clone(),
        repo.wave_cove_cache().clone(),
        SharedCodexAppServer::new_stub(repo.clone()),
        &registry,
    )
    .await
    .unwrap();

    assert_eq!(recovered, 1);
    assert!(registry.get(&declined_id).is_none());
    let valid = registry
        .remove(&valid_id)
        .expect("valid runtime after declined row must still recover");
    valid.shutdown().await.unwrap();
}

#[tokio::test]
async fn boot_recovery_respawns_harness_with_snapshot() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "boot".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            template_input: None,
            cove_id: cove.id.clone(),
            title: "boot".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id,
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .unwrap();
    let runtime_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(
        42,
        vec![Observation::WaveGoal {
            text: "recover me".into(),
        }],
    );
    snapshot.phase = HarnessPhaseTag::TurnCompleted;
    snapshot.last_thread_id = Some("thread-recovered".into());
    snapshot.last_turn_id = Some("turn-recovered".into());
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some("thread-recovered".into()),
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

    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let registry = HarnessRegistry::new();
    let recovered = recover_harnesses_on_boot(
        repo,
        EventBus::new(),
        calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::wave_cove_cache::WaveCoveCache::new(),
        daemon,
        &registry,
    )
    .await
    .unwrap();
    assert_eq!(recovered, 1);
    let handle = registry.get(&runtime_id).expect("recovered harness");
    let restored = handle.snapshot().await;
    assert_eq!(restored.push_watermark, 42);
    assert_eq!(restored.pending_queue.len(), 1);
    assert_eq!(restored.last_turn_id.as_deref(), Some("turn-recovered"));
    handle.shutdown().await.unwrap();
}

/// #953 test 3 — boot-spawn failure no longer skips harness recovery
/// forever: the Err arm arms a DEFERRED claim-based recovery (not run
/// immediately), and the first `running: true` on the supervisor readiness
/// watch triggers a pass that recovers what the user didn't touch and never
/// shutdown-replaces a runtime the user resumed in the meantime.
#[tokio::test]
async fn boot_spawn_failure_defers_recovery_until_heal_then_recovers_claim_based() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let untouched_runtime_id =
        seed_recoverable_runtime(&repo, "deferred-untouched", "thread-untouched").await;
    let user_runtime_id = seed_recoverable_runtime(&repo, "deferred-user", "thread-user").await;

    let state = app_state_for_boot_test(repo.clone());
    let recovered = calm_server::recover_harnesses_after_daemon_boot(
        &state,
        Err(CalmError::CodexAppServer("daemon unavailable".into())),
    )
    .await
    .unwrap();
    // Deferred-armed, NOT run: nothing is recovered while the daemon stays
    // down.
    assert_eq!(recovered, 0);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(state.harness.get(&untouched_runtime_id).is_none());
    assert!(state.harness.get(&user_runtime_id).is_none());

    // The user resumes one runtime before the daemon heals (today's
    // shutdown-replace semantics).
    let user_runtime = repo
        .session_projection_by_id(&user_runtime_id)
        .await
        .unwrap()
        .unwrap();
    let user_handle = spawn_recovered_harness(
        repo.clone(),
        state.events.clone(),
        calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::wave_cove_cache::WaveCoveCache::new(),
        state.shared_codex_appserver.clone(),
        &state.harness,
        user_runtime,
        ClaimMode::Replace,
    )
    .await
    .unwrap()
    .installed()
    .expect("user resume registers a harness");

    // First heal success: the readiness watch flips running.
    state
        .shared_codex_appserver
        .publish_readiness_for_test(1, true);

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if state.harness.get(&untouched_runtime_id).is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("deferred recovery must recover the untouched runtime after heal");

    // The user's harness was never shutdown-replaced: its run loop still
    // accepts observations and its registry slot is intact.
    tokio::time::sleep(Duration::from_millis(100)).await;
    user_handle
        .observe(Observation::WaveGoal {
            text: "still mine".into(),
        })
        .expect("user harness must stay alive through deferred recovery");
    assert!(state.harness.get(&user_runtime_id).is_some());

    for runtime_id in [&untouched_runtime_id, &user_runtime_id] {
        if let Some(handle) = state.harness.remove(runtime_id) {
            handle.shutdown().await.unwrap();
        }
    }
}

/// #953 test 8 — deterministic claim race: the user's registration lands
/// AFTER the deferred task's per-runtime eligibility check (fixtures-only
/// post-eligibility hook) and BEFORE its claim ⇒ `try_reserve` returns None
/// ⇒ the runtime is skipped without any shutdown-replace.
#[tokio::test]
async fn deferred_recovery_skips_runtime_claimed_after_eligibility_check() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let runtime_id = seed_recoverable_runtime(&repo, "deferred-race", "thread-race").await;
    let runtime = repo
        .session_projection_by_id(&runtime_id)
        .await
        .unwrap()
        .unwrap();

    let daemon = SharedCodexAppServer::new_stub_with_pending(repo.clone(), None);
    let registry = HarnessRegistry::new();
    let events = EventBus::new();

    // The user's harness, built but NOT registered yet — the hook lands it
    // inside the eligibility→claim window.
    let user_handle = SpecHarness::run(SpecHarnessParams {
        runtime_id: runtime_id.clone(),
        wave_id: WaveId::from(
            repo.card_get(&runtime.card_id)
                .await
                .unwrap()
                .unwrap()
                .wave_id
                .to_string(),
        ),
        card_id: CardId::from(runtime.card_id.clone()),
        thread_id: runtime.thread_id.clone(),
        repo: repo.clone(),
        events: events.clone(),
        card_role_cache: calm_server::card_role_cache::CardRoleCache::new(),
        wave_cove_cache: calm_server::wave_cove_cache::WaveCoveCache::new(),
        daemon: daemon.clone(),
        config: HarnessConfig::default(),
        snapshot: HarnessSnapshot::initial(0, vec![]),
    });

    let hook_fired = Arc::new(AtomicUsize::new(0));
    let pending_user_install = Arc::new(std::sync::Mutex::new(Some(user_handle.clone())));
    let hook_registry = registry.clone();
    let hook_fired_in_hook = hook_fired.clone();
    let hook_target = runtime_id.clone();
    let post_eligibility_hook: std::sync::Arc<dyn Fn(&String) + Send + Sync> =
        std::sync::Arc::new(move |eligible_runtime_id: &String| {
            if *eligible_runtime_id != hook_target {
                return;
            }
            hook_fired_in_hook.fetch_add(1, Ordering::SeqCst);
            if let Some(handle) = pending_user_install.lock().unwrap().take() {
                let (reservation, previous_live) =
                    hook_registry.reserve_replacing(eligible_runtime_id.clone());
                assert!(
                    previous_live.is_none(),
                    "deferred task must not have claimed yet"
                );
                assert!(reservation.install(handle));
            }
        });

    let driver = tokio::spawn(recover_harnesses_deferred(DeferredRecoveryParams {
        repo: repo.clone(),
        events,
        card_role_cache: calm_server::card_role_cache::CardRoleCache::new(),
        wave_cove_cache: calm_server::wave_cove_cache::WaveCoveCache::new(),
        daemon: daemon.clone(),
        registry: registry.clone(),
        post_eligibility_hook: Some(post_eligibility_hook),
    }));
    daemon.publish_readiness_for_test(1, true);
    tokio::time::timeout(Duration::from_secs(5), driver)
        .await
        .expect("deferred recovery pass must complete")
        .unwrap();

    assert_eq!(hook_fired.load(Ordering::SeqCst), 1);
    // try_reserve lost against the user's registration: no shutdown-replace
    // — the user's run loop still accepts observations and holds the slot.
    user_handle
        .observe(Observation::WaveGoal {
            text: "user wins the claim".into(),
        })
        .expect("user harness must never be shutdown-replaced by deferred recovery");
    registry
        .remove(&runtime_id)
        .expect("user harness still registered")
        .shutdown()
        .await
        .unwrap();
}

/// #953 PR2 review D1 — claim-boundary daemon re-check: the daemon leaves
/// Running inside the eligibility→claim window (which contains the
/// potentially long event replay — the fixtures post-eligibility hook fires
/// at the start of exactly that window). The deferred task must NOT install
/// a harness against the stale generation: it abandons the pass without
/// reserving anything, re-arms the wait loop, and recovers only after the
/// daemon heals again (new generation).
#[tokio::test]
async fn deferred_recovery_abandons_claim_and_rearms_when_daemon_transitions_during_replay() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let runtime_id = seed_recoverable_runtime(&repo, "deferred-daemon-flap", "thread-flap").await;

    let daemon = SharedCodexAppServer::new_stub_with_pending(repo.clone(), None);
    let registry = HarnessRegistry::new();
    let events = EventBus::new();

    let hook_fired = Arc::new(AtomicUsize::new(0));
    let hook_daemon = daemon.clone();
    let hook_fired_in_hook = hook_fired.clone();
    let hook_target = runtime_id.clone();
    let post_eligibility_hook: std::sync::Arc<dyn Fn(&String) + Send + Sync> =
        std::sync::Arc::new(move |eligible_runtime_id: &String| {
            if *eligible_runtime_id != hook_target {
                return;
            }
            // First pass only: the daemon fails (readiness invalidated with
            // the outgoing generation — what transition ENTRY publishes)
            // inside the eligibility→claim window.
            if hook_fired_in_hook.fetch_add(1, Ordering::SeqCst) == 0 {
                hook_daemon.publish_readiness_for_test(1, false);
            }
        });

    let driver = tokio::spawn(recover_harnesses_deferred(DeferredRecoveryParams {
        repo: repo.clone(),
        events,
        card_role_cache: calm_server::card_role_cache::CardRoleCache::new(),
        wave_cove_cache: calm_server::wave_cove_cache::WaveCoveCache::new(),
        daemon: daemon.clone(),
        registry: registry.clone(),
        post_eligibility_hook: Some(post_eligibility_hook),
    }));

    // First heal success: pass 1 starts, eligibility sees Running(gen 1).
    daemon.publish_readiness_for_test(1, true);
    tokio::time::timeout(Duration::from_secs(5), async {
        while hook_fired.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("pass 1 must reach the post-eligibility window");

    // Let replay + the claim-boundary re-check run to completion: nothing
    // may be installed against the stale generation, and the task must
    // re-arm (still alive) rather than finish its pass.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        registry.get(&runtime_id).is_none(),
        "no harness may be installed against a daemon that left Running during replay"
    );
    assert!(
        !driver.is_finished(),
        "the deferred task must abandon the pass and re-arm, not exit"
    );

    // The daemon heals again (new generation): recovery resumes and installs.
    daemon.publish_readiness_for_test(2, true);
    tokio::time::timeout(Duration::from_secs(5), driver)
        .await
        .expect("deferred recovery must complete after the daemon heals again")
        .unwrap();
    assert_eq!(hook_fired.load(Ordering::SeqCst), 2);
    let handle = registry
        .get(&runtime_id)
        .expect("recovery must resume on the next heal");
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn boot_recovery_is_deferred_until_shared_daemon_is_running() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "boot-deferred".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            template_input: None,
            cove_id: cove.id.clone(),
            title: "boot-deferred".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id,
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .unwrap();
    let runtime_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(
        7,
        vec![Observation::TaskCompleted {
            idempotency_key: "deferred-boot".into(),
            result: json!({"ok": true}),
        }],
    );
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some("thread-deferred".into());
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some("thread-deferred".into()),
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

    let disconnected = SharedCodexAppServer::new_stub_with_pending(repo.clone(), None);
    let registry = HarnessRegistry::new();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(disconnected.turn_start_count_for_test(), 0);
    assert!(registry.get(&runtime_id).is_none());

    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let recovered = recover_harnesses_on_boot(
        repo,
        EventBus::new(),
        calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::wave_cove_cache::WaveCoveCache::new(),
        daemon.clone(),
        &registry,
    )
    .await
    .unwrap();
    assert_eq!(recovered, 1);
    tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            if daemon.turn_start_count_for_test() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("recovered harness should issue a turn after daemon takeover");
    assert_eq!(daemon.turn_start_count_for_test(), 1);
    let handle = registry.get(&runtime_id).expect("recovered harness");
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn boot_recovery_replays_events_since_snapshot_watermark() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "boot-replay".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            template_input: None,
            cove_id: cove.id.clone(),
            title: "boot-replay".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = seed_spec_card_row(&repo, &wave.id).await;
    let bus = EventBus::new();
    let role_cache = calm_server::card_role_cache::CardRoleCache::new();
    let cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    let missed_id = repo
        .log_pure_event(
            ActorId::User,
            EventScope::Wave {
                wave: wave.id.clone(),
                cove: cove.id.clone(),
            },
            None,
            &bus,
            &role_cache,
            &cove_cache,
            Event::WaveReportEdited {
                wave_id: wave.id.clone(),
                card_id: card.id.clone(),
                author: EditAuthor::User,
                author_plugin_id: None,
                edit_id: "missed-edit".into(),
                summary_before: String::new(),
                summary_after: "missed summary".into(),
                body_before: String::new(),
                body_after: "missed body".into(),
                agent_message: None,
            },
        )
        .await
        .unwrap();
    let queued_id = repo
        .log_pure_event(
            ActorId::User,
            EventScope::Wave {
                wave: wave.id.clone(),
                cove: cove.id.clone(),
            },
            None,
            &bus,
            &role_cache,
            &cove_cache,
            Event::WaveReportEdited {
                wave_id: wave.id.clone(),
                card_id: card.id.clone(),
                author: EditAuthor::User,
                author_plugin_id: None,
                edit_id: "queued-edit".into(),
                summary_before: String::new(),
                summary_after: "queued summary".into(),
                body_before: String::new(),
                body_after: "queued body".into(),
                agent_message: None,
            },
        )
        .await
        .unwrap();
    let runtime_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some("thread-recovered".into());
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some("thread-recovered".into()),
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

    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let registry = HarnessRegistry::new();
    let recovered = recover_harnesses_on_boot(
        repo.clone(),
        EventBus::new(),
        calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::wave_cove_cache::WaveCoveCache::new(),
        daemon,
        &registry,
    )
    .await
    .unwrap();
    assert_eq!(recovered, 1);
    let runtime = repo
        .session_projection_by_id(&runtime_id)
        .await
        .unwrap()
        .unwrap();
    let stored: HarnessSnapshot =
        serde_json::from_value(runtime.handle_state_json.unwrap()).unwrap();
    assert_eq!(stored.push_watermark, queued_id.max(missed_id));
    assert_eq!(stored.pending_queue.len(), 2);
    assert!(stored.pending_queue.iter().any(|obs| {
        matches!(obs, Observation::ReportEdited { body, .. } if body == "queued body")
    }));
    assert!(stored.pending_queue.iter().any(|obs| {
        matches!(obs, Observation::ReportEdited { body, .. } if body == "missed body")
    }));
    let handle = registry.get(&runtime_id).expect("recovered harness");
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn boot_recovery_skips_terminal_waves() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "boot-terminal".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            template_input: None,
            cove_id: cove.id,
            title: "boot-terminal".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE waves SET lifecycle = 'done' WHERE id = ?1")
        .bind(wave.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id,
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .unwrap();
    let runtime_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(
        42,
        vec![Observation::WaveGoal {
            text: "do not recover".into(),
        }],
    );
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some("thread-terminal".into());
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some("thread-terminal".into()),
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

    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let registry = HarnessRegistry::new();
    let recovered = recover_harnesses_on_boot(
        repo,
        EventBus::new(),
        calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::wave_cove_cache::WaveCoveCache::new(),
        daemon,
        &registry,
    )
    .await
    .unwrap();
    assert_eq!(recovered, 0);
    assert!(registry.get(&runtime_id).is_none());
}

#[tokio::test]
async fn boot_recovery_skips_deferred_worker_session_phantom_ghost() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "boot-phantom".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            template_input: None,
            cove_id: cove.id,
            title: "boot-phantom".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id,
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .unwrap();
    let placeholder_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(
        1,
        vec![Observation::WaveGoal {
            text: "must not recover".into(),
        }],
    );
    snapshot.phase = HarnessPhaseTag::Idle;

    let mut tx = repo.pool().begin().await.unwrap();
    session_prepare_deferred_spec_tx(
        &mut tx,
        &WorkerSessionInit {
            id: placeholder_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Starting,
            terminal_run_id: None,
            thread_id: None,
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

    let mirror: Option<String> = sqlx::query_scalar("SELECT id FROM worker_sessions WHERE id = ?1")
        .bind(&placeholder_id)
        .fetch_optional(repo.pool())
        .await
        .unwrap();
    assert_eq!(mirror.as_deref(), Some(placeholder_id.as_str()));

    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let registry = HarnessRegistry::new();
    let recovered = recover_harnesses_on_boot(
        repo,
        EventBus::new(),
        calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::wave_cove_cache::WaveCoveCache::new(),
        daemon,
        &registry,
    )
    .await
    .unwrap();
    assert_eq!(recovered, 0);
    assert!(registry.get(&placeholder_id).is_none());
}

#[tokio::test]
async fn force_new_thread_recovery_after_phase2_crash() {
    let tmp = TempDir::new().unwrap();
    let db_url = sqlite_url(&tmp, "phase2-crash.db");
    let (card_id, wave_id, old_runtime_id, placeholder_id, op_id) = {
        let repo = Arc::new(SqlxRepo::open(&db_url).await.unwrap());
        let cove = repo
            .cove_create(NewCove {
                name: "phase2-crash".into(),
                color: "#111111".into(),
                sort: None,
            })
            .await
            .unwrap();
        let wave = repo
            .wave_create(NewWave {
                template_input: None,
                cove_id: cove.id,
                title: "phase2 crash".into(),
                sort: None,
                cwd: "/tmp".into(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: calm_server::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let card = repo
            .card_create(NewCard {
                wave_id: wave.id.clone(),
                title: None,
                kind: "codex".into(),
                sort: None,
                payload: json!({
                    "schemaVersion": 1,
                    "codex_source": "shared",
                    "spec_harness": true
                }),
            })
            .await
            .unwrap();
        let old_runtime_id = new_id();
        let old_snapshot = HarnessSnapshot::initial(0, vec![]);
        let placeholder_id = new_id();
        let placeholder_snapshot = HarnessSnapshot::initial(0, vec![]);
        let now = now_ms();
        let payload = serde_json::to_value(SpecHarnessStartOperationPayload {
            actor: ActorId::User,
            wave_id: wave.id.to_string(),
            spec_card_id: card.id.clone(),
            report_card_id: None,
            sort: None,
            cwd: wave.workspace.path.clone(),
            goal: Some("recover after crash".into()),
            reset_harness_items: false,
            force_new_thread: true,
            profile: Default::default(),
            create_card: None,
            first_message_sha256: None,
        })
        .unwrap();
        let mut output = TxOutput::new(
            "card",
            Some(card.id.to_string()),
            serde_json::to_value(&card).unwrap(),
        );
        output.data = json!({
            "card_id": card.id.to_string(),
            "wave_id": wave.id.to_string(),
            "runtime_id": placeholder_id.clone(),
            "runtime_deferred": true,
            "cwd": wave.workspace.path.clone(),
            "goal": "recover after crash",
            "report_card_id": null,
            "snapshot": serde_json::to_value(&placeholder_snapshot).unwrap(),
            "old_runtime_id": old_runtime_id.clone(),
            "old_runtime_status": WorkerSessionState::Idle,
        });
        let op_id = new_id();

        let mut tx = repo.pool().begin().await.unwrap();
        session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: old_runtime_id.clone(),
                card_id: card.id.to_string(),
                kind: WorkerSessionKind::SharedSpec,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Idle,
                terminal_run_id: None,
                thread_id: Some("thread-old-before-crash".into()),
                session_id: None,
                active_turn_id: None,
                handle_state_json: Some(serde_json::to_value(&old_snapshot).unwrap()),
                spawn_op_id: None,
                now_ms: now,
            },
        )
        .await
        .unwrap();
        session_prepare_deferred_spec_tx(
            &mut tx,
            &WorkerSessionInit {
                id: placeholder_id.clone(),
                card_id: card.id.to_string(),
                kind: WorkerSessionKind::SharedSpec,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Starting,
                terminal_run_id: None,
                thread_id: None,
                session_id: None,
                active_turn_id: None,
                handle_state_json: Some(serde_json::to_value(&placeholder_snapshot).unwrap()),
                spawn_op_id: None,
                now_ms: now + 1,
            },
        )
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO operations (
                   id, operation_key, kind, idempotency_key, payload_hash,
                   target_type, target_id, target_json, payload_json,
                   tx_output_json, phase, created_at_ms, updated_at_ms
               )
               VALUES (?1, ?2, 'spec-harness-start', NULL, ?3,
                       'card', ?4, ?5, ?6, ?7, 'tx_committed', ?8, ?8)"#,
        )
        .bind(&op_id)
        .bind(new_id())
        .bind(new_id())
        .bind(card.id.as_str())
        .bind(serde_json::to_string(&json!({"type": "card", "id": card.id})).unwrap())
        .bind(serde_json::to_string(&payload).unwrap())
        .bind(serde_json::to_string(&output).unwrap())
        .bind(now + 2)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();

        (
            card.id.to_string(),
            wave.id.to_string(),
            old_runtime_id,
            placeholder_id,
            op_id,
        )
    };

    let repo = Arc::new(SqlxRepo::open(&db_url).await.unwrap());
    let role_cache = calm_server::card_role_cache::CardRoleCache::new();
    role_cache.insert(
        CardId::from(card_id.clone()),
        CardRole::Spec,
        WaveId::from(wave_id.clone()),
    );
    let state = app_state_for_boot_test_with_role_cache(repo.clone(), role_cache)
        .with_shared_codex_appserver(SharedCodexAppServer::new_fake_running_with_pending(
            repo.clone(),
            None,
        ));

    calm_server::recover_operations_on_boot(&state)
        .await
        .unwrap();

    let active = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("phase-2 recovery should leave a new active session");
    assert_eq!(active.id, placeholder_id);
    assert_eq!(active.status, WorkerSessionState::Idle);
    assert_eq!(active.thread_id.as_deref(), Some("fake-thread-0001"));
    assert_ne!(active.id, old_runtime_id);

    let old_state: String = sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id = ?1")
        .bind(&old_runtime_id)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(old_state, "superseded");

    let active_count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)
             FROM worker_sessions
            WHERE card_id = ?1
              AND state IN ('starting','running','idle','turn_pending')"#,
    )
    .bind(&card_id)
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(active_count, 1);

    let card_session: Option<String> =
        sqlx::query_scalar("SELECT session_id FROM cards WHERE id = ?1")
            .bind(&card_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(card_session.as_deref(), Some(placeholder_id.as_str()));

    let phase: String = sqlx::query_scalar("SELECT phase FROM operations WHERE id = ?1")
        .bind(&op_id)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(phase, "succeeded");

    if let Some(handle) = state.harness.remove(&placeholder_id) {
        handle.shutdown().await.unwrap();
    }
}

/// Issue #644 PR-C (§6.5/§8) — the boot replay applies the SAME
/// gated-self-report consultation as the live push branch: a gated
/// task's `task.completed` is NOT replayed to the spec (the gate
/// verdict is what wakes it), an ungated task's self-report and the
/// `task.gate_result` itself replay as observations. Round-3 review
/// F1: a stale `task.failed` against a gated row the gate owns
/// (`verifying` here) is suppressed too, while a gated task whose
/// worker genuinely failed pre-gate (`failed` + `worker-reported`)
/// replays as today.
#[tokio::test]
async fn boot_replay_suppresses_gated_self_report_and_replays_gate_result() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "gate-replay".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            template_input: None,
            cove_id: cove.id.clone(),
            title: "gate-replay".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = seed_spec_card_row(&repo, &wave.id).await;

    // One gated and one ungated tasks row.
    let mk_task = |key: &str, gate: Option<String>| calm_server::model::Task {
        id: format!("{}:{key}", wave.id.as_str()),
        wave_id: wave.id.as_str().to_string(),
        key: key.to_string(),
        kind: calm_server::model::TaskKind::Codex,
        goal: "g".into(),
        context_json: "null".into(),
        acceptance_criteria: None,
        cwd: None,
        depends_on_json: "[]".into(),
        priority: 0,
        gate_json: gate,
        status: calm_server::model::TaskStatus::Verifying,
        status_detail: None,
        worker_card_id: None,
        gate_result_json: None,
        gate_attempt: 0,
        gate_pid: None,
        gate_pid_starttime: None,
        gate_pid_boot_id: None,
        running_deadline_ms: None,
        context_stale_at_ms: None,
        declared_by: "spec".into(),
        spawn: "in-wave".into(),
        created_at_ms: now_ms(),
        updated_at_ms: now_ms(),
        finished_at_ms: None,
    };
    let gate_json = json!({ "steps": [{ "name": "t", "cmd": "true" }] }).to_string();
    let gated = mk_task("gated", Some(gate_json.clone()));
    let mut ungated = mk_task("ungated", None);
    ungated.status = calm_server::model::TaskStatus::Done;
    // Round-3 review F1 — a gated task whose worker genuinely failed
    // pre-gate: the failure landed on the row, so its `task.failed`
    // replays as today.
    let mut gated_failed = mk_task("gated-failed", Some(gate_json));
    gated_failed.status = calm_server::model::TaskStatus::Failed;
    gated_failed.status_detail = Some("worker-reported".to_string());
    let gated_id = gated.id.clone();
    let ungated_id = ungated.id.clone();
    let gated_failed_id = gated_failed.id.clone();
    calm_server::db::write_in_tx_typed(repo.as_ref() as &dyn Repo, move |tx| {
        Box::pin(async move {
            crate::support::task::insert_task_tx(tx, &gated).await?;
            crate::support::task::insert_task_tx(tx, &ungated).await?;
            crate::support::task::insert_task_tx(tx, &gated_failed).await?;
            Ok(())
        })
    })
    .await
    .unwrap();

    let bus = EventBus::new();
    let role_cache = calm_server::card_role_cache::CardRoleCache::new();
    let cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&cove_cache).await.unwrap();
    let scope = EventScope::Wave {
        wave: wave.id.clone(),
        cove: cove.id.clone(),
    };
    for event in [
        Event::TaskCompleted {
            idempotency_key: gated_id.clone(),
            result: json!({ "claim": true }),
            artifacts: Vec::new(),
            agent_message: None,
        },
        Event::TaskCompleted {
            idempotency_key: ungated_id.clone(),
            result: json!({ "ok": true }),
            artifacts: Vec::new(),
            agent_message: None,
        },
        // Round-3 review F1 — a stale/retried `task.failed` against
        // the gated row the gate owns (`verifying`): the failure never
        // landed on the row, so it must NOT replay.
        Event::TaskFailed {
            idempotency_key: gated_id.clone(),
            reason: "stale worker claim".into(),
            agent_message: None,
        },
        // ... while the genuine pre-gate worker failure replays.
        Event::TaskFailed {
            idempotency_key: gated_failed_id.clone(),
            reason: "worker said no".into(),
            agent_message: None,
        },
    ] {
        repo.log_pure_event(
            ActorId::User,
            scope.clone(),
            None,
            &bus,
            &role_cache,
            &cove_cache,
            event,
        )
        .await
        .unwrap();
    }
    repo.log_pure_event(
        ActorId::KernelDispatcher,
        scope.clone(),
        None,
        &bus,
        &role_cache,
        &cove_cache,
        Event::TaskGateResult {
            task_id: gated_id.clone(),
            idempotency_key: gated_id.clone(),
            passed: true,
            failing_step: None,
            exit_code: Some(0),
            log_tail: String::new(),
            log_path: "/tmp/gate.log".into(),
            attempt: 1,
            agent_message: None,
        },
    )
    .await
    .unwrap();

    let runtime_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some("thread-recovered".into());
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some("thread-recovered".into()),
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

    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let registry = HarnessRegistry::new();
    let recovered = recover_harnesses_on_boot(
        repo.clone(),
        EventBus::new(),
        calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::wave_cove_cache::WaveCoveCache::new(),
        daemon,
        &registry,
    )
    .await
    .unwrap();
    assert_eq!(recovered, 1);
    let runtime = repo
        .session_projection_by_id(&runtime_id)
        .await
        .unwrap()
        .unwrap();
    let stored: HarnessSnapshot =
        serde_json::from_value(runtime.handle_state_json.unwrap()).unwrap();
    assert_eq!(
        stored.pending_queue.len(),
        3,
        "ungated self-report + gate result + genuine pre-gate failure, \
         never the gated self-report or the stale gated task.failed: {:?}",
        stored.pending_queue
    );
    assert!(
        stored.pending_queue.iter().any(|obs| matches!(
            obs,
            Observation::TaskCompleted { idempotency_key, .. } if idempotency_key == &ungated_id
        )),
        "{:?}",
        stored.pending_queue
    );
    assert!(
        stored.pending_queue.iter().any(|obs| matches!(
            obs,
            Observation::TaskGateResult { idempotency_key, passed: true, .. }
                if idempotency_key == &gated_id
        )),
        "{:?}",
        stored.pending_queue
    );
    assert!(
        !stored.pending_queue.iter().any(|obs| matches!(
            obs,
            Observation::TaskCompleted { idempotency_key, .. } if idempotency_key == &gated_id
        )),
        "gated self-report must be suppressed in replay (§6.5): {:?}",
        stored.pending_queue
    );
    // Round-3 review F1 — failure split.
    assert!(
        !stored.pending_queue.iter().any(|obs| matches!(
            obs,
            Observation::TaskFailed { idempotency_key, .. } if idempotency_key == &gated_id
        )),
        "stale task.failed against the verifying gated row must be suppressed in replay: {:?}",
        stored.pending_queue
    );
    assert!(
        stored.pending_queue.iter().any(|obs| matches!(
            obs,
            Observation::TaskFailed { idempotency_key, .. }
                if idempotency_key == &gated_failed_id
        )),
        "genuine pre-gate worker failure must replay as today: {:?}",
        stored.pending_queue
    );
    let handle = registry.get(&runtime_id).expect("recovered harness");
    handle.shutdown().await.unwrap();
}

/// #1189 A1 — a kernel restart mid-conversation.
///
/// Two halves of one bug, on one ordinary (non-cove-chat) wave:
///
/// * **the selector.** `session_projection_recover_harnesses_on_boot`'s second
///   `OR` arm was written for cove chat (`executor` + `role = 'worker'` +
///   `plain_chat`) and a wave assistant matches none of its three conjuncts. A
///   restart during an assistant turn therefore left the `worker_sessions` row
///   alive with no run loop behind it: `GET /spec/run` answers dormant and the
///   user's reply never arrives.
/// * **the replay.** `spawn_recovered_harness` called
///   `replay_harness_events_since` for every role. That function replays the
///   SPEC push stream — task completions, report edits, gate verdicts — which
///   the live dispatcher only ever pushes to the card whose role is
///   `CardRole::Spec` (`Dispatcher::resolve_spec_card`). A freshly minted
///   assistant starts at watermark 0, so the first recovery would have queued
///   the wave's entire spec backlog into a conversation.
///
/// The fixture keeps all four recovery classes side by side so a fix that
/// widened the selector too far is red as well: the real codex worker must stay
/// out, and the cove chat must stay in.
#[tokio::test]
async fn boot_recovery_registers_the_assistant_without_replaying_the_spec_backlog() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "assistant-recovery".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            template_input: None,
            cove_id: cove.id.clone(),
            title: "assistant recovery".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    // A cove chat wave alongside it: the #1098 recovery class must keep working.
    let chat_wave = repo
        .wave_create(NewWave {
            template_input: None,
            cove_id: cove.id.clone(),
            title: "cove chat".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE waves SET purpose = 'cove-chat' WHERE id = ?1")
        .bind(chat_wave.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();

    let mut tx = repo.pool().begin().await.unwrap();
    let mk = |wave_id: WaveId, payload: serde_json::Value| NewCard {
        wave_id,
        title: None,
        kind: "codex".into(),
        sort: None,
        payload,
    };
    // The spec card: the only legitimate recipient of the spec push stream.
    let spec_card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        mk(wave.id.clone(), json!({"schemaVersion": 1})),
        CardRole::Spec,
        false,
        repo.card_role_cache(),
    )
    .await
    .unwrap();
    // The wave assistant, exactly as `spec_harness_start_adapter` mints it.
    let assistant_card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        mk(
            wave.id.clone(),
            json!({"schemaVersion": 1, "harness_profile": "assistant"}),
        ),
        CardRole::Assistant,
        false,
        repo.card_role_cache(),
    )
    .await
    .unwrap();
    // A real dispatched codex worker on the same wave: never harness-recovered.
    let worker_card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        mk(wave.id.clone(), json!({"schemaVersion": 1})),
        CardRole::Worker,
        true,
        repo.card_role_cache(),
    )
    .await
    .unwrap();
    let chat_card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        mk(
            chat_wave.id.clone(),
            json!({"schemaVersion": 1, "harness_profile": "plain_chat"}),
        ),
        CardRole::Worker,
        false,
        repo.card_role_cache(),
    )
    .await
    .unwrap();

    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    let spec_runtime_id = new_id();
    let assistant_runtime_id = new_id();
    let worker_runtime_id = new_id();
    let chat_runtime_id = new_id();
    for (runtime_id, card_id, kind) in [
        (
            &spec_runtime_id,
            spec_card.id.as_str(),
            WorkerSessionKind::SharedSpec,
        ),
        (
            &assistant_runtime_id,
            assistant_card.id.as_str(),
            WorkerSessionKind::CodexCard,
        ),
        (
            &worker_runtime_id,
            worker_card.id.as_str(),
            WorkerSessionKind::CodexCard,
        ),
        (
            &chat_runtime_id,
            chat_card.id.as_str(),
            WorkerSessionKind::CodexCard,
        ),
    ] {
        let mut snapshot = snapshot.clone();
        snapshot.last_thread_id = Some(format!("thread-{card_id}"));
        session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: runtime_id.clone(),
                card_id: card_id.to_string(),
                kind,
                agent_provider: Some(AgentProvider::Codex),
                // `turn_pending` is the state the bug bites in: a turn was in
                // flight when the kernel went down.
                status: WorkerSessionState::TurnPending,
                terminal_run_id: None,
                thread_id: Some(format!("thread-{card_id}")),
                session_id: None,
                active_turn_id: None,
                handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
                spawn_op_id: None,
                now_ms: now_ms(),
            },
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();

    // Spec-push backlog accumulated on the wave while the kernel was down.
    let bus = EventBus::new();
    let role_cache = repo.card_role_cache().clone();
    let cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&cove_cache).await.unwrap();
    let scope = EventScope::Wave {
        wave: wave.id.clone(),
        cove: cove.id.clone(),
    };
    repo.log_pure_event(
        ActorId::User,
        scope.clone(),
        None,
        &bus,
        &role_cache,
        &cove_cache,
        Event::TaskCompleted {
            idempotency_key: format!("{}:only", wave.id.as_str()),
            result: json!({ "ok": true }),
            artifacts: Vec::new(),
            agent_message: None,
        },
    )
    .await
    .unwrap();
    repo.log_pure_event(
        ActorId::User,
        scope,
        None,
        &bus,
        &role_cache,
        &cove_cache,
        Event::WaveReportEdited {
            wave_id: wave.id.clone(),
            card_id: spec_card.id.clone(),
            author: EditAuthor::User,
            author_plugin_id: None,
            edit_id: new_id(),
            summary_before: String::new(),
            summary_after: String::new(),
            body_before: "before".into(),
            body_after: "after".into(),
            agent_message: None,
        },
    )
    .await
    .unwrap();

    // The selector itself, before any harness is built.
    let mut selected = repo
        .session_projection_recover_harnesses_on_boot()
        .await
        .unwrap()
        .into_iter()
        .map(|runtime| runtime.id)
        .collect::<Vec<_>>();
    selected.sort();
    let mut expected = vec![
        spec_runtime_id.clone(),
        assistant_runtime_id.clone(),
        chat_runtime_id.clone(),
    ];
    expected.sort();
    assert_eq!(
        selected, expected,
        "boot recovery must select the spec harness, the wave assistant and the \
         cove chat — and must not select the dispatched codex worker \
         ({worker_runtime_id})"
    );

    let registry = HarnessRegistry::new();
    let recovered = recover_harnesses_on_boot(
        repo.clone(),
        EventBus::new(),
        role_cache.clone(),
        cove_cache.clone(),
        SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None),
        &registry,
    )
    .await
    .unwrap();
    assert_eq!(recovered, 3, "spec + assistant + cove chat");
    assert!(
        registry.get(&assistant_runtime_id).is_some(),
        "the assistant must come back REGISTERED; a dormant runtime is a user \
         waiting forever for a reply"
    );
    assert!(registry.get(&spec_runtime_id).is_some());
    assert!(registry.get(&chat_runtime_id).is_some());
    assert!(
        registry.get(&worker_runtime_id).is_none(),
        "a dispatched codex worker is not a harness"
    );

    let stored = |runtime_id: String| {
        let repo = repo.clone();
        async move {
            let runtime = repo
                .session_projection_by_id(&runtime_id)
                .await
                .unwrap()
                .unwrap();
            let snapshot: HarnessSnapshot = serde_json::from_value(
                runtime
                    .handle_state_json
                    .expect("recovered runtime keeps its handle state"),
            )
            .unwrap();
            snapshot.pending_queue
        }
    };

    let spec_queue = stored(spec_runtime_id.clone()).await;
    assert_eq!(
        spec_queue.len(),
        2,
        "the spec still catches up on its own backlog: {spec_queue:?}"
    );
    assert!(
        spec_queue
            .iter()
            .any(|obs| matches!(obs, Observation::TaskCompleted { .. }))
    );
    assert!(
        spec_queue
            .iter()
            .any(|obs| matches!(obs, Observation::ReportEdited { .. }))
    );

    let assistant_queue = stored(assistant_runtime_id.clone()).await;
    assert!(
        assistant_queue.is_empty(),
        "the assistant was handed the SPEC's backlog on recovery — it would open \
         the conversation by reporting somebody else's task results: \
         {assistant_queue:?}"
    );
    let chat_queue = stored(chat_runtime_id.clone()).await;
    assert!(
        chat_queue.is_empty(),
        "a cove chat is not a spec-push recipient either: {chat_queue:?}"
    );

    for runtime_id in [&spec_runtime_id, &assistant_runtime_id, &chat_runtime_id] {
        if let Some(handle) = registry.get(runtime_id) {
            handle.shutdown().await.unwrap();
        }
    }
}
