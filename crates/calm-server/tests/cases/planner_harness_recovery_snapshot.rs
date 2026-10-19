use super::*;
use calm_server::test_seams::{
    PLANNER_RECOVERY_BEFORE_WRITE, PLANNER_RECOVERY_CAPTURED, PausePoint, install_pause_for_test,
};

async fn stored(repo: &SqlxRepo, session: &str) -> HarnessSnapshot {
    HarnessSnapshot::from_value_strict(
        repo.session_projection_by_id(session)
            .await
            .unwrap()
            .unwrap()
            .handle_state_json
            .unwrap(),
    )
}

async fn event(repo: &SqlxRepo, track: &calm_server::model::Track, key: &str) -> i64 {
    repo.log_pure_event(
        ActorId::Kernel,
        EventScope::Track {
            track: track.id.clone(),
            area: track.area_id.clone(),
        },
        None,
        &EventBus::new(),
        &calm_server::card_role_cache::CardRoleCache::new(),
        &calm_server::track_area_cache::TrackAreaCache::new(),
        Event::TaskCompleted {
            idempotency_key: key.into(),
            result: json!({"message": key}),
            artifacts: vec![],
            agent_message: None,
        },
    )
    .await
    .unwrap()
}

async fn deferred_ack_race(replace_session: bool) {
    let tmp = TempDir::new().unwrap();
    let url = sqlite_url(&tmp, "recovery-ack.sqlite");
    let repo = Arc::new(SqlxRepo::open(&url).await.unwrap());
    let session = seed_recoverable_runtime(&repo, "recovery-ack", "thread-ack").await;
    let runtime = repo
        .session_projection_by_id(&session)
        .await
        .unwrap()
        .unwrap();
    let card = repo.card_get(&runtime.card_id).await.unwrap().unwrap();
    let track = repo
        .track_get(card.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let a = event(&repo, &track, "A").await;
    let daemon = SharedCodexAppServer::new_stub_with_pending(repo.clone(), None);
    let registry = HarnessRegistry::new();
    let pause = PausePoint::default();
    install_pause_for_test(PLANNER_RECOVERY_CAPTURED, &runtime.card_id, pause.clone());
    let driver = tokio::spawn(recover_harnesses_deferred(DeferredRecoveryParams {
        repo: repo.clone(),
        events: EventBus::new(),
        card_role_cache: calm_server::card_role_cache::CardRoleCache::new(),
        track_area_cache: calm_server::track_area_cache::TrackAreaCache::new(),
        daemon: daemon.clone(),
        seals: daemon.thread_seals().clone(),
        claude: calm_server::claude_planner::wiring::ClaudePlannerWiring::unconfigured_for_test(
            repo.clone(),
        ),
        registry: registry.clone(),
        track_delete_locks: calm_server::harness::new_track_delete_locks(),
        post_eligibility_hook: None,
    }));
    daemon.publish_readiness_for_test(1, true);
    tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
        .await
        .unwrap();

    let current_session = if replace_session {
        let successor = new_id();
        let mut tx = repo.pool().begin().await.unwrap();
        session_supersede_and_start_tx(
            &mut tx,
            &session,
            WorkerSessionInit {
                id: successor.clone(),
                card_id: runtime.card_id.clone(),
                kind: WorkerSessionKind::SharedPlanner,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Idle,
                terminal_run_id: None,
                thread_id: Some("thread-successor".into()),
                session_id: None,
                active_turn_id: None,
                handle_state_json: runtime.handle_state_json.clone(),
                spawn_op_id: None,
                now_ms: now_ms(),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        successor
    } else {
        session.clone()
    };
    // A legal live owner claims the current SQL session after recovery captured its candidate.
    let live = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: current_session.clone(),
        track_id: track.id.clone(),
        card_id: card.id.clone(),
        thread_id: runtime.thread_id.clone(),
        repo: repo.clone(),
        events: EventBus::new(),
        card_role_cache: calm_server::card_role_cache::CardRoleCache::new(),
        track_area_cache: calm_server::track_area_cache::TrackAreaCache::new(),
        backend: daemon.clone().into(),
        live_replies: registry.live_replies().clone(),
        config: HarnessConfig::default(),
        snapshot: stored(&repo, &current_session).await,
    });
    live.pause_issuance_for_dev();
    assert!(
        registry
            .try_reserve(current_session.clone())
            .unwrap()
            .install(live.clone())
    );
    let b = event(&repo, &track, "B").await;
    live.observe_envelope_for_test(
        Observation::TaskCompleted {
            idempotency_key: "B".into(),
            result_contract: None,
            result: json!({"message":"B"}),
        },
        b,
    )
    .await
    .unwrap();
    let acked = stored(&repo, &current_session).await;
    assert_eq!(acked.push_watermark, b);
    assert!(
        acked
            .pending_entries()
            .iter()
            .any(|e| e.envelope_id() == Some(b))
    );
    assert!(b > a);
    pause.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), driver)
        .await
        .unwrap()
        .unwrap();
    let after = stored(&repo, &current_session).await;
    // Check before any cleanup or subsequent live persistence can hide the lost update.
    assert_eq!(
        after.push_watermark, b,
        "recovery must not revoke an acknowledged watermark"
    );
    assert_eq!(after.pending_entries(), acked.pending_entries());
    assert_eq!(
        repo.session_projection_active_for_card(&runtime.card_id)
            .await
            .unwrap()
            .unwrap()
            .id,
        current_session
    );
    let reopened = SqlxRepo::open(&url).await.unwrap();
    assert_eq!(
        stored(&reopened, &current_session).await.pending_entries(),
        acked.pending_entries()
    );
    assert_eq!(stored(&reopened, &current_session).await.push_watermark, b);
    registry
        .shutdown_and_remove(&current_session)
        .await
        .unwrap();
    let reopened = Arc::new(reopened);
    let restarted_runtime = reopened
        .session_projection_by_id(&current_session)
        .await
        .unwrap()
        .unwrap();
    let restarted_daemon = SharedCodexAppServer::new_stub_with_pending(reopened.clone(), None);
    let restarted = spawn_recovered_harness(
        reopened.clone(),
        EventBus::new(),
        calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::track_area_cache::TrackAreaCache::new(),
        restarted_daemon.clone(),
        restarted_daemon.thread_seals().clone(),
        &calm_server::claude_planner::wiring::ClaudePlannerWiring::unconfigured_for_test(reopened),
        &registry,
        &calm_server::harness::new_track_delete_locks(),
        restarted_runtime,
        ClaimMode::Replace,
    )
    .await
    .unwrap()
    .installed()
    .expect("restart recovers the acknowledged queue");
    assert_eq!(
        restarted.snapshot().await.pending_entries(),
        acked.pending_entries()
    );
    assert_eq!(restarted.snapshot().await.push_watermark, b);
    registry
        .shutdown_and_remove(&current_session)
        .await
        .unwrap();
}

