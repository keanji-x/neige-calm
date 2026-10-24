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
use calm_truth::db::RepoRead;

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
        crate::acp_planner::config::boot(repo.as_ref(), &adapter.acp_host)
            .await
            .unwrap();
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
    corrupt_predecessor(&fx, &predecessor).await;
    let result = fx.interact().await;
    let error = result.unwrap_err();
    assert!(error.to_string().contains("missing field"), "{error:?}");
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
    corrupt_predecessor(&fx, &predecessor).await;
    let result = fx.interact().await;
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
    sqlx::query("CREATE TABLE c44_displaced_fault (worker TEXT NOT NULL)")
        .execute(fx.repo.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO c44_displaced_fault(worker) VALUES (?)")
        .bind(&predecessor)
        .execute(fx.repo.pool())
        .await
        .unwrap();
    // A durable read fault after the checkpoint must not discard committed output.
    // At this boundary the displaced carrier has already passed its unclaimed fence.
    sqlx::query(concat!(
        "CREATE TRIGGER c44_displaced_fault AFTER UPDATE OF tx_output_json ON operations ",
        "BEGIN UPDATE worker_sessions SET provider_execution_json='{}' ",
        "WHERE id IN (SELECT worker FROM c44_displaced_fault); END",
    ))
    .execute(fx.repo.pool())
    .await
    .unwrap();
    let result = fx.interact().await;
    let error = result.unwrap_err();
    assert!(error.to_string().contains("missing field"), "{error:?}");
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

async fn runtime_stop_settles(path: &str) {
    use crate::provider_execution as execution;
    use workspace_runtime::execution::{ProcessAlias, TrackId, test_support};
    let mut fx = Fixture::new(AgentProvider::OpenCode, false).await;
    let root = tempfile::tempdir().unwrap();
    let supervisor = crate::test_support::execution::Fixture::new(fx.repo.db().clone()).await;
    fx.adapter
        .acp_host
        .execution
        .bind(supervisor.engine.clone());
    let worker = fx.worker();
    let row = fx
        .repo
        .session_projection_by_id(&worker)
        .await
        .unwrap()
        .unwrap();
    let mut intent = test_support::intent(root.path().to_owned());
    intent.metadata.consumer = crate::acp_planner::execution::CONSUMER.into();
    let id = intent.metadata.id.clone();
    intent.metadata.alias = ProcessAlias(format!("planner:{worker}:{}", id.as_str()));
    intent.metadata.track = TrackId(fx.output.output_string("track_id", "test").unwrap());
    intent.metadata.program.program = "/bin/sleep".into();
    intent.metadata.program.args = vec!["300".into()];
    let (_, guard) = fx
        .adapter
        .acp_host
        .provider_execution
        .admit(
            fx.repo.db(),
            &supervisor.engine,
            &worker,
            execution::PreparedIntent(intent),
            execution::Purpose::Turn { turn_id: new_id() },
        )
        .await
        .unwrap();
    execution::release::wait_spawned(&supervisor.engine, &id)
        .await
        .unwrap();
    drop(guard);
    let factory = PlannerFactory::new(
        Arc::downgrade(&fx.adapter.daemon),
        &fx.adapter.claude_host,
        &fx.adapter.acp_host,
    );
    match path {
        "shutdown" => factory.shutdown_replay(&row).await.unwrap(),
        "predecessor" => factory
            .stop_historical_predecessor(&AgentProvider::OpenCode, "card", &worker)
            .await
            .unwrap(),
        "displaced" => factory
            .stop_displaced_historical(
                &AgentProvider::OpenCode,
                &fx.adapter.harness_registry,
                Some(&worker),
            )
            .await
            .unwrap(),
        "compensation" => {
            fx.output
                .set_output_data(SUCCESSOR_SESSION_KEY, json!(worker), "test")
                .unwrap();
            fx.adapter
                .compensate_step(
                    &CompensationStep::new(
                        "interrupt_thread",
                        json!({"card_id": fx.output.output_string("card_id", "test").unwrap()}),
                    ),
                    &fx.output,
                    &fx.op,
                    &fx.ctx,
                )
                .await
                .unwrap();
        }
        _ => unreachable!(),
    }
    assert!(
        execution::settlement(fx.repo.db(), &id)
            .await
            .unwrap()
            .is_some(),
        "{path} returned without Engine settlement"
    );
    assert!(
        calm_truth::db::sqlite::provider_execution::load(fx.repo.pool(), &worker)
            .await
            .unwrap()
            .is_none(),
        "owner completion clears the stopped claim"
    );
}

#[tokio::test]
async fn c4_acp_shutdown_replay_settles_execution() {
    runtime_stop_settles("shutdown").await;
}
#[tokio::test]
async fn c4_acp_historical_predecessor_settles_execution() {
    runtime_stop_settles("predecessor").await;
}
#[tokio::test]
async fn c4_acp_displaced_historical_settles_execution() {
    runtime_stop_settles("displaced").await;
}
#[tokio::test]
async fn c4_acp_compensation_settles_execution() {
    runtime_stop_settles("compensation").await;
}

async fn corrupt_predecessor(fx: &Fixture, predecessor: &str) {
    if fx
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(predecessor.into()))
        .await
        .unwrap()
        .is_none()
    {
        let card_id = fx.output.output_string("card_id", "test").unwrap();
        let card = fx.repo.card_get(&card_id).await.unwrap().unwrap();
        let provider = startable_provider(HarnessProfile::Planner, &card).unwrap();
        let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
        let mut init = starting_runtime_init(
            HarnessProfile::Planner,
            provider,
            predecessor.into(),
            card_id,
            None,
            serde_json::to_value(initial_snapshot_with_goal(None)).unwrap(),
            now_ms(),
        );
        init.status = crate::session_projection_repo::WorkerSessionState::Superseded;
        session_start_runtime_tx(&mut tx, init).await.unwrap();
        tx.commit().await.unwrap();
    }
    sqlx::query("UPDATE worker_sessions SET provider_execution_json='{}' WHERE id=?")
        .bind(predecessor)
        .execute(fx.repo.pool())
        .await
        .unwrap();
}
