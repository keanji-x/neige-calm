use std::sync::Arc;
use std::time::{Duration, Instant};

use calm_server::codex_appserver::Notification;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, runtime_start_tx};
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessSnapshot, HarnessState, Observation, SpecHarness,
    SpecHarnessParams,
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
        daemon,
        config,
        snapshot,
    });
    (repo, harness, runtime_id, thread_id)
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
    harness.observe(Observation::WaveGoal {
        text: "Read the wave goal.".into(),
    });
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
    harness.observe(Observation::TaskFailed {
        idempotency_key: "task-1".into(),
        error: "boom".into(),
    });
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