#[tokio::test]
async fn deferred_recovery_cannot_overwrite_live_durable_observation_ack() {
    deferred_ack_race(false).await;
}

#[tokio::test]
async fn deferred_recovery_cannot_write_old_snapshot_into_successor_session() {
    deferred_ack_race(true).await;
}

/// Another production reservation can supersede Reserved while recovery awaits storage.
/// Its live owner's ACK must make the captured complete snapshot fail the final comparison.
#[tokio::test]
async fn recovery_snapshot_conflict_skips_install_and_preserves_durable_observation() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let session = seed_recoverable_runtime(&repo, "recovery-cas", "thread-cas").await;
    let runtime = repo
        .session_projection_by_id(&session)
        .await
        .unwrap()
        .unwrap();
    let card = repo.card_get(&runtime.card_id).await.unwrap().unwrap();
    let track = repo
        .track_get(card.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    event(&repo, &track, "A").await;
    let daemon = SharedCodexAppServer::new_stub_with_pending(repo.clone(), None);
    let registry = HarnessRegistry::new();
    let pause = PausePoint::default();
    install_pause_for_test(
        PLANNER_RECOVERY_BEFORE_WRITE,
        &runtime.card_id,
        pause.clone(),
    );
    let recovering_repo = repo.clone();
    let recovering_daemon = daemon.clone();
    let recovering_registry = registry.clone();
    let stale_runtime = runtime.clone();
    let driver = tokio::spawn(async move {
        spawn_recovered_harness(
            recovering_repo.clone(),
            EventBus::new(),
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::track_area_cache::TrackAreaCache::new(),
            recovering_daemon.clone(),
            recovering_daemon.thread_seals().clone(),
            &calm_server::claude_planner::wiring::ClaudePlannerWiring::unconfigured_for_test(
                recovering_repo,
            ),
            &recovering_registry,
            &calm_server::harness::new_track_delete_locks(),
            stale_runtime,
            ClaimMode::Replace,
        )
        .await
        .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
        .await
        .unwrap();
    let live = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: session.clone(),
        track_id: track.id.clone(),
        card_id: card.id.clone(),
        thread_id: runtime.thread_id.clone(),
        repo: repo.clone(),
        events: EventBus::new(),
        card_role_cache: calm_server::card_role_cache::CardRoleCache::new(),
        track_area_cache: calm_server::track_area_cache::TrackAreaCache::new(),
        backend: daemon.clone().into(),
        live_replies: registry.live_replies().clone(),
        config: HarnessConfig::default(),
        snapshot: stored(&repo, &session).await,
    });
    live.pause_issuance_for_dev();
    assert!(
        registry
            .reserve_after_shutdown(session.clone())
            .await
            .unwrap()
            .install(live.clone())
    );
    let b = event(&repo, &track, "B").await;
    live.observe_envelope_for_test(
        Observation::TaskCompleted {
            idempotency_key: "B".into(),
            result_contract: None,
            result: json!({"message":"B"}),
        },
        b,
    )
    .await
    .unwrap();
    let acked = stored(&repo, &session).await;
    pause.release.notify_one();
    let outcome = tokio::time::timeout(Duration::from_secs(5), driver)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        outcome,
        calm_server::harness::RecoveryOutcome::Skipped
    ));
    assert_eq!(
        stored(&repo, &session).await.pending_entries(),
        acked.pending_entries()
    );
    assert_eq!(stored(&repo, &session).await.push_watermark, b);
    // Retrying the same ACK proves the winning live owner was not shutdown by the loser.
    live.observe_envelope_for_test(
        Observation::TrackGoal {
            text: "duplicate".into(),
        },
        b,
    )
    .await
    .unwrap();
    registry.shutdown_and_remove(&session).await.unwrap();

    // A replacement given the original projection MUST reload the whole checkpoint after shutdown.
    // B is already <= watermark: event replay alone cannot reconstruct its missing pending entry.
    let recovered = spawn_recovered_harness(
        repo.clone(),
        EventBus::new(),
        calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::track_area_cache::TrackAreaCache::new(),
        daemon.clone(),
        daemon.thread_seals().clone(),
        &calm_server::claude_planner::wiring::ClaudePlannerWiring::unconfigured_for_test(
            repo.clone(),
        ),
        &registry,
        &calm_server::harness::new_track_delete_locks(),
        runtime,
        ClaimMode::Replace,
    )
    .await
    .unwrap()
    .installed()
    .unwrap();
    assert_eq!(
        recovered.snapshot().await.pending_entries(),
        acked.pending_entries()
    );
    assert_eq!(recovered.snapshot().await.push_watermark, b);
    registry.shutdown_and_remove(&session).await.unwrap();
}

