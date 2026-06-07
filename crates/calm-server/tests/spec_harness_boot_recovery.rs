use std::sync::Arc;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, runtime_start_tx};
use calm_server::harness::{
    HarnessPhaseTag, HarnessRegistry, HarnessSnapshot, Observation, recover_harnesses_on_boot,
};
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
            cove_id: cove.id,
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
    let recovered = recover_harnesses_on_boot(repo, daemon, &registry)
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
