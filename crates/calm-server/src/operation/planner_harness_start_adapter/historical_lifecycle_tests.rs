#![cfg(test)]
//! Exercise actual start phases, including their durable operation checkpoint.
use super::*;
use crate::db::prelude::*;
use crate::db::sqlite::{SqlxRepo, begin_immediate_tx};
use crate::event::EventBus;
use crate::operation::{OperationCompletionBus, OperationKey, OperationRepo, SqlxOperationRepo};
use crate::plugin_host::PluginRegistry;
use crate::state::DaemonClient;
use crate::terminal_renderer::TerminalRendererRegistry;

struct Fixture {
    repo: Arc<SqlxRepo>,
    adapter: PlannerHarnessStartAdapter,
    ctx: SpawnCtx,
    op: Operation,
    output: TxOutput,
}

impl Fixture {
    async fn new(provider: AgentProvider, deferred: bool) -> Self {
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let track = super::tests::make_track(&repo, None, None, None).await;
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        let card = card_create_with_id_tx(
            &mut tx, new_id(), NewCard {
                track_id: track.id.clone(), kind: "codex".into(), sort: None, title: None,
                payload: json!({"schemaVersion":1,"planner_harness":true,"planner_provider":provider}),
            }, CardRole::Planner, false, repo.card_role_cache(),
        ).await.unwrap();
        tx.commit().await.unwrap();
        let events = EventBus::new();
        let plugin = Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join(new_id()),
            vec![],
            events.clone(),
            WriteContext::new(
                repo.card_role_cache().clone(),
                repo.track_area_cache().clone(),
            ),
        ));
        let adapter = super::tests::adapter_for(repo.clone(), plugin);
        let operations = Arc::new(SqlxOperationRepo::new(repo.db().clone()));
        let payload = serde_json::to_value(super::tests::historical_start_payload(
            ActorId::Kernel,
            track.id.to_string(),
            card.id,
            HarnessProfile::Planner,
            deferred,
            None,
        ))
        .unwrap();
        let id = operations
            .insert_operation(
                crate::routes::conversations_shared::PLANNER_HARNESS_START,
                OperationKey {
                    operation_key: new_id(),
                    idempotency_key: None,
                    payload_hash: new_id(),
                },
                payload.clone(),
            )
            .await
            .unwrap();
        let pending = operations.get_operation(&id).await.unwrap().unwrap();
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        let output = adapter
            .prepare_tx(&mut tx, &payload, &pending)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        sqlx::query("UPDATE operations SET phase='app_server_interact',phase_detail_json=?1,tx_output_json=?2 WHERE id=?3")
            .bind(r#"{"kind":"mint_and_await","thread_id":null}"#)
            .bind(serde_json::to_string(&output).unwrap()).bind(id)
            .execute(repo.pool()).await.unwrap();
        let op = operations
            .claim_drive_batch(1)
            .await
            .unwrap()
            .pop()
            .unwrap();
        let ctx = SpawnCtx::new(
            repo.clone(),
            operations,
            Arc::new(DaemonClient::new_stub()),
            TerminalRendererRegistry::new_with_repo(repo.clone()),
            events,
            OperationCompletionBus::new(),
        );
        Self {
            repo,
            adapter,
            ctx,
            op,
            output,
        }
    }

    async fn interact(&mut self) -> Result<AppServerInteractOutcome> {
        self.adapter
            .app_server_interact(&mut self.output, &self.op, &self.ctx)
            .await
    }

    fn worker(&self) -> String {
        self.output
            .output_string(SUCCESSOR_SESSION_KEY, "test")
            .unwrap()
    }
}

#[tokio::test]
async fn historical_local_start_keeps_native_attribution_without_eager_credentials_or_rpc() {
    for provider in [
        AgentProvider::Claude,
        AgentProvider::OpenCode,
        AgentProvider::Grok,
    ] {
        let mut fx = Fixture::new(provider, false).await;
        let worker = fx.worker();
        sqlx::query("UPDATE worker_sessions SET agent_session_id='native-existing' WHERE id=?1")
            .bind(&worker)
            .execute(fx.repo.pool())
            .await
            .unwrap();
        // Missing socket and a stopped daemon must be irrelevant to a local mint.
        fx.adapter.mcp_socket_path = None;
        fx.interact().await.unwrap();
        let row = fx
            .repo
            .session_get(&calm_types::worker::WorkerSessionId(worker))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.agent_session_id.as_deref(), Some("native-existing"));
        assert!(uuid::Uuid::parse_str(row.thread_id.as_deref().unwrap()).is_ok());
        let hashes: i64 = sqlx::query_scalar("SELECT count(*) FROM card_mcp_tokens")
            .fetch_one(fx.repo.pool())
            .await
            .unwrap();
        assert_eq!(hashes, 0, "local start must not mint a credential");
        assert!(
            fx.adapter.harness_registry.get(&fx.worker()).is_none(),
            "identity preparation cannot install a harness"
        );
    }
}

