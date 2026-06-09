use std::sync::Arc;
use std::time::Duration;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, runtime_start_tx};
use calm_server::event::{EditAuthor, Event, EventBus, EventScope};
use calm_server::harness::{
    HarnessPhaseTag, HarnessRegistry, HarnessSnapshot, Observation, recover_harnesses_on_boot,
};
use calm_server::ids::ActorId;
use calm_server::model::{NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::runtime_repo::{AgentProvider, RunStatus, RuntimeInit, RuntimeKind};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use serde_json::json;

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