#[tokio::test]
async fn deferred_recovery_readiness_loss_before_write_leaves_checkpoint_untouched() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let session = seed_recoverable_runtime(&repo, "recovery-readiness", "thread-ready").await;
    let runtime = repo
        .session_projection_by_id(&session)
        .await
        .unwrap()
        .unwrap();
    let before = runtime.handle_state_json.clone();
    let card = repo.card_get(&runtime.card_id).await.unwrap().unwrap();
    let track = repo
        .track_get(card.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    event(&repo, &track, "A").await;
    let daemon = SharedCodexAppServer::new_stub_with_pending(repo.clone(), None);
    daemon.publish_readiness_for_test(1, true);
    let registry = HarnessRegistry::new();
    let pause = PausePoint::default();
    install_pause_for_test(
        PLANNER_RECOVERY_BEFORE_WRITE,
        &runtime.card_id,
        pause.clone(),
    );
    let recovering_repo = repo.clone();
    let recovering_daemon = daemon.clone();
    let recovering_registry = registry.clone();
    let driver = tokio::spawn(async move {
        spawn_recovered_harness(
            recovering_repo.clone(),
            EventBus::new(),
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::track_area_cache::TrackAreaCache::new(),
            recovering_daemon.clone(),
            recovering_daemon.thread_seals().clone(),
            &calm_server::claude_planner::wiring::ClaudePlannerWiring::unconfigured_for_test(
                recovering_repo,
            ),
            &recovering_registry,
            &calm_server::harness::new_track_delete_locks(),
            runtime,
            ClaimMode::SkipIfClaimed {
                expected_generation: 1,
            },
        )
        .await
        .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
        .await
        .unwrap();
    daemon.publish_readiness_for_test(1, false);
    pause.release.notify_one();
    let outcome = tokio::time::timeout(Duration::from_secs(5), driver)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        outcome,
        calm_server::harness::RecoveryOutcome::DaemonIneligible
    ));
    assert_eq!(
        repo.session_projection_by_id(&session)
            .await
            .unwrap()
            .unwrap()
            .handle_state_json,
        before
    );
    assert!(
        registry.try_reserve(session).is_some(),
        "abandoned reservation must release"
    );
}