#[tokio::test]
async fn historical_acp_predecessor_stop_failure_is_strict_before_mint() {
    let mut fx = Fixture::new(AgentProvider::OpenCode, false).await;
    let predecessor = new_id();
    fx.output
        .set_output_data(PREDECESSOR_SESSION_KEY, json!(predecessor), "test")
        .unwrap();
    crate::planner_process::fail_claude_planner_stop_for_test(&predecessor);
    let result = fx.interact().await;
    crate::planner_process::clear_claude_planner_stop_failure_for_test(&predecessor);
    assert!(result.unwrap_err().to_string().contains("failed (fixture)"));
    assert!(output_existing_thread_id(&fx.output).unwrap().is_none());
}

#[tokio::test]
async fn historical_claude_predecessor_stop_failure_is_best_effort() {
    let mut fx = Fixture::new(AgentProvider::Claude, false).await;
    let predecessor = new_id();
    fx.output
        .set_output_data(PREDECESSOR_SESSION_KEY, json!(predecessor), "test")
        .unwrap();
    // Keep the operation's durable checkpoint equal to its real replay input.
    sqlx::query("UPDATE operations SET tx_output_json=?1 WHERE id=?2")
        .bind(serde_json::to_string(&fx.output).unwrap())
        .bind(&fx.op.id)
        .execute(fx.repo.pool())
        .await
        .unwrap();
    fx.op.tx_output = Some(fx.output.clone());
    crate::planner_process::fail_claude_planner_stop_for_test(&predecessor);
    let result = fx.interact().await;
    crate::planner_process::clear_claude_planner_stop_failure_for_test(&predecessor);
    result.unwrap();
    assert!(output_existing_thread_id(&fx.output).unwrap().is_some());
}

#[tokio::test]
async fn historical_acp_displaced_stop_failure_keeps_committed_output() {
    let mut fx = Fixture::new(AgentProvider::Grok, true).await;
    let worker = fx.worker();
    let card = fx.output.output_string("card_id", "test").unwrap();
    let predecessor = new_id();
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    session_supersede_and_start_tx(
        &mut tx,
        &worker,
        starting_runtime_init(
            HarnessProfile::Planner,
            AgentProvider::Grok,
            predecessor.clone(),
            card,
            Some("another-operations-thread".into()),
            serde_json::to_value(initial_snapshot_with_goal(None)).unwrap(),
            now_ms(),
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    crate::planner_process::fail_claude_planner_stop_for_test(&predecessor);
    let result = fx.interact().await;
    crate::planner_process::clear_claude_planner_stop_failure_for_test(&predecessor);
    assert!(result.unwrap_err().to_string().contains("failed (fixture)"));
    let stored = fx
        .ctx
        .operation_repo
        .get_operation(&fx.op.id)
        .await
        .unwrap()
        .unwrap()
        .tx_output
        .unwrap();
    assert_eq!(
        serde_json::to_value(&fx.output).unwrap(),
        serde_json::to_value(stored).unwrap()
    );
    assert_eq!(fx.output.data[PREDECESSOR_SESSION_KEY], predecessor);
    assert_ne!(
        fx.output.data["codex_thread_id"],
        "another-operations-thread"
    );
    assert_eq!(fx.worker(), worker);
}

#[tokio::test]
async fn historical_native_start_clears_local_attribution_and_checkpoints_credentials() {
    let mut fx = Fixture::new(AgentProvider::Codex, false).await;
    fx.adapter.daemon = SharedCodexAppServer::new_fake_running_with_pending(fx.repo.clone(), None);
    let worker = fx.worker();
    sqlx::query("UPDATE worker_sessions SET agent_session_id='stale-local-session' WHERE id=?1")
        .bind(&worker)
        .execute(fx.repo.pool())
        .await
        .unwrap();
    fx.interact().await.unwrap();
    let row = fx
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(worker))
        .await
        .unwrap()
        .unwrap();
    assert!(row.agent_session_id.is_none());
    assert!(row.thread_id.is_some());
    assert!(row.mcp_token_hash.is_some());
    let hash: String = sqlx::query_scalar("SELECT hashed_token FROM card_mcp_tokens")
        .fetch_one(fx.repo.pool())
        .await
        .unwrap();
    assert_eq!(row.mcp_token_hash, Some(hash));
}
