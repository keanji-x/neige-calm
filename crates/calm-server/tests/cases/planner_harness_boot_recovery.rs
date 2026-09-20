use std::sync::Arc;
use std::time::Duration;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_create_with_id_tx, session_mark_queue_harvested_tx,
    session_mark_superseded_runtime_tx, session_prepare_deferred_planner_tx,
    session_start_runtime_tx, session_supersede_and_start_tx,
};
use calm_server::error::CalmError;
use calm_server::event::{EditAuthor, Event, EventBus, EventScope};
use calm_server::harness::{
    ClaimMode, DeferredRecoveryParams, HarnessConfig, HarnessPhaseTag, HarnessRegistry,
    HarnessSnapshot, Observation, PlannerHarness, PlannerHarnessParams, QueueEntry,
    recover_harnesses_deferred, recover_harnesses_on_boot, spawn_recovered_harness,
};
use calm_server::ids::{ActorId, CardId, TrackId};
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack, new_id, now_ms};
use calm_server::operation::TxOutput;
use calm_server::operation::planner_harness_start_adapter::PlannerHarnessStartOperationPayload;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use serde_json::json;
use tempfile::TempDir;

/// The stored `planner-harness-start` payload every boot-recovery fixture needs, differing only in `goal`.
fn start_payload(
    track_id: &str,
    planner_card_id: &CardId,
    cwd: &str,
    goal: Option<&str>,
) -> serde_json::Value {
    serde_json::to_value(PlannerHarnessStartOperationPayload {
        actor: ActorId::User,
        track_id: track_id.to_string(),
        planner_card_id: planner_card_id.clone(),
        report_card_id: None,
        sort: None,
        cwd: cwd.to_string(),
        goal: goal.map(ToOwned::to_owned),
        reset_harness_items: false,
        force_new_thread: true,
        profile: Default::default(),
        create_card: None,
        opening_briefing: None,
        first_message: None,
        create_request_sha256: None,
    })
    .expect("planner-harness-start payload serializes")
}

fn app_state_for_boot_test_with_role_cache(
    repo: Arc<SqlxRepo>,
    role_cache: calm_server::card_role_cache::CardRoleCache,
) -> AppState {
    let events = EventBus::new();
    let area_cache = calm_server::track_area_cache::TrackAreaCache::new();
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
            WriteContext::new(role_cache.clone(), area_cache.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(role_cache),
        Some(area_cache),
    )
}

fn app_state_for_boot_test(repo: Arc<SqlxRepo>) -> AppState {
    app_state_for_boot_test_with_role_cache(
        repo,
        calm_server::card_role_cache::CardRoleCache::new(),
    )
}

/// A track's planner card, minted with the role production gives it: `Repo::card_create` defaults to `CardRole::Worker`,
/// and recovery replays the planner push stream only for `CardRole::Planner`. The payload is the production shape from `planner_harness_card_payload`.
async fn seed_planner_card_row(repo: &SqlxRepo, track_id: &TrackId) -> calm_server::model::Card {
    let mut tx = repo.pool().begin().await.unwrap();
    let card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            track_id: track_id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: calm_server::routes::tracks::planner_harness_card_payload(None),
        },
        CardRole::Planner,
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
    let area = repo
        .area_create(NewArea {
            name: "plain-chat-recovery".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
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
    sqlx::query("UPDATE tracks SET purpose = 'area-chat' WHERE id = ?1")
        .bind(track.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    let chat = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "harness_profile": "plain_chat"}),
        })
        .await
        .unwrap();
    let pty = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
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
        repo.track_area_cache().clone(),
        SharedCodexAppServer::new_stub(repo.clone()),
        &registry,
        &calm_server::harness::new_track_delete_locks(),
        recovered.into_iter().next().unwrap(),
        ClaimMode::Replace,
    )
    .await
    .expect("marked Worker plain-chat runtime must pass the area-chat recovery fence");
    let handle = outcome
        .installed()
        .expect("plain-chat runtime must install a harness");
    assert!(registry.get(&chat_runtime_id).is_some());
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn direct_recovery_boundary_rejects_area_chat_planner_runtime() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "chat-planner-recovery-fence".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "chat planner".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET purpose = 'area-chat' WHERE id = ?1")
        .bind(track.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some("thread-chat-planner-fence".into());
    let mut tx = repo.pool().begin().await.unwrap();
    let card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            track_id: track.id,
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        },
        CardRole::Planner,
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
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some("thread-chat-planner-fence".into()),
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
        repo.track_area_cache().clone(),
        SharedCodexAppServer::new_stub(repo.clone()),
        &registry,
        &calm_server::harness::new_track_delete_locks(),
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

