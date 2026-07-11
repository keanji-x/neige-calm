use std::sync::Arc;
use std::time::{Duration, Instant};

use calm_server::codex_appserver::Notification;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::event::EventBus;
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessSnapshot, SpecHarness, SpecHarnessParams,
};
use calm_server::model::{NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use serde_json::{Value, json};

async fn seed_harness(
    repo: Arc<SqlxRepo>,
    events: EventBus,
) -> (SpecHarness, Arc<SharedCodexAppServer>, String, String) {
    let cove = repo
        .cove_create(NewCove {
            name: "items-persist".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            workflow_input: None,
            cove_id: cove.id.clone(),
            title: "items persist".into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
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
            payload: json!({"schemaVersion": 1, "spec_harness": true}),
        })
        .await
        .unwrap();
    let runtime_id = new_id();
    let thread_id = "thread-items-persist".to_string();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_id.clone());

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
            thread_id: Some(thread_id.clone()),
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
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    wave_cove_cache.insert(wave.id.clone(), cove.id);
    let harness = SpecHarness::run(SpecHarnessParams {
        runtime_id,
        wave_id: card.wave_id.clone(),
        card_id: card.id.clone(),
        thread_id: Some(thread_id),
        repo: repo_dyn,
        events,
        card_role_cache: calm_server::card_role_cache::CardRoleCache::new(),
        wave_cove_cache,
        daemon: daemon.clone(),
        config: HarnessConfig {
            debounce_min_idle: Duration::from_secs(60),
            debounce_max_wait: Duration::from_secs(60),
            ..HarnessConfig::default()
        },
        snapshot,
    });

    (
        harness,
        daemon,
        card.id.to_string(),
        card.wave_id.to_string(),
    )
}

async fn wait_for_rows(
    repo: &SqlxRepo,
    card_id: &str,
    count: usize,
) -> Vec<calm_server::model::HarnessItem> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let rows = repo
            .harness_item_list_by_card(card_id, 0, 100, false)
            .await
            .unwrap();
        if rows.len() == count {
            return rows;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {count} harness item rows; got {}",
            rows.len()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_notification_receiver(daemon: &SharedCodexAppServer) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if daemon.notification_receiver_count_for_test() > 0 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for harness notification receiver"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn mcp_tool_call_notifications_persist_with_camelcase_status_round_trip() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let (harness, daemon, card_id, _wave_id) = seed_harness(repo.clone(), events).await;
    wait_for_notification_receiver(&daemon).await;

    daemon.emit_notification_for_test(Notification::Item {
        method: "item/started".into(),
        params: json!({
            "threadId": "thread-items-persist",
            "turn": { "id": "turn-mcp-1" },
            "item": {
                "id": "mcp-1",
                "type": "mcpToolCall",
                "server": "neige",
                "tool": "calm.wave.cat",
                "status": "inProgress",
                "arguments": { "path": "report.md" }
            }
        }),
    });

    let rows = wait_for_rows(&repo, &card_id, 1).await;
    let row = &rows[0];
    assert_eq!(row.item_type.as_deref(), Some("mcpToolCall"));
    assert_eq!(row.method, "item/started");
    let params: Value = serde_json::from_str(&row.params).unwrap();
    assert_eq!(params["item"]["status"], "inProgress");

    daemon.emit_notification_for_test(Notification::Item {
        method: "item/completed".into(),
        params: json!({
            "threadId": "thread-items-persist",
            "turn": { "id": "turn-mcp-1" },
            "item": {
                "id": "mcp-1",
                "type": "mcpToolCall",
                "server": "neige",
                "tool": "calm.wave.cat",
                "status": "completed",
                "result": { "content": [{ "type": "text", "text": "ok" }] },
                "durationMs": 42
            }
        }),
    });

    let rows = wait_for_rows(&repo, &card_id, 2).await;
    assert!(rows[0].id < rows[1].id);
    let row = &rows[1];
    assert_eq!(row.item_type.as_deref(), Some("mcpToolCall"));
    assert_eq!(row.method, "item/completed");
    let params: Value = serde_json::from_str(&row.params).unwrap();
    assert_eq!(params["item"]["status"], "completed");
    assert_eq!(params["item"]["result"]["content"][0]["text"], "ok");

    harness.shutdown().await.unwrap();
}
