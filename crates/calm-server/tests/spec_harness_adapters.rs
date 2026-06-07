use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::harness::HarnessState;
use calm_server::model::{NewCove, NewWave, new_id};
use calm_server::operation::spec_harness_interrupt_adapter::SpecHarnessInterruptOperationPayload;
use calm_server::operation::spec_harness_shutdown_adapter::SpecHarnessShutdownOperationPayload;
use calm_server::operation::spec_harness_start_adapter::SpecHarnessStartOperationPayload;
use calm_server::operation::{OperationKey, OperationOutcome};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::runtime_repo::RunStatus;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use calm_server::wave_cove_cache::WaveCoveCache;

async fn state_with_fake_daemon() -> (AppState, Arc<SqlxRepo>) {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            EventBus::new(),
            WriteContext::new(CardRoleCache::new(), WaveCoveCache::new()),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    );
    let shared = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    (state.with_shared_codex_appserver(shared), repo)
}

async fn seed_wave(repo: &SqlxRepo) -> calm_server::model::Wave {
    let cove = repo
        .cove_create(NewCove {
            name: "adapter".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    repo.wave_create(NewWave {
        cove_id: cove.id,
        title: "adapter goal".into(),
        sort: None,
        cwd: "/tmp".into(),
        attach_folder: false,
        theme: calm_server::routes::theme::RequestTheme::default_dark(),
    })
    .await
    .unwrap()
}

fn key() -> OperationKey {
    OperationKey {
        operation_key: new_id(),
        idempotency_key: None,
        payload_hash: new_id(),
    }
}

async fn wait_op(state: &AppState, op_id: &String) -> OperationOutcome {
    state.operation_runtime.wait(op_id).await.unwrap().outcome
}

#[tokio::test]
async fn start_interrupt_and_shutdown_adapters_drive_harness_lifecycle() {
    let (state, repo) = state_with_fake_daemon().await;
    let wave = seed_wave(&repo).await;
    let card_id = new_id();
    let payload = serde_json::to_value(SpecHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        wave_id: wave.id.to_string(),
        card_id: Some(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: wave.cwd.clone(),
        goal: Some("adapter goal".into()),
    })
    .unwrap();
    let op_id = state
        .operation_runtime
        .submit("spec-harness-start", key(), payload)
        .await
        .unwrap();
    assert!(matches!(
        wait_op(&state, &op_id).await,
        OperationOutcome::Succeeded { .. }
    ));

    let runtime = repo
        .runtime_get_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("runtime row");
    assert_eq!(runtime.status, RunStatus::Idle);
    assert!(runtime.thread_id.is_some());
    assert!(state.harness.get(&runtime.id).is_some());

    let harness = state.harness.get(&runtime.id).unwrap();
    let thread_id = runtime.thread_id.clone().unwrap();
    let turn_id = "turn-interrupt".to_string();
    state
        .shared_codex_appserver
        .set_active_turn_for_test(&thread_id, &turn_id);
    harness
        .set_state_for_test(HarnessState::TurnRunning {
            turn_id: turn_id.clone(),
            started_at: Instant::now(),
        })
        .await;
    let interrupt_id = state
        .operation_runtime
        .submit(
            "spec-harness-interrupt",
            key(),
            serde_json::to_value(SpecHarnessInterruptOperationPayload {
                runtime_id: runtime.id.clone(),
                reason: "test interrupt".into(),
            })
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(matches!(
        wait_op(&state, &interrupt_id).await,
        OperationOutcome::Succeeded { .. }
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        state
            .shared_codex_appserver
            .active_turn_for_test(&thread_id)
            .is_none()
    );

    let shutdown_id = state
        .operation_runtime
        .submit(
            "spec-harness-shutdown",
            key(),
            serde_json::to_value(SpecHarnessShutdownOperationPayload {
                runtime_id: runtime.id.clone(),
            })
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(matches!(
        wait_op(&state, &shutdown_id).await,
        OperationOutcome::Succeeded { .. }
    ));
    let stored = repo.runtime_get_by_id(&runtime.id).await.unwrap().unwrap();
    assert_eq!(stored.status, RunStatus::Superseded);
    assert!(state.harness.get(&runtime.id).is_none());
}

#[tokio::test]
async fn start_adapter_reuses_checkpointed_thread_on_recovery() {
    let (state, repo) = state_with_fake_daemon().await;
    let wave = seed_wave(&repo).await;
    let card_id = new_id();
    let payload = serde_json::to_value(SpecHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        wave_id: wave.id.to_string(),
        card_id: Some(card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: wave.cwd.clone(),
        goal: Some("adapter goal".into()),
    })
    .unwrap();
    let op_id = state
        .operation_runtime
        .submit("spec-harness-start", key(), payload)
        .await
        .unwrap();
    assert!(matches!(
        wait_op(&state, &op_id).await,
        OperationOutcome::Succeeded { .. }
    ));
    let first_thread = repo
        .card_codex_thread_get_by_card(&card_id)
        .await
        .unwrap()
        .expect("thread row")
        .thread_id;
    assert_eq!(first_thread, "fake-thread-0001");

    sqlx::query(
        r#"UPDATE operations
              SET phase = 'app_server_interact',
                  phase_detail_json = ?1,
                  lease_owner = NULL,
                  lease_until_ms = NULL,
                  completed_at_ms = NULL
            WHERE id = ?2"#,
    )
    .bind(
        serde_json::to_string(&serde_json::json!({
            "kind": "mint_and_await",
            "thread_id": first_thread,
        }))
        .unwrap(),
    )
    .bind(&op_id)
    .execute(repo.pool())
    .await
    .unwrap();

    state.operation_runtime.drive().await.unwrap();
    assert!(matches!(
        wait_op(&state, &op_id).await,
        OperationOutcome::Succeeded { .. }
    ));
    let recovered_thread = repo
        .card_codex_thread_get_by_card(&card_id)
        .await
        .unwrap()
        .expect("thread row after recovery")
        .thread_id;
    assert_eq!(recovered_thread, "fake-thread-0001");
    assert!(
        state
            .shared_codex_appserver
            .cached_card_for_thread("fake-thread-0002")
            .is_none(),
        "recovery must not mint a second spec thread"
    );
}