/// Seed an area/track/card + recoverable SharedPlanner runtime row; returns the
/// runtime id.
async fn seed_recoverable_runtime(repo: &Arc<SqlxRepo>, tag: &str, thread_id: &str) -> String {
    let area = repo
        .area_create(NewArea {
            name: tag.into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
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
            track_id: track.id,
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
            kind: WorkerSessionKind::SharedPlanner,
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
async fn boot_recovery_skips_area_chat_planner_and_recovers_later_valid_runtime() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let declined_id = seed_recoverable_runtime(&repo, "declined-first", "thread-declined").await;
    let declined = repo
        .session_projection_by_id(&declined_id)
        .await
        .unwrap()
        .unwrap();
    let declined_card = repo.card_get(&declined.card_id).await.unwrap().unwrap();
    sqlx::query("UPDATE cards SET role = 'planner' WHERE id = ?1")
        .bind(declined_card.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    repo.card_role_cache().insert(
        declined_card.id.clone(),
        CardRole::Planner,
        declined_card.track_id.clone(),
    );
    sqlx::query("UPDATE tracks SET purpose = 'area-chat' WHERE id = ?1")
        .bind(declined_card.track_id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    let valid_id = seed_recoverable_runtime(&repo, "valid-second", "thread-valid").await;
    let registry = HarnessRegistry::new();

    let recovered = recover_harnesses_on_boot(
        repo.clone(),
        EventBus::new(),
        repo.card_role_cache().clone(),
        repo.track_area_cache().clone(),
        SharedCodexAppServer::new_stub(repo.clone()),
        &registry,
        &calm_server::harness::new_track_delete_locks(),
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
    let area = repo
        .area_create(NewArea {
            name: "boot".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
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
            track_id: track.id,
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
        QueueEntry::entries_from_observations_for_test(vec![Observation::TrackGoal {
            text: "recover me".into(),
        }]),
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
            kind: WorkerSessionKind::SharedPlanner,
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
        calm_server::track_area_cache::TrackAreaCache::new(),
        daemon,
        &registry,
        &calm_server::harness::new_track_delete_locks(),
    )
    .await
    .unwrap();
    assert_eq!(recovered, 1);
    let handle = registry.get(&runtime_id).expect("recovered harness");
    let restored = handle.snapshot().await;
    assert_eq!(restored.push_watermark, 42);
    assert_eq!(restored.pending_observations().len(), 1);
    assert_eq!(restored.last_turn_id.as_deref(), Some("turn-recovered"));
    handle.shutdown().await.unwrap();
}

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
    // Deferred-armed, NOT run: nothing is recovered while the daemon stays down.
    assert_eq!(recovered, 0);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(state.harness.get(&untouched_runtime_id).is_none());
    assert!(state.harness.get(&user_runtime_id).is_none());

    let user_runtime = repo
        .session_projection_by_id(&user_runtime_id)
        .await
        .unwrap()
        .unwrap();
    let user_handle = spawn_recovered_harness(
        repo.clone(),
        state.events.clone(),
        calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::track_area_cache::TrackAreaCache::new(),
        state.shared_codex_appserver.clone(),
        &state.harness,
        &calm_server::harness::new_track_delete_locks(),
        user_runtime,
        ClaimMode::Replace,
    )
    .await
    .unwrap()
    .installed()
    .expect("user resume registers a harness");

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

    tokio::time::sleep(Duration::from_millis(100)).await;
    user_handle
        .observe(Observation::TrackGoal {
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

/// The user's registration lands after the per-runtime eligibility check (fixtures-only hook) and before its claim, so `try_reserve` returns None.
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

    // The user's harness, built but NOT registered yet — the hook lands it inside the eligibility→claim window.
    let user_handle = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: runtime_id.clone(),
        track_id: TrackId::from(
            repo.card_get(&runtime.card_id)
                .await
                .unwrap()
                .unwrap()
                .track_id
                .to_string(),
        ),
        card_id: CardId::from(runtime.card_id.clone()),
        thread_id: runtime.thread_id.clone(),
        repo: repo.clone(),
        events: events.clone(),
        card_role_cache: calm_server::card_role_cache::CardRoleCache::new(),
        track_area_cache: calm_server::track_area_cache::TrackAreaCache::new(),
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
        track_area_cache: calm_server::track_area_cache::TrackAreaCache::new(),
        daemon: daemon.clone(),
        registry: registry.clone(),
        track_delete_locks: calm_server::harness::new_track_delete_locks(),
        post_eligibility_hook: Some(post_eligibility_hook),
    }));
    daemon.publish_readiness_for_test(1, true);
    tokio::time::timeout(Duration::from_secs(5), driver)
        .await
        .expect("deferred recovery pass must complete")
        .unwrap();

    assert_eq!(hook_fired.load(Ordering::SeqCst), 1);
    user_handle
        .observe(Observation::TrackGoal {
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

/// The daemon leaves Running inside the eligibility→claim window (the fixtures hook fires at its start); the deferred
/// task must abandon the pass without reserving, re-arm, and recover only after the daemon heals again.
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
            // First pass only: the daemon fails (readiness invalidated with the outgoing generation) inside the eligibility→claim window.
            if hook_fired_in_hook.fetch_add(1, Ordering::SeqCst) == 0 {
                hook_daemon.publish_readiness_for_test(1, false);
            }
        });

    let driver = tokio::spawn(recover_harnesses_deferred(DeferredRecoveryParams {
        repo: repo.clone(),
        events,
        card_role_cache: calm_server::card_role_cache::CardRoleCache::new(),
        track_area_cache: calm_server::track_area_cache::TrackAreaCache::new(),
        daemon: daemon.clone(),
        registry: registry.clone(),
        track_delete_locks: calm_server::harness::new_track_delete_locks(),
        post_eligibility_hook: Some(post_eligibility_hook),
    }));

    daemon.publish_readiness_for_test(1, true);
    tokio::time::timeout(Duration::from_secs(5), async {
        while hook_fired.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("pass 1 must reach the post-eligibility window");

    // Nothing may be installed against the stale generation, and the task must re-arm rather than finish its pass.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        registry.get(&runtime_id).is_none(),
        "no harness may be installed against a daemon that left Running during replay"
    );
    assert!(
        !driver.is_finished(),
        "the deferred task must abandon the pass and re-arm, not exit"
    );

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
    let area = repo
        .area_create(NewArea {
            name: "boot-deferred".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
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
            track_id: track.id,
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
        QueueEntry::entries_from_observations_for_test(vec![Observation::TaskCompleted {
            idempotency_key: "deferred-boot".into(),
            result: json!({"ok": true}),
        }]),
    );
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some("thread-deferred".into());
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
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
        calm_server::track_area_cache::TrackAreaCache::new(),
        daemon.clone(),
        &registry,
        &calm_server::harness::new_track_delete_locks(),
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
    let area = repo
        .area_create(NewArea {
            name: "boot-replay".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
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
    let card = seed_planner_card_row(&repo, &track.id).await;
    let bus = EventBus::new();
    let role_cache = calm_server::card_role_cache::CardRoleCache::new();
    let area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    let missed_id = repo
        .log_pure_event(
            ActorId::User,
            EventScope::Track {
                track: track.id.clone(),
                area: area.id.clone(),
            },
            None,
            &bus,
            &role_cache,
            &area_cache,
            Event::TrackReportEdited {
                track_id: track.id.clone(),
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
            EventScope::Track {
                track: track.id.clone(),
                area: area.id.clone(),
            },
            None,
            &bus,
            &role_cache,
            &area_cache,
            Event::TrackReportEdited {
                track_id: track.id.clone(),
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
            kind: WorkerSessionKind::SharedPlanner,
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
        calm_server::track_area_cache::TrackAreaCache::new(),
        daemon,
        &registry,
        &calm_server::harness::new_track_delete_locks(),
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
    assert_eq!(stored.pending_observations().len(), 2);
    assert!(stored.pending_observations().iter().any(|obs| {
        matches!(obs, Observation::ReportEdited { body, .. } if body == "queued body")
    }));
    assert!(stored.pending_observations().iter().any(|obs| {
        matches!(obs, Observation::ReportEdited { body, .. } if body == "missed body")
    }));
    let handle = registry.get(&runtime_id).expect("recovered harness");
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn boot_recovery_skips_terminal_tracks() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "boot-terminal".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
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
    sqlx::query("UPDATE tracks SET lifecycle = 'done' WHERE id = ?1")
        .bind(track.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            track_id: track.id,
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
        QueueEntry::entries_from_observations_for_test(vec![Observation::TrackGoal {
            text: "do not recover".into(),
        }]),
    );
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some("thread-terminal".into());
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
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
        calm_server::track_area_cache::TrackAreaCache::new(),
        daemon,
        &registry,
        &calm_server::harness::new_track_delete_locks(),
    )
    .await
    .unwrap();
    assert_eq!(recovered, 0);
    assert!(registry.get(&runtime_id).is_none());
}

#[tokio::test]
async fn boot_recovery_skips_deferred_worker_session_phantom_ghost() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "boot-phantom".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
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
            track_id: track.id,
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
        QueueEntry::entries_from_observations_for_test(vec![Observation::TrackGoal {
            text: "must not recover".into(),
        }]),
    );
    snapshot.phase = HarnessPhaseTag::Idle;

    let mut tx = repo.pool().begin().await.unwrap();
    session_prepare_deferred_planner_tx(
        &mut tx,
        &WorkerSessionInit {
            id: placeholder_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
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
        calm_server::track_area_cache::TrackAreaCache::new(),
        daemon,
        &registry,
        &calm_server::harness::new_track_delete_locks(),
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
    let (card_id, track_id, old_runtime_id, placeholder_id, op_id) = {
        let repo = Arc::new(SqlxRepo::open(&db_url).await.unwrap());
        let area = repo
            .area_create(NewArea {
                name: "phase2-crash".into(),
                color: "#111111".into(),
                sort: None,
            })
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                template_input: None,
                area_id: area.id,
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
                track_id: track.id.clone(),
                title: None,
                kind: "codex".into(),
                sort: None,
                payload: json!({
                    "schemaVersion": 1,
                    "codex_source": "shared",
                    "planner_harness": true
                }),
            })
            .await
            .unwrap();
        let old_runtime_id = new_id();
        let old_snapshot = HarnessSnapshot::initial(0, vec![]);
        let placeholder_id = new_id();
        let placeholder_snapshot = HarnessSnapshot::initial(0, vec![]);
        let now = now_ms();
        let payload = start_payload(
            track.id.as_ref(),
            &card.id,
            &track.workspace.path,
            Some("recover after crash"),
        );
        let mut output = TxOutput::new(
            "card",
            Some(card.id.to_string()),
            serde_json::to_value(&card).unwrap(),
        );
        output.data = json!({
            "card_id": card.id.to_string(),
            "track_id": track.id.to_string(),
            "runtime_id": placeholder_id.clone(),
            "runtime_deferred": true,
            "cwd": track.workspace.path.clone(),
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
                kind: WorkerSessionKind::SharedPlanner,
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
        session_prepare_deferred_planner_tx(
            &mut tx,
            &WorkerSessionInit {
                id: placeholder_id.clone(),
                card_id: card.id.to_string(),
                kind: WorkerSessionKind::SharedPlanner,
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
               VALUES (?1, ?2, 'planner-harness-start', NULL, ?3,
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
            track.id.to_string(),
            old_runtime_id,
            placeholder_id,
            op_id,
        )
    };

    let repo = Arc::new(SqlxRepo::open(&db_url).await.unwrap());
    let role_cache = calm_server::card_role_cache::CardRoleCache::new();
    role_cache.insert(
        CardId::from(card_id.clone()),
        CardRole::Planner,
        TrackId::from(track_id.clone()),
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

/// The boot replay applies the same gated-self-report consultation as the live push: a gated task's `task.completed`
/// is not replayed (the gate verdict wakes the planner); a stale `task.failed` against a gate-owned row is suppressed too.
#[tokio::test]
async fn boot_replay_suppresses_gated_self_report_and_replays_gate_result() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "gate-replay".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
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
    let card = seed_planner_card_row(&repo, &track.id).await;

    let mk_task = |key: &str, gate: Option<String>| calm_server::model::Task {
        id: format!("{}:{key}", track.id.as_str()),
        track_id: track.id.as_str().to_string(),
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
    // A gated task whose worker genuinely failed pre-gate: the failure landed on the row, so its `task.failed` replays.
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
    let area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    repo.seed_track_area_cache(&area_cache).await.unwrap();
    let scope = EventScope::Track {
        track: track.id.clone(),
        area: area.id.clone(),
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
        // A stale/retried `task.failed` against the gated row the gate owns (`verifying`): never landed on the row, must NOT replay.
        Event::TaskFailed {
            idempotency_key: gated_id.clone(),
            reason: "stale worker claim".into(),
            details: None,
            agent_message: None,
        },
        // ... while the genuine pre-gate worker failure replays.
        Event::TaskFailed {
            idempotency_key: gated_failed_id.clone(),
            reason: "worker said no".into(),
            details: None,
            agent_message: None,
        },
    ] {
        repo.log_pure_event(
            ActorId::User,
            scope.clone(),
            None,
            &bus,
            &role_cache,
            &area_cache,
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
        &area_cache,
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
            kind: WorkerSessionKind::SharedPlanner,
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
        calm_server::track_area_cache::TrackAreaCache::new(),
        daemon,
        &registry,
        &calm_server::harness::new_track_delete_locks(),
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
        stored.pending_observations().len(),
        3,
        "ungated self-report + gate result + genuine pre-gate failure, \
         never the gated self-report or the stale gated task.failed: {:?}",
        stored.pending_observations()
    );
    assert!(
        stored.pending_observations().iter().any(|obs| matches!(
            obs,
            Observation::TaskCompleted { idempotency_key, .. } if idempotency_key == &ungated_id
        )),
        "{:?}",
        stored.pending_observations()
    );
    assert!(
        stored.pending_observations().iter().any(|obs| matches!(
            obs,
            Observation::TaskGateResult { idempotency_key, passed: true, .. }
                if idempotency_key == &gated_id
        )),
        "{:?}",
        stored.pending_observations()
    );
    assert!(
        !stored.pending_observations().iter().any(|obs| matches!(
            obs,
            Observation::TaskCompleted { idempotency_key, .. } if idempotency_key == &gated_id
        )),
        "gated self-report must be suppressed in replay (§6.5): {:?}",
        stored.pending_observations()
    );
    assert!(
        !stored.pending_observations().iter().any(|obs| matches!(
            obs,
            Observation::TaskFailed { idempotency_key, .. } if idempotency_key == &gated_id
        )),
        "stale task.failed against the verifying gated row must be suppressed in replay: {:?}",
        stored.pending_observations()
    );
    assert!(
        stored.pending_observations().iter().any(|obs| matches!(
            obs,
            Observation::TaskFailed { idempotency_key, .. }
                if idempotency_key == &gated_failed_id
        )),
        "genuine pre-gate worker failure must replay as today: {:?}",
        stored.pending_observations()
    );
    let handle = registry.get(&runtime_id).expect("recovered harness");
    handle.shutdown().await.unwrap();
}

/// A kernel restart mid-conversation on an ordinary track. All four recovery classes sit side by side so a selector
/// widened too far is red as well: the real codex worker must stay out, and the area chat must stay in.
#[tokio::test]
async fn boot_recovery_registers_the_assistant_without_replaying_the_planner_backlog() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "assistant-recovery".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
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
    // An area chat track alongside it: that recovery class must keep working.
    let chat_track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "area chat".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET purpose = 'area-chat' WHERE id = ?1")
        .bind(chat_track.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();

    let mut tx = repo.pool().begin().await.unwrap();
    let mk = |track_id: TrackId, payload: serde_json::Value| NewCard {
        track_id,
        title: None,
        kind: "codex".into(),
        sort: None,
        payload,
    };
    // The planner card: the only legitimate recipient of the planner push stream.
    let planner_card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        mk(track.id.clone(), json!({"schemaVersion": 1})),
        CardRole::Planner,
        false,
        repo.card_role_cache(),
    )
    .await
    .unwrap();
    // The track assistant, exactly as `planner_harness_start_adapter` mints it.
    let assistant_card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        mk(
            track.id.clone(),
            json!({"schemaVersion": 1, "harness_profile": "assistant"}),
        ),
        CardRole::Assistant,
        false,
        repo.card_role_cache(),
    )
    .await
    .unwrap();
    // A real dispatched codex worker on the same track: never harness-recovered.
    let worker_card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        mk(track.id.clone(), json!({"schemaVersion": 1})),
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
            chat_track.id.clone(),
            json!({"schemaVersion": 1, "harness_profile": "plain_chat"}),
        ),
        CardRole::Worker,
        false,
        repo.card_role_cache(),
    )
    .await
    .unwrap();

    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    // Keep the snapshot in a non-issuable phase: an Idle harness may drain the replayed planner queue on its first
    // run-loop tick before the assertions below can observe the catch-up.
    snapshot.phase = HarnessPhaseTag::TurnRunning;
    let planner_runtime_id = new_id();
    let assistant_runtime_id = new_id();
    let worker_runtime_id = new_id();
    let chat_runtime_id = new_id();
    for (runtime_id, card_id, kind) in [
        (
            &planner_runtime_id,
            planner_card.id.as_str(),
            WorkerSessionKind::SharedPlanner,
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
        snapshot.last_turn_id = Some(format!("turn-{card_id}"));
        session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: runtime_id.clone(),
                card_id: card_id.to_string(),
                kind,
                agent_provider: Some(AgentProvider::Codex),
                // `turn_pending`: a turn was in flight when the kernel went down.
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

    // Planner-push backlog accumulated on the track while the kernel was down.
    let bus = EventBus::new();
    let role_cache = repo.card_role_cache().clone();
    let area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    repo.seed_track_area_cache(&area_cache).await.unwrap();
    let scope = EventScope::Track {
        track: track.id.clone(),
        area: area.id.clone(),
    };
    repo.log_pure_event(
        ActorId::User,
        scope.clone(),
        None,
        &bus,
        &role_cache,
        &area_cache,
        Event::TaskCompleted {
            idempotency_key: format!("{}:only", track.id.as_str()),
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
        &area_cache,
        Event::TrackReportEdited {
            track_id: track.id.clone(),
            card_id: planner_card.id.clone(),
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
        planner_runtime_id.clone(),
        assistant_runtime_id.clone(),
        chat_runtime_id.clone(),
    ];
    expected.sort();
    assert_eq!(
        selected, expected,
        "boot recovery must select the planner harness, the track assistant and the \
         area chat — and must not select the dispatched codex worker \
         ({worker_runtime_id})"
    );

    let registry = HarnessRegistry::new();
    let recovered = recover_harnesses_on_boot(
        repo.clone(),
        EventBus::new(),
        role_cache.clone(),
        area_cache.clone(),
        SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None),
        &registry,
        &calm_server::harness::new_track_delete_locks(),
    )
    .await
    .unwrap();
    assert_eq!(recovered, 3, "planner + assistant + area chat");
    assert!(
        registry.get(&assistant_runtime_id).is_some(),
        "the assistant must come back REGISTERED; a dormant runtime is a user \
         waiting forever for a reply"
    );
    assert!(registry.get(&planner_runtime_id).is_some());
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
            snapshot.pending_observations()
        }
    };

    let planner_queue = stored(planner_runtime_id.clone()).await;
    assert_eq!(
        planner_queue.len(),
        2,
        "the planner still catches up on its own backlog: {planner_queue:?}"
    );
    assert!(
        planner_queue
            .iter()
            .any(|obs| matches!(obs, Observation::TaskCompleted { .. }))
    );
    assert!(
        planner_queue
            .iter()
            .any(|obs| matches!(obs, Observation::ReportEdited { .. }))
    );

    let assistant_queue = stored(assistant_runtime_id.clone()).await;
    assert!(
        assistant_queue.is_empty(),
        "the assistant was handed the PLANNER's backlog on recovery — it would open \
         the conversation by reporting somebody else's task results: \
         {assistant_queue:?}"
    );
    let chat_queue = stored(chat_runtime_id.clone()).await;
    assert!(
        chat_queue.is_empty(),
        "an area chat is not a planner-push recipient either: {chat_queue:?}"
    );

    for runtime_id in [&planner_runtime_id, &assistant_runtime_id, &chat_runtime_id] {
        if let Some(handle) = registry.get(runtime_id) {
            handle.shutdown().await.unwrap();
        }
    }
}

/// A start re-driven after a crash must hand the racer's undelivered sentence to the runtime it ACTUALLY starts.
/// `spawn_side_effect` builds the harness from `output` and then `persist_snapshot()` overwrites the row, so a
/// harvest that reached only the row is erased; assert on the started runtime.
#[tokio::test]
async fn a_redriven_start_hands_the_raced_in_runtimes_sentence_to_the_harness_it_starts() {
    const STRANDED: &str = "the racer never got to say this";

    let tmp = TempDir::new().unwrap();
    let db_url = sqlite_url(&tmp, "raced-in-harvest.db");
    let (card_id, track_id, racer_id, placeholder_id, op_id) = {
        let repo = Arc::new(SqlxRepo::open(&db_url).await.unwrap());
        let area = repo
            .area_create(NewArea {
                name: "raced-in-harvest".into(),
                color: "#111111".into(),
                sort: None,
            })
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                template_input: None,
                area_id: area.id,
                title: "raced in".into(),
                sort: None,
                cwd: "/tmp".into(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: calm_server::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let card = seed_planner_card_row(&repo, &track.id).await;

        let placeholder_id = new_id();
        let placeholder_snapshot = HarnessSnapshot::initial(0, vec![]);
        // The racer carries a sentence a human typed into it during the window.
        let racer_id = new_id();
        let racer_snapshot = HarnessSnapshot::initial(
            0,
            vec![QueueEntry::user_message(STRANDED.into(), None, Vec::new())],
        );
        let now = now_ms();
        let payload = start_payload(track.id.as_ref(), &card.id, &track.workspace.path, None);
        let mut output = TxOutput::new(
            "card",
            Some(card.id.to_string()),
            serde_json::to_value(&card).unwrap(),
        );
        // Exactly what `prepare_tx` committed: the placeholder's own snapshot, which knows nothing about the racer.
        // The retiring key (and the `fail_runtime` step args below) are read back by `planner_harness_start_adapter.rs`
        // from rows written by shipped binaries; spelling them otherwise builds a payload the adapter cannot read.
        output.data = json!({
            "card_id": card.id.to_string(),
            "track_id": track.id.to_string(),
            "runtime_id": placeholder_id.clone(),
            "runtime_deferred": true,
            "cwd": track.workspace.path.clone(),
            "goal": null,
            "report_card_id": null,
            "snapshot": serde_json::to_value(&placeholder_snapshot).unwrap(),
        });
        let op_id = new_id();

        let mut tx = repo.pool().begin().await.unwrap();
        // 1. the deferred placeholder this operation minted...
        session_prepare_deferred_planner_tx(
            &mut tx,
            &WorkerSessionInit {
                id: placeholder_id.clone(),
                card_id: card.id.to_string(),
                kind: WorkerSessionKind::SharedPlanner,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Starting,
                terminal_run_id: None,
                thread_id: None,
                session_id: None,
                active_turn_id: None,
                handle_state_json: Some(serde_json::to_value(&placeholder_snapshot).unwrap()),
                spawn_op_id: None,
                now_ms: now,
            },
        )
        .await
        .unwrap();
        // 2. ...displaced by a runtime that took the card's active slot while the thread was being minted; this pair
        //    deliberately does NOT stamp the placeholder: nothing has taken its queue.
        session_supersede_and_start_tx(
            &mut tx,
            &placeholder_id,
            WorkerSessionInit {
                id: racer_id.clone(),
                card_id: card.id.to_string(),
                kind: WorkerSessionKind::SharedPlanner,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Idle,
                terminal_run_id: None,
                thread_id: Some("thread-racer".into()),
                session_id: None,
                active_turn_id: None,
                handle_state_json: Some(serde_json::to_value(&racer_snapshot).unwrap()),
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
               VALUES (?1, ?2, 'planner-harness-start', NULL, ?3,
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
            track.id.to_string(),
            racer_id,
            placeholder_id,
            op_id,
        )
    };

    let repo = Arc::new(SqlxRepo::open(&db_url).await.unwrap());
    let role_cache = calm_server::card_role_cache::CardRoleCache::new();
    role_cache.insert(
        CardId::from(card_id.clone()),
        CardRole::Planner,
        TrackId::from(track_id.clone()),
    );
    let state = app_state_for_boot_test_with_role_cache(repo.clone(), role_cache)
        .with_shared_codex_appserver(SharedCodexAppServer::new_fake_running_with_pending(
            repo.clone(),
            None,
        ));

    calm_server::recover_operations_on_boot(&state)
        .await
        .unwrap();

    let phase: String = sqlx::query_scalar("SELECT phase FROM operations WHERE id = ?1")
        .bind(&op_id)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(phase, "succeeded", "premise: the re-drive must complete");
    let racer_state: String = sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id = ?1")
        .bind(&racer_id)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(
        racer_state, "superseded",
        "premise: the re-drive must retire the runtime that raced in"
    );
    assert!(
        state.harness.get(&placeholder_id).is_some(),
        "premise: the re-drive must start the placeholder's harness"
    );

    // THE assertion: the daemon got it. The row is correct by construction, so a harvest that reached the row and nowhere else would still pass a row assertion.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let delivered = loop {
        let handed_over =
            serde_json::to_string(&state.shared_codex_appserver.started_turns_for_test())
                .unwrap_or_default()
                .matches(STRANDED)
                .count();
        if handed_over > 0 || std::time::Instant::now() >= deadline {
            break handed_over;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert_eq!(
        delivered, 1,
        "the harvested sentence must be delivered by the runtime the re-drive actually started, \
         exactly once"
    );
    // The undo journal names the racer. Structural, deliberately: the end-to-end witness would need this operation to
    // fail after its app-server transaction committed, and the reachable failure injections all land before it.
    let tx_output: String =
        sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE id = ?1")
            .bind(&op_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    let tx_output: serde_json::Value = serde_json::from_str(&tx_output).unwrap();
    let journal = tx_output["data"]["harvested_from"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        journal.iter().any(
            |entry| entry["worker_session_id"] == serde_json::json!(racer_id)
                && entry["messages"].to_string().contains(STRANDED)
        ),
        "the app-server transaction's harvest must reach the undo journal, or a later failure \
         strands the racer's sentence on a `failed` runtime: {journal:#?}"
    );

    let racer_stamp: Option<i64> =
        sqlx::query_scalar("SELECT queue_harvested_at_ms FROM worker_sessions WHERE id = ?1")
            .bind(&racer_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert!(
        racer_stamp.is_some(),
        "and the row it came from must be stamped, or the next start takes it again"
    );

    if let Some(handle) = state.harness.remove(&placeholder_id) {
        handle.shutdown().await.unwrap();
    }
}

/// A harness is started from the queue on its OWN ROW, never from the copy in the operation output: the pending queue
/// is shared state later mints move between rows. Staged at `TxCommitted`, not `SpawnStarted`: `app_server_interact`
/// writes the row on the way through.
#[tokio::test]
async fn a_redriven_start_takes_the_queue_from_the_row_not_from_the_carried_output() {
    const MOVED_AWAY: &str = "this sentence already belongs to somebody else";

    let tmp = TempDir::new().unwrap();
    let db_url = sqlite_url(&tmp, "row-is-the-single-home.db");
    let (card_id, track_id, worker_session_id, op_id) = {
        let repo = Arc::new(SqlxRepo::open(&db_url).await.unwrap());
        let area = repo
            .area_create(NewArea {
                name: "single-home".into(),
                color: "#111111".into(),
                sort: None,
            })
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                template_input: None,
                area_id: area.id,
                title: "single home".into(),
                sort: None,
                cwd: "/tmp".into(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: calm_server::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let card = seed_planner_card_row(&repo, &track.id).await;

        let worker_session_id = new_id();
        // The row: the queue is empty, because a mint in between moved the sentence to another runtime.
        let row_snapshot = HarnessSnapshot::initial(0, vec![]);
        // The carried output: still holds it, frozen at `prepare_tx` time.
        let carried_snapshot = HarnessSnapshot::initial(
            0,
            vec![QueueEntry::user_message(
                MOVED_AWAY.into(),
                None,
                Vec::new(),
            )],
        );
        let now = now_ms();
        let payload = start_payload(track.id.as_ref(), &card.id, &track.workspace.path, None);
        let mut output = TxOutput::new(
            "card",
            Some(card.id.to_string()),
            serde_json::to_value(&card).unwrap(),
        );
        output.data = json!({
            "card_id": card.id.to_string(),
            "track_id": track.id.to_string(),
            "runtime_id": worker_session_id.clone(),
            "runtime_deferred": true,
            "cwd": track.workspace.path.clone(),
            "goal": null,
            "report_card_id": null,
            // No `codex_thread_id`: with one already in the output the app-server phase short-circuits and never writes the row, the writer under test.
            "snapshot": serde_json::to_value(&carried_snapshot).unwrap(),
        });
        let op_id = new_id();

        let mut tx = repo.pool().begin().await.unwrap();
        session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: worker_session_id.clone(),
                card_id: card.id.to_string(),
                kind: WorkerSessionKind::SharedPlanner,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Idle,
                terminal_run_id: None,
                thread_id: None,
                session_id: None,
                active_turn_id: None,
                handle_state_json: Some(serde_json::to_value(&row_snapshot).unwrap()),
                spawn_op_id: None,
                now_ms: now,
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
               VALUES (?1, ?2, 'planner-harness-start', NULL, ?3,
                       'card', ?4, ?5, ?6, ?7, 'tx_committed', ?8, ?8)"#,
        )
        .bind(&op_id)
        .bind(new_id())
        .bind(new_id())
        .bind(card.id.as_str())
        .bind(serde_json::to_string(&json!({"type": "card", "id": card.id})).unwrap())
        .bind(serde_json::to_string(&payload).unwrap())
        .bind(serde_json::to_string(&output).unwrap())
        .bind(now + 1)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();

        (
            card.id.to_string(),
            track.id.to_string(),
            worker_session_id,
            op_id,
        )
    };

    let repo = Arc::new(SqlxRepo::open(&db_url).await.unwrap());
    let role_cache = calm_server::card_role_cache::CardRoleCache::new();
    role_cache.insert(
        CardId::from(card_id.clone()),
        CardRole::Planner,
        TrackId::from(track_id.clone()),
    );
    let state = app_state_for_boot_test_with_role_cache(repo.clone(), role_cache)
        .with_shared_codex_appserver(SharedCodexAppServer::new_fake_running_with_pending(
            repo.clone(),
            None,
        ));

    calm_server::recover_operations_on_boot(&state)
        .await
        .unwrap();

    let phase: String = sqlx::query_scalar("SELECT phase FROM operations WHERE id = ?1")
        .bind(&op_id)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(
        phase, "succeeded",
        "premise: the re-drive must run the operation to completion, THROUGH \
         `app_server_interact` — that is the writer this test exists for"
    );
    assert!(
        state.harness.get(&worker_session_id).is_some(),
        "premise: the re-drive must start the harness: op {op_id}"
    );

    // Give the run loop room to drain anything it thinks it owes before concluding.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        let handed_over =
            serde_json::to_string(&state.shared_codex_appserver.started_turns_for_test())
                .unwrap_or_default();
        assert!(
            !handed_over.contains(MOVED_AWAY),
            "the re-drive started the harness from the queue its output has been carrying since \
             `prepare_tx`, so a sentence another mint already took has been delivered again"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let persisted: Option<String> =
        sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id = ?1")
            .bind(&worker_session_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert!(
        !persisted.unwrap_or_default().contains(MOVED_AWAY),
        "and `handle.persist_snapshot()` must not have written the resurrected queue back onto \
         the row"
    );

    if let Some(handle) = state.harness.remove(&worker_session_id) {
        handle.shutdown().await.unwrap();
    }
}

/// The give-back returns a message only if its id is still on the failing runtime's queue; here another mint took
/// it onward, so nothing comes back. Staged at `Phase::Compensating` so boot recovery resumes the compensation.
#[tokio::test]
async fn the_give_back_returns_nothing_that_somebody_else_has_taken_onward() {
    const MOVED_ONWARD: &str = "a sentence that has since moved on";
    const MESSAGE_ID: &str = "instance-of-the-moved-sentence";

    let tmp = TempDir::new().unwrap();
    let db_url = sqlite_url(&tmp, "give-back-condition.db");
    let (source_id, failing_id) = {
        let repo = Arc::new(SqlxRepo::open(&db_url).await.unwrap());
        let area = repo
            .area_create(NewArea {
                name: "give-back".into(),
                color: "#111111".into(),
                sort: None,
            })
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                template_input: None,
                area_id: area.id,
                title: "give back".into(),
                sort: None,
                cwd: "/tmp".into(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: calm_server::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let card = seed_planner_card_row(&repo, &track.id).await;

        // `R`: harvested from, therefore emptied and stamped.
        let source_id = new_id();
        // The failing runtime: its queue no longer holds the sentence, because another mint inherited it away.
        let failing_id = new_id();
        let now = now_ms();

        let mut output = TxOutput::new(
            "card",
            Some(card.id.to_string()),
            serde_json::to_value(&card).unwrap(),
        );
        output.data = json!({
            "card_id": card.id.to_string(),
            "track_id": track.id.to_string(),
            "runtime_id": failing_id.clone(),
            "runtime_deferred": true,
            "cwd": track.workspace.path.clone(),
            "goal": null,
            "report_card_id": null,
            "snapshot": serde_json::to_value(HarnessSnapshot::initial(0, vec![])).unwrap(),
            "harvested_from": [{
                "worker_session_id": source_id.clone(),
                "messages": [{"text": MOVED_ONWARD, "ids": [MESSAGE_ID]}],
            }],
        });
        let payload = start_payload(track.id.as_ref(), &card.id, &track.workspace.path, None);
        let compensation_state = json!({
            "version": 1,
            "from_phase": "app_server_interact",
            "reason": "injected thread/start failure",
            "steps": [{
                "op": "fail_runtime",
                "args": {"runtime_id": failing_id.clone()},
                "completed": false,
                "attempts": 0,
                "last_error": null,
            }],
        });

        let mut tx = repo.pool().begin().await.unwrap();
        session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: source_id.clone(),
                card_id: card.id.to_string(),
                kind: WorkerSessionKind::SharedPlanner,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Starting,
                terminal_run_id: None,
                thread_id: None,
                session_id: None,
                active_turn_id: None,
                handle_state_json: Some(
                    serde_json::to_value(HarnessSnapshot::initial(0, vec![])).unwrap(),
                ),
                spawn_op_id: None,
                now_ms: now,
            },
        )
        .await
        .unwrap();
        session_mark_superseded_runtime_tx(&mut tx, &source_id)
            .await
            .unwrap();
        session_mark_queue_harvested_tx(&mut tx, &source_id, now)
            .await
            .unwrap();
        session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: failing_id.clone(),
                card_id: card.id.to_string(),
                kind: WorkerSessionKind::SharedPlanner,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Starting,
                terminal_run_id: None,
                thread_id: None,
                session_id: None,
                active_turn_id: None,
                // The sentence is NOT here: somebody inherited it away.
                handle_state_json: Some(
                    serde_json::to_value(HarnessSnapshot::initial(0, vec![])).unwrap(),
                ),
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
                   tx_output_json, compensation_state, phase, last_error,
                   created_at_ms, updated_at_ms
               )
               VALUES (?1, ?2, 'planner-harness-start', NULL, ?3,
                       'card', ?4, ?5, ?6, ?7, ?8, 'compensating',
                       'injected thread/start failure', ?9, ?9)"#,
        )
        .bind(new_id())
        .bind(new_id())
        .bind(new_id())
        .bind(card.id.as_str())
        .bind(serde_json::to_string(&json!({"type": "card", "id": card.id})).unwrap())
        .bind(serde_json::to_string(&payload).unwrap())
        .bind(serde_json::to_string(&output).unwrap())
        .bind(serde_json::to_string(&compensation_state).unwrap())
        .bind(now + 2)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();

        (source_id, failing_id)
    };

    let repo = Arc::new(SqlxRepo::open(&db_url).await.unwrap());
    let state = app_state_for_boot_test(repo.clone()).with_shared_codex_appserver(
        SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None),
    );
    calm_server::recover_operations_on_boot(&state)
        .await
        .unwrap();

    let source_state: Option<String> =
        sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id = ?1")
            .bind(&source_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert!(
        !source_state.unwrap_or_default().contains(MOVED_ONWARD),
        "the give-back put back a sentence its journal named but the failing runtime no longer \
         held, so the same sentence is now in two places"
    );
    let source_stamp: Option<i64> =
        sqlx::query_scalar("SELECT queue_harvested_at_ms FROM worker_sessions WHERE id = ?1")
            .bind(&source_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert!(
        source_stamp.is_some(),
        "and a row that got nothing back keeps its marker: its queue is somewhere else, \
         legitimately"
    );
    let failing_state: String =
        sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id = ?1")
            .bind(&failing_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(
        failing_state, "failed",
        "premise: the compensation must have run its fail_runtime step"
    );
}

/// `remaining.is_empty()` has two causes: every id went back, or the entry never had one. An entry with no ids was
/// enqueued before ids existed (migration 0095 does not stamp live rows) and must not be pruned as "returned".
#[tokio::test]
async fn the_give_back_keeps_a_pre_upgrade_sentence_it_cannot_identify() {
    const LEGACY: &str = "typed before the upgrade, no id to its name";
    const RETURNED: &str = "typed after, and going back";
    const RETURNED_ID: &str = "instance-of-the-returned-sentence";

    let tmp = TempDir::new().unwrap();
    let db_url = sqlite_url(&tmp, "give-back-legacy.db");
    let (source_id, failing_id) = {
        let repo = Arc::new(SqlxRepo::open(&db_url).await.unwrap());
        let area = repo
            .area_create(NewArea {
                name: "give-back-legacy".into(),
                color: "#111111".into(),
                sort: None,
            })
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                template_input: None,
                area_id: area.id,
                title: "give back legacy".into(),
                sort: None,
                cwd: "/tmp".into(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: calm_server::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let card = seed_planner_card_row(&repo, &track.id).await;

        let source_id = new_id();
        let failing_id = new_id();
        let now = now_ms();

        // The failing runtime holds both: an upgraded entry with no identity, and one this operation harvested and can name.
        let seeded = HarnessSnapshot::initial(
            0,
            vec![
                QueueEntry::user_message(LEGACY.into(), None, Vec::new()),
                QueueEntry::user_message_moved(
                    RETURNED.into(),
                    vec![RETURNED_ID.to_string()],
                    None,
                ),
            ],
        );
        // A user sentence with neither an addressable `QueueEntryId` nor a transfer identity; editing the JSON is the only way to reach that shape.
        let mut seeded_value = serde_json::to_value(&seeded).unwrap();
        seeded_value["pending_entry_meta"][0] = serde_json::Value::Null;
        seeded_value["pending_message_ids"][0] = json!([]);
        let failing_snapshot = HarnessSnapshot::from_value_strict(seeded_value);
        {
            let entries = failing_snapshot.pending_entries();
            assert_eq!(entries.len(), 2);
            assert!(
                entries[0].message_ids().is_empty() && entries[0].id().is_none(),
                "premise: the upgraded entry has no identity of either kind"
            );
            assert_eq!(
                entries[1].message_ids(),
                [RETURNED_ID.to_string()],
                "premise: the harvested entry can be named"
            );
        }

        let mut output = TxOutput::new(
            "card",
            Some(card.id.to_string()),
            serde_json::to_value(&card).unwrap(),
        );
        output.data = json!({
            "card_id": card.id.to_string(),
            "track_id": track.id.to_string(),
            "runtime_id": failing_id.clone(),
            "runtime_deferred": true,
            "cwd": track.workspace.path.clone(),
            "goal": null,
            "report_card_id": null,
            "snapshot": serde_json::to_value(&failing_snapshot).unwrap(),
            "harvested_from": [{
                "worker_session_id": source_id.clone(),
                "messages": [{"text": RETURNED, "ids": [RETURNED_ID]}],
            }],
        });
        let payload = start_payload(track.id.as_ref(), &card.id, &track.workspace.path, None);
        let compensation_state = json!({
            "version": 1,
            "from_phase": "app_server_interact",
            "reason": "injected thread/start failure",
            "steps": [{
                "op": "fail_runtime",
                "args": {"runtime_id": failing_id.clone()},
                "completed": false,
                "attempts": 0,
                "last_error": null,
            }],
        });

        let mut tx = repo.pool().begin().await.unwrap();
        session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: source_id.clone(),
                card_id: card.id.to_string(),
                kind: WorkerSessionKind::SharedPlanner,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Starting,
                terminal_run_id: None,
                thread_id: None,
                session_id: None,
                active_turn_id: None,
                // Emptied by the harvest, as a move leaves it.
                handle_state_json: Some(
                    serde_json::to_value(HarnessSnapshot::initial(0, vec![])).unwrap(),
                ),
                spawn_op_id: None,
                now_ms: now,
            },
        )
        .await
        .unwrap();
        session_mark_superseded_runtime_tx(&mut tx, &source_id)
            .await
            .unwrap();
        session_mark_queue_harvested_tx(&mut tx, &source_id, now)
            .await
            .unwrap();
        session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: failing_id.clone(),
                card_id: card.id.to_string(),
                kind: WorkerSessionKind::SharedPlanner,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Starting,
                terminal_run_id: None,
                thread_id: None,
                session_id: None,
                active_turn_id: None,
                handle_state_json: Some(serde_json::to_value(&failing_snapshot).unwrap()),
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
                   tx_output_json, compensation_state, phase, last_error,
                   created_at_ms, updated_at_ms
               )
               VALUES (?1, ?2, 'planner-harness-start', NULL, ?3,
                       'card', ?4, ?5, ?6, ?7, ?8, 'compensating',
                       'injected thread/start failure', ?9, ?9)"#,
        )
        .bind(new_id())
        .bind(new_id())
        .bind(new_id())
        .bind(card.id.as_str())
        .bind(serde_json::to_string(&json!({"type": "card", "id": card.id})).unwrap())
        .bind(serde_json::to_string(&payload).unwrap())
        .bind(serde_json::to_string(&output).unwrap())
        .bind(serde_json::to_string(&compensation_state).unwrap())
        .bind(now + 2)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();

        (source_id, failing_id)
    };

    let repo = Arc::new(SqlxRepo::open(&db_url).await.unwrap());
    let state = app_state_for_boot_test(repo.clone()).with_shared_codex_appserver(
        SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None),
    );
    calm_server::recover_operations_on_boot(&state)
        .await
        .unwrap();

    let source_state: String =
        sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id = ?1")
            .bind(&source_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert!(
        source_state.contains(RETURNED),
        "premise: the identifiable sentence must go back to the row it came from"
    );

    let failing_state: String =
        sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id = ?1")
            .bind(&failing_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert!(
        failing_state.contains(LEGACY),
        "a sentence with no id was never returned, and its source row is already empty — \
         pruning it here deletes it from both sides. It has to stay: {failing_state}"
    );
    assert!(
        !failing_state.contains(RETURNED),
        "premise: what WAS returned is pruned from the failing runtime"
    );
}
