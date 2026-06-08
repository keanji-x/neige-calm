use std::sync::Arc;
use std::time::{Duration, Instant};

use calm_server::codex_appserver::Notification;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, runtime_start_tx};
use calm_server::event::EventBus;
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessSnapshot, HarnessState, IssuingKind, Observation,
    SpecHarness, SpecHarnessParams,
};
use calm_server::model::{CardRole, NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::runtime_repo::{AgentProvider, RunStatus, RuntimeInit, RuntimeKind};
use calm_server::shared_codex_appserver::{SharedCodexAppServer, SharedThreadStartParams};
use serde_json::json;

async fn fresh_repo() -> Arc<SqlxRepo> {
    Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap())
}

async fn seed_card(repo: &SqlxRepo) -> calm_server::model::Card {
    let cove = repo
        .cove_create(NewCove {
            name: "harness".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "goal".into(),
            sort: None,
            cwd: "/tmp".into(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    repo.card_create(NewCard {
        wave_id: wave.id,
        kind: "codex".into(),
        sort: None,
        payload: json!({"schemaVersion": 1}),
    })
    .await
    .unwrap()
}

async fn harness_with(
    repo: Arc<SqlxRepo>,
    daemon: Arc<SharedCodexAppServer>,
    phase: HarnessPhaseTag,
    config: HarnessConfig,
) -> (Arc<SqlxRepo>, SpecHarness, String, String) {
    let card = seed_card(&repo).await;
    let thread_id = if daemon.is_running() {
        daemon
            .thread_start_for_card(
                card.id.as_str(),
                CardRole::Spec,
                Some(card.wave_id.as_str()),
                SharedThreadStartParams {
                    cwd: "/tmp".into(),
                    approval_policy: "never".into(),
                    sandbox_mode: "workspace-write".into(),
                    developer_instructions: None,
                },
            )
            .await
            .unwrap()
    } else {
        "thread-offline".to_string()
    };
    let runtime_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = phase;
    snapshot.last_thread_id = Some(thread_id.clone());
    if matches!(
        phase,
        HarnessPhaseTag::TurnRunning | HarnessPhaseTag::TurnCompleted
    ) {
        snapshot.last_turn_id = Some("turn-prior".into());
    }
    let mut tx = repo.pool().begin().await.unwrap();
    runtime_start_tx(
        &mut tx,
        RuntimeInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: RuntimeKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: RunStatus::Idle,
            terminal_run_id: None,
            thread_id: Some(thread_id.clone()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
            lease_owner: None,
            lease_until_ms: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let harness = SpecHarness::run(SpecHarnessParams {
        runtime_id: runtime_id.clone(),
        wave_id: card.wave_id,
        card_id: card.id,
        thread_id: Some(thread_id.clone()),
        repo: repo.clone(),
        events: EventBus::new(),
        card_role_cache: calm_server::card_role_cache::CardRoleCache::new(),
        wave_cove_cache: calm_server::wave_cove_cache::WaveCoveCache::new(),
        daemon,
        config,
        snapshot,
    });
    (repo, harness, runtime_id, thread_id)
}

async fn harness_from_snapshot(
    repo: Arc<SqlxRepo>,
    daemon: Arc<SharedCodexAppServer>,
    snapshot: HarnessSnapshot,
) -> (SpecHarness, String) {
    let card = seed_card(&repo).await;
    let runtime_id = new_id();
    let thread_id = snapshot
        .last_thread_id
        .clone()
        .unwrap_or_else(|| "thread-offline".to_string());
    let mut tx = repo.pool().begin().await.unwrap();
    runtime_start_tx(
        &mut tx,
        RuntimeInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: RuntimeKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: RunStatus::Idle,
            terminal_run_id: None,
            thread_id: Some(thread_id.clone()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
            lease_owner: None,
            lease_until_ms: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let harness = SpecHarness::run(SpecHarnessParams {
        runtime_id: runtime_id.clone(),
        wave_id: card.wave_id,
        card_id: card.id,
        thread_id: Some(thread_id),
        repo,
        events: EventBus::new(),
        card_role_cache: calm_server::card_role_cache::CardRoleCache::new(),
        wave_cove_cache: calm_server::wave_cove_cache::WaveCoveCache::new(),
        daemon,
        config: HarnessConfig::default(),
        snapshot,
    });
    (harness, runtime_id)
}

async fn wait_for_state(
    harness: &SpecHarness,
    pred: impl Fn(&HarnessState) -> bool,
) -> HarnessState {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let state = harness.state_for_test().await;
        if pred(&state) {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting; last={state:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn issuing_turn_snapshot_with_turn_id_restores_resumed() {
    let repo = fresh_repo().await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::IssuingTurn;
    snapshot.last_thread_id = Some("thread-prior".into());
    snapshot.last_turn_id = Some("T1".into());
    let (harness, _runtime_id) = harness_from_snapshot(repo, daemon, snapshot).await;

    assert!(matches!(
        harness.state_for_test().await,
        HarnessState::Resumed { .. }
    ));
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn issuing_turn_snapshot_without_turn_id_restores_retryable_completed() {
    let repo = fresh_repo().await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::IssuingTurn;
    snapshot.last_thread_id = Some("thread-prior".into());
    let (harness, _runtime_id) = harness_from_snapshot(repo, daemon, snapshot).await;

    assert!(matches!(
        harness.state_for_test().await,
        HarnessState::TurnCompleted { last_turn_id } if last_turn_id.is_empty()
    ));
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn pending_thread_start_to_idle_on_thread_started() {
    let repo = fresh_repo().await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let (_repo, harness, _runtime_id, thread_id) = harness_with(
        repo,
        daemon.clone(),
        HarnessPhaseTag::PendingThreadStart,
        HarnessConfig::default(),
    )
    .await;
    daemon.emit_notification_for_test(Notification::ThreadStarted {
        params: json!({ "thread": { "id": thread_id } }),
    });
    wait_for_state(&harness, |s| matches!(s, HarnessState::Idle)).await;
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn idle_to_turn_running_to_completed() {
    let repo = fresh_repo().await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let (_repo, harness, _runtime_id, thread_id) = harness_with(
        repo,
        daemon.clone(),
        HarnessPhaseTag::Idle,
        HarnessConfig::default(),
    )
    .await;
    harness
        .observe(Observation::WaveGoal {
            text: "Read the wave goal.".into(),
        })
        .unwrap();
    let running = wait_for_state(&harness, |s| matches!(s, HarnessState::TurnRunning { .. })).await;
    let turn_id = match running {
        HarnessState::TurnRunning { turn_id, .. } => turn_id,
        other => panic!("expected running, got {other:?}"),
    };
    daemon.emit_notification_for_test(Notification::TurnCompleted {
        thread_id,
        turn: json!({ "id": turn_id, "status": "completed" }),
    });
    wait_for_state(&harness, |s| {
        matches!(s, HarnessState::TurnCompleted { .. })
    })
    .await;
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn stale_turn_completed_ignored_during_active_turn() {
    let repo = fresh_repo().await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let (_repo, harness, _runtime_id, thread_id) = harness_with(
        repo,
        daemon.clone(),
        HarnessPhaseTag::Idle,
        HarnessConfig::default(),
    )
    .await;
    harness
        .observe(Observation::WaveGoal {
            text: "Read the wave goal.".into(),
        })
        .unwrap();
    let running = wait_for_state(&harness, |s| matches!(s, HarnessState::TurnRunning { .. })).await;
    let turn1 = match running {
        HarnessState::TurnRunning { turn_id, .. } => turn_id,
        other => panic!("expected running, got {other:?}"),
    };
    daemon.emit_notification_for_test(Notification::TurnCompleted {
        thread_id: thread_id.clone(),
        turn: json!({ "id": turn1, "status": "completed" }),
    });
    wait_for_state(
        &harness,
        |s| matches!(s, HarnessState::TurnCompleted { last_turn_id } if last_turn_id == &turn1),
    )
    .await;

    harness
        .observe(Observation::WaveGoal {
            text: "Read the next wave goal.".into(),
        })
        .unwrap();
    let running = wait_for_state(
        &harness,
        |s| matches!(s, HarnessState::TurnRunning { turn_id, .. } if turn_id != &turn1),
    )
    .await;
    let turn2 = match running {
        HarnessState::TurnRunning { turn_id, .. } => turn_id,
        other => panic!("expected second running turn, got {other:?}"),
    };

    daemon.emit_notification_for_test(Notification::TurnCompleted {
        thread_id,
        turn: json!({ "id": turn1, "status": "completed" }),
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(matches!(
        harness.state_for_test().await,
        HarnessState::TurnRunning { turn_id, .. } if turn_id == turn2
    ));
    assert_eq!(
        harness.snapshot().await.last_turn_id.as_deref(),
        Some(turn2.as_str())
    );
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn stale_turn_started_ignored_when_other_turn_active() {
    let repo = fresh_repo().await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let (_repo, harness, _runtime_id, thread_id) = harness_with(
        repo,
        daemon.clone(),
        HarnessPhaseTag::Idle,
        HarnessConfig::default(),
    )
    .await;
    harness
        .observe(Observation::WaveGoal {
            text: "Read the wave goal.".into(),
        })
        .unwrap();
    let running = wait_for_state(&harness, |s| matches!(s, HarnessState::TurnRunning { .. })).await;
    let turn1 = match running {
        HarnessState::TurnRunning { turn_id, .. } => turn_id,
        other => panic!("expected running, got {other:?}"),
    };

    daemon.emit_notification_for_test(Notification::TurnStarted {
        thread_id,
        turn: json!({ "id": "ghost-turn" }),
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(matches!(
        harness.state_for_test().await,
        HarnessState::TurnRunning { turn_id, .. } if turn_id == turn1
    ));
    assert_eq!(
        harness.snapshot().await.last_turn_id.as_deref(),
        Some(turn1.as_str())
    );
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn replayed_turn_started_after_completed_ignored() {
    let repo = fresh_repo().await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let (_repo, harness, _runtime_id, thread_id) = harness_with(
        repo,
        daemon.clone(),
        HarnessPhaseTag::Idle,
        HarnessConfig::default(),
    )
    .await;
    harness
        .observe(Observation::WaveGoal {
            text: "Read the wave goal.".into(),
        })
        .unwrap();
    let running = wait_for_state(&harness, |s| matches!(s, HarnessState::TurnRunning { .. })).await;
    let turn1 = match running {
        HarnessState::TurnRunning { turn_id, .. } => turn_id,
        other => panic!("expected running, got {other:?}"),
    };

    daemon.emit_notification_for_test(Notification::TurnCompleted {
        thread_id: thread_id.clone(),
        turn: json!({ "id": turn1, "status": "completed" }),
    });
    wait_for_state(
        &harness,
        |s| matches!(s, HarnessState::TurnCompleted { last_turn_id } if last_turn_id == &turn1),
    )
    .await;

    daemon.emit_notification_for_test(Notification::TurnStarted {
        thread_id,
        turn: json!({ "id": turn1 }),
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(matches!(
        harness.state_for_test().await,
        HarnessState::TurnCompleted { last_turn_id } if last_turn_id == turn1
    ));
    assert_eq!(
        harness.snapshot().await.last_turn_id.as_deref(),
        Some(turn1.as_str())
    );
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn stale_turn_started_in_turn_completed_ignored() {
    let repo = fresh_repo().await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let (_repo, harness, _runtime_id, thread_id) = harness_with(
        repo,
        daemon.clone(),
        HarnessPhaseTag::Idle,
        HarnessConfig::default(),
    )
    .await;
    harness
        .observe(Observation::WaveGoal {
            text: "Read the wave goal.".into(),
        })
        .unwrap();
    let running = wait_for_state(&harness, |s| matches!(s, HarnessState::TurnRunning { .. })).await;
    let turn1 = match running {
        HarnessState::TurnRunning { turn_id, .. } => turn_id,
        other => panic!("expected running, got {other:?}"),
    };

    daemon.emit_notification_for_test(Notification::TurnCompleted {
        thread_id: thread_id.clone(),
        turn: json!({ "id": turn1, "status": "completed" }),
    });
    wait_for_state(
        &harness,
        |s| matches!(s, HarnessState::TurnCompleted { last_turn_id } if last_turn_id == &turn1),
    )
    .await;

    daemon.emit_notification_for_test(Notification::TurnStarted {
        thread_id,
        turn: json!({ "id": "foreign-turn" }),
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(matches!(
        harness.state_for_test().await,
        HarnessState::TurnCompleted { last_turn_id } if last_turn_id == turn1
    ));
    assert_eq!(
        harness.snapshot().await.last_turn_id.as_deref(),
        Some(turn1.as_str())
    );
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn stale_turn_started_in_issuing_with_foreign_id_ignored() {
    let repo = fresh_repo().await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let (_repo, harness, _runtime_id, thread_id) = harness_with(
        repo,
        daemon.clone(),
        HarnessPhaseTag::Idle,
        HarnessConfig::default(),
    )
    .await;
    harness
        .set_state_for_test(HarnessState::Issuing {
            since: Instant::now(),
            kind: IssuingKind::TurnStart,
        })
        .await;
    harness
        .set_issued_turn_id_for_test(Some("turn-A".into()))
        .await;

    daemon.emit_notification_for_test(Notification::TurnStarted {
        thread_id,
        turn: json!({ "id": "foreign-turn" }),
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(matches!(
        harness.state_for_test().await,
        HarnessState::Issuing {
            kind: IssuingKind::TurnStart,
            ..
        }
    ));
    assert!(harness.snapshot().await.last_turn_id.is_none());
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn recovered_resumed_accepts_matching_turn_started() {
    let repo = fresh_repo().await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Resumed;
    snapshot.last_thread_id = Some("thread-resumed".into());
    snapshot.last_turn_id = Some("turn-prior".into());
    let (harness, _runtime_id) = harness_from_snapshot(repo, daemon.clone(), snapshot).await;
    harness
        .set_state_for_test(HarnessState::Resumed {
            resumed_at: Instant::now(),
        })
        .await;

    daemon.emit_notification_for_test(Notification::TurnStarted {
        thread_id: "thread-resumed".into(),
        turn: json!({ "id": "turn-prior" }),
    });
    wait_for_state(
        &harness,
        |s| matches!(s, HarnessState::TurnRunning { turn_id, .. } if turn_id == "turn-prior"),
    )
    .await;

    daemon.emit_notification_for_test(Notification::TurnStarted {
        thread_id: "thread-resumed".into(),
        turn: json!({ "id": "other" }),
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(matches!(
        harness.state_for_test().await,
        HarnessState::TurnRunning { turn_id, .. } if turn_id == "turn-prior"
    ));
    assert_eq!(
        harness.snapshot().await.last_turn_id.as_deref(),
        Some("turn-prior")
    );
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn turn_started_without_id_ignored() {
    let repo = fresh_repo().await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let (_repo, harness, _runtime_id, thread_id) = harness_with(
        repo,
        daemon.clone(),
        HarnessPhaseTag::Idle,
        HarnessConfig::default(),
    )
    .await;
    harness
        .observe(Observation::WaveGoal {
            text: "Read the wave goal.".into(),
        })
        .unwrap();
    let running = wait_for_state(&harness, |s| matches!(s, HarnessState::TurnRunning { .. })).await;
    let turn1 = match running {
        HarnessState::TurnRunning { turn_id, .. } => turn_id,
        other => panic!("expected running, got {other:?}"),
    };

    daemon.emit_notification_for_test(Notification::TurnStarted {
        thread_id,
        turn: json!({}),
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(matches!(
        harness.state_for_test().await,
        HarnessState::TurnRunning { turn_id, .. } if turn_id == turn1
    ));
    assert_eq!(
        harness.snapshot().await.last_turn_id.as_deref(),
        Some(turn1.as_str())
    );
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn running_to_wedged_on_system_error() {
    let repo = fresh_repo().await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let (_repo, harness, _runtime_id, thread_id) = harness_with(
        repo,
        daemon.clone(),
        HarnessPhaseTag::TurnRunning,
        HarnessConfig::default(),
    )
    .await;
    daemon.emit_notification_for_test(Notification::ThreadStatusChanged {
        thread_id,
        status: json!({ "type": "systemError" }),
    });
    let state = wait_for_state(
        &harness,
        |s| matches!(s, HarnessState::Wedged { reason, .. } if reason == "system_error"),
    )
    .await;
    assert!(matches!(state, HarnessState::Wedged { .. }));
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn resumed_blocks_turn_start_until_idle_status() {
    let repo = fresh_repo().await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let config = HarnessConfig {
        debounce_min_idle: Duration::from_millis(10),
        debounce_max_wait: Duration::from_millis(20),
        resumed_reconcile_budget: Duration::from_secs(60),
        ..HarnessConfig::default()
    };
    let (_repo, harness, _runtime_id, thread_id) =
        harness_with(repo, daemon.clone(), HarnessPhaseTag::Resumed, config).await;
    harness
        .observe(Observation::WaveGoal {
            text: "wait for reconcile".into(),
        })
        .unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(daemon.turn_start_count_for_test(), 0);
    assert!(matches!(
        harness.state_for_test().await,
        HarnessState::Resumed { .. }
    ));

    daemon.emit_notification_for_test(Notification::ThreadStatusChanged {
        thread_id,
        status: json!({ "type": "idle" }),
    });
    wait_for_state(&harness, |s| matches!(s, HarnessState::TurnRunning { .. })).await;
    assert_eq!(daemon.turn_start_count_for_test(), 1);
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn resumed_reconcile_budget_allows_turn_start_after_timeout() {
    let repo = fresh_repo().await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let config = HarnessConfig {
        debounce_min_idle: Duration::from_millis(10),
        debounce_max_wait: Duration::from_millis(20),
        resumed_reconcile_budget: Duration::from_millis(50),
        ..HarnessConfig::default()
    };
    let (_repo, harness, _runtime_id, _thread_id) =
        harness_with(repo, daemon.clone(), HarnessPhaseTag::Resumed, config).await;
    harness
        .observe(Observation::WaveGoal {
            text: "wait for timeout".into(),
        })
        .unwrap();
    wait_for_state(&harness, |s| matches!(s, HarnessState::TurnRunning { .. })).await;
    assert_eq!(daemon.turn_start_count_for_test(), 1);
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn watchdog_interrupt_timeout_wedges() {
    let repo = fresh_repo().await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let config = HarnessConfig {
        max_turn_duration: Duration::from_millis(10),
        interrupt_completion_budget: Duration::from_millis(10),
        ..HarnessConfig::default()
    };
    let (_repo, harness, _runtime_id, _thread_id) =
        harness_with(repo, daemon, HarnessPhaseTag::TurnRunning, config).await;
    harness
        .set_state_for_test(HarnessState::TurnRunning {
            turn_id: "turn-timeout".into(),
            started_at: Instant::now() - Duration::from_secs(1),
        })
        .await;
    wait_for_state(
        &harness,
        |s| matches!(s, HarnessState::Wedged { reason, .. } if reason == "interrupt_timeout"),
    )
    .await;
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn turn_start_error_rolls_back_and_rebuffers() {
    let repo = fresh_repo().await;
    let daemon = SharedCodexAppServer::new_stub(repo.clone());
    let (_repo, harness, _runtime_id, _thread_id) = harness_with(
        repo,
        daemon,
        HarnessPhaseTag::Idle,
        HarnessConfig::default(),
    )
    .await;
    harness
        .observe(Observation::TaskFailed {
            idempotency_key: "task-1".into(),
            error: "boom".into(),
        })
        .unwrap();
    wait_for_state(&harness, |s| {
        matches!(s, HarnessState::TurnCompleted { .. })
    })
    .await;
    assert_eq!(harness.pending_len_for_test().await, 1);
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn restored_wave_goal_issues_first_turn_without_new_observation() {
    let repo = fresh_repo().await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let card = seed_card(&repo).await;
    let thread_id = daemon
        .thread_start_for_card(
            card.id.as_str(),
            CardRole::Spec,
            Some(card.wave_id.as_str()),
            SharedThreadStartParams {
                cwd: "/tmp".into(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
            },
        )
        .await
        .unwrap();
    let runtime_id = new_id();
    let mut snapshot =
        HarnessSnapshot::initial(0, vec![Observation::WaveGoal { text: "go".into() }]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_id.clone());
    let mut tx = repo.pool().begin().await.unwrap();
    runtime_start_tx(
        &mut tx,
        RuntimeInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: RuntimeKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: RunStatus::Idle,
            terminal_run_id: None,
            thread_id: Some(thread_id.clone()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
            lease_owner: None,
            lease_until_ms: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let config = HarnessConfig {
        debounce_min_idle: Duration::from_millis(25),
        debounce_max_wait: Duration::from_secs(1),
        ..HarnessConfig::default()
    };
    let harness = SpecHarness::run(SpecHarnessParams {
        runtime_id,
        wave_id: card.wave_id,
        card_id: card.id,
        thread_id: Some(thread_id),
        repo,
        events: EventBus::new(),
        card_role_cache: calm_server::card_role_cache::CardRoleCache::new(),
        wave_cove_cache: calm_server::wave_cove_cache::WaveCoveCache::new(),
        daemon: daemon.clone(),
        config,
        snapshot,
    });

    tokio::time::timeout(
        config.debounce_min_idle + Duration::from_millis(300),
        async {
            loop {
                if daemon.turn_start_count_for_test() > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        },
    )
    .await
    .expect("restored pending WaveGoal should issue a first turn");
    wait_for_state(&harness, |s| matches!(s, HarnessState::TurnRunning { .. })).await;
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn interrupt_target_completed_status_clears_watchdog() {
    let repo = fresh_repo().await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let (_repo, harness, _runtime_id, thread_id) = harness_with(
        repo,
        daemon.clone(),
        HarnessPhaseTag::Idle,
        HarnessConfig::default(),
    )
    .await;
    harness
        .set_state_for_test(HarnessState::TurnRunning {
            turn_id: "turn-race".into(),
            started_at: Instant::now(),
        })
        .await;
    harness.interrupt("manual".into()).await.unwrap();
    assert!(matches!(
        harness.state_for_test().await,
        HarnessState::Issuing {
            kind: calm_server::harness::IssuingKind::Interrupt { .. },
            ..
        }
    ));

    daemon.emit_notification_for_test(Notification::TurnCompleted {
        thread_id,
        turn: json!({ "id": "turn-race", "status": "completed" }),
    });
    let state = wait_for_state(&harness, |s| {
        matches!(
            s,
            HarnessState::TurnCompleted { last_turn_id } if last_turn_id == "turn-race"
        )
    })
    .await;
    assert!(matches!(state, HarnessState::TurnCompleted { .. }));
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn interrupt_target_aborted_notification_clears_watchdog() {
    let repo = fresh_repo().await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let (_repo, harness, _runtime_id, thread_id) = harness_with(
        repo,
        daemon.clone(),
        HarnessPhaseTag::Idle,
        HarnessConfig::default(),
    )
    .await;
    harness
        .set_state_for_test(HarnessState::TurnRunning {
            turn_id: "turn-aborted".into(),
            started_at: Instant::now(),
        })
        .await;
    harness.interrupt("manual".into()).await.unwrap();
    assert!(matches!(
        harness.state_for_test().await,
        HarnessState::Issuing {
            kind: calm_server::harness::IssuingKind::Interrupt { .. },
            ..
        }
    ));

    daemon.emit_notification_for_test(Notification::Other {
        method: "turn/aborted".into(),
        params: json!({
            "threadId": thread_id,
            "turnId": "turn-aborted",
        }),
    });
    let state = wait_for_state(&harness, |s| {
        matches!(
            s,
            HarnessState::TurnCompleted { last_turn_id } if last_turn_id == "turn-aborted"
        )
    })
    .await;
    assert!(matches!(state, HarnessState::TurnCompleted { .. }));
    harness.shutdown().await.unwrap();
}
