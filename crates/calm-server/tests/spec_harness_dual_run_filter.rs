use std::sync::Arc;
use std::time::{Duration, Instant};

use calm_server::codex_appserver::Notification;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, runtime_start_tx};
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessSnapshot, HarnessState, SpecHarness, SpecHarnessParams,
};
use calm_server::model::{NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::runtime_repo::{AgentProvider, RunStatus, RuntimeInit, RuntimeKind};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use serde_json::json;

#[tokio::test]
async fn harness_drops_foreign_thread_notifications() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "dual".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "dual".into(),
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
    let thread_b = "thread-harness-b".to_string();
    let runtime_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_b.clone());
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
            thread_id: Some(thread_b.clone()),
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
    let harness = SpecHarness::run(SpecHarnessParams {
        runtime_id,
        wave_id: card.wave_id,
        card_id: card.id,
        thread_id: Some(thread_b.clone()),
        repo,
        daemon: daemon.clone(),
        config: HarnessConfig::default(),
        snapshot,
    });

    daemon.emit_notification_for_test(Notification::TurnStarted {
        thread_id: "thread-legacy-a".into(),
        turn: json!({ "id": "foreign-turn" }),
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(matches!(harness.state_for_test().await, HarnessState::Idle));

    daemon.emit_notification_for_test(Notification::TurnStarted {
        thread_id: thread_b,
        turn: json!({ "id": "own-turn" }),
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if matches!(
            harness.state_for_test().await,
            HarnessState::TurnRunning { .. }
        ) {
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    harness.shutdown().await.unwrap();
}
