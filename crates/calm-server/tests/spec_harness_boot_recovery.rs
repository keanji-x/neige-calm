use std::sync::Arc;
use std::time::Duration;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, runtime_start_tx};
use calm_server::error::CalmError;
use calm_server::event::{EditAuthor, Event, EventBus, EventScope};
use calm_server::harness::{
    HarnessPhaseTag, HarnessRegistry, HarnessSnapshot, Observation, recover_harnesses_on_boot,
};
use calm_server::ids::ActorId;
use calm_server::model::{NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::runtime_repo::{AgentProvider, RunStatus, RuntimeInit, RuntimeKind};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use serde_json::json;

fn app_state_for_boot_test(repo: Arc<SqlxRepo>) -> AppState {
    let events = EventBus::new();
    let role_cache = calm_server::card_role_cache::CardRoleCache::new();
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
            cove_id: cove.id.clone(),
            title: "boot".into(),
            sort: None,
            cwd: "/tmp".into(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id,
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
    runtime_start_tx(
        &mut tx,
        RuntimeInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: RuntimeKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: RunStatus::Idle,
            terminal_run_id: None,
            thread_id: Some("thread-recovered".into()),
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

#[tokio::test]
async fn recover_harnesses_on_boot_skipped_when_daemon_unavailable() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "boot-unavailable".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "boot-unavailable".into(),
            sort: None,
            cwd: "/tmp".into(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .unwrap();
    let runtime_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(
        11,
        vec![Observation::WaveGoal {
            text: "wait for daemon".into(),
        }],
    );
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some("thread-unavailable".into());
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
            thread_id: Some("thread-unavailable".into()),
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

    let state = app_state_for_boot_test(repo.clone());
    let recovered = calm_server::recover_harnesses_after_daemon_boot(
        &state,
        Err(CalmError::CodexAppServer("daemon unavailable".into())),
    )
    .await
    .unwrap();
    assert_eq!(recovered, 0);
    assert!(state.harness.get(&runtime_id).is_none());
    let runtime = repo.runtime_get_by_id(&runtime_id).await.unwrap().unwrap();
    let stored: HarnessSnapshot =
        serde_json::from_value(runtime.handle_state_json.unwrap()).unwrap();
    assert_eq!(stored.pending_queue.len(), 1);
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
            cove_id: cove.id.clone(),
            title: "boot-deferred".into(),
            sort: None,
            cwd: "/tmp".into(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id,
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
    runtime_start_tx(
        &mut tx,
        RuntimeInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: RuntimeKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: RunStatus::Idle,
            terminal_run_id: None,
            thread_id: Some("thread-deferred".into()),
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
            cove_id: cove.id.clone(),
            title: "boot-replay".into(),
            sort: None,
            cwd: "/tmp".into(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .unwrap();
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
    runtime_start_tx(
        &mut tx,
        RuntimeInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: RuntimeKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: RunStatus::Idle,
            terminal_run_id: None,
            thread_id: Some("thread-recovered".into()),
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
    let runtime = repo.runtime_get_by_id(&runtime_id).await.unwrap().unwrap();
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
            cove_id: cove.id,
            title: "boot-terminal".into(),
            sort: None,
            cwd: "/tmp".into(),
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
    runtime_start_tx(
        &mut tx,
        RuntimeInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: RuntimeKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: RunStatus::Idle,
            terminal_run_id: None,
            thread_id: Some("thread-terminal".into()),
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
            cove_id: cove.id.clone(),
            title: "gate-replay".into(),
            sort: None,
            cwd: "/tmp".into(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .unwrap();

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
            calm_server::db::sqlite::task_insert_tx(tx, &gated).await?;
            calm_server::db::sqlite::task_insert_tx(tx, &ungated).await?;
            calm_server::db::sqlite::task_insert_tx(tx, &gated_failed).await?;
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
    runtime_start_tx(
        &mut tx,
        RuntimeInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: RuntimeKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: RunStatus::Idle,
            terminal_run_id: None,
            thread_id: Some("thread-recovered".into()),
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
    let runtime = repo.runtime_get_by_id(&runtime_id).await.unwrap().unwrap();
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
