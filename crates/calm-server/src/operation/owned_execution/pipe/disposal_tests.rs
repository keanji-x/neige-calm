//! Exercise the real Pipe transaction entry points and registered disposal ports.
use super::*;
use crate::{
    db::{RouteRepo, sqlite::SqlxRepo},
    event::EventBus,
    execution_quiesce::owned::{DisposalOwner, dispose},
    execution_store::{ExecutionProjection, SqliteExecutionStore},
    operation::{
        OperationCompletionBus, OperationKey, OperationRepo, OperationRuntime, SqlxOperationRepo,
        TxOutput,
    },
    state::DaemonClient,
    terminal_renderer::TerminalRendererRegistry,
};
use serde_json::json;
use std::sync::Arc;

const CONFIG: PipeConfig = PipeConfig {
    consumer: "disposal-test",
    alias_prefix: "disposal-test",
    record_key: "terminal_launch",
};
struct Projection;
#[async_trait::async_trait]
impl ExecutionProjection for Projection {
    fn disposal(&self) -> Option<&dyn DisposalOwner> {
        Some(&CONFIG)
    }
    async fn spawned(
        &self,
        _: &mut Tx<'_>,
        _: &str,
        _: i64,
    ) -> std::result::Result<(), sqlx::Error> {
        Ok(())
    }
    async fn settled(
        &self,
        _: &mut Tx<'_>,
        _: &str,
        _: &Settlement,
        _: i64,
    ) -> std::result::Result<(), sqlx::Error> {
        Ok(())
    }
}
struct Harness {
    db: Arc<SqlxRepo>,
    repo: Arc<SqlxOperationRepo>,
    ctx: SpawnCtx,
    runtime: Arc<OperationRuntime>,
    store: SqliteExecutionStore,
    op: Operation,
    metadata: ExecutionMetadata,
    _executor: crate::operation::forge_action_adapter::tests::executor::Executor,
}
impl Harness {
    async fn new() -> Self {
        Self::with_card(false).await
    }
    async fn with_card(card_target: bool) -> Self {
        let db = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let (track, card) = if card_target {
            card_tests::seed_card(&db).await
        } else {
            ("track".into(), "card".into())
        };
        let repo = Arc::new(SqlxOperationRepo::new(db.db().clone()));
        let id = repo
            .insert_operation(
                "disposal-test",
                OperationKey {
                    operation_key: crate::model::new_id(),
                    idempotency_key: None,
                    payload_hash: "hash".into(),
                },
                json!({}),
            )
            .await
            .unwrap();
        let mut output = TxOutput::new("card", Some(card.clone()), json!({}));
        output.data = json!({});
        sqlx::query("UPDATE operations SET phase='spawn_started',tx_output_json=?2 WHERE id=?1")
            .bind(&id)
            .bind(serde_json::to_string(&output).unwrap())
            .execute(db.pool())
            .await
            .unwrap();
        if card_target {
            sqlx::query("UPDATE operations SET target_type='card',target_id=?2 WHERE id=?1")
                .bind(&id)
                .bind(&card)
                .execute(db.pool())
                .await
                .unwrap();
        }
        let op = repo.claim_drive_operation(&id).await.unwrap().unwrap();
        let executor =
            crate::operation::forge_action_adapter::tests::executor::Executor::new(db.db().clone())
                .await;
        let secret = serde_json::from_reader(
            std::fs::File::open(executor._root.path().join("key.json")).unwrap(),
        )
        .unwrap();
        let projection: Arc<dyn ExecutionProjection> = if card_target {
            Arc::new(card_tests::WorkerProjection)
        } else {
            Arc::new(Projection)
        };
        let store = SqliteExecutionStore::new(db.db().clone())
            .with_projection(CONFIG.consumer.into(), projection);
        let engine = Arc::new(Engine::new(
            store.clone(),
            session_executor::SessionExecutor::new(
                executor._socket.path().join("control.sock"),
                secret,
            ),
        ));
        assert!(engine.recover().await.error.is_none());
        engine.output_generation().await.unwrap();
        let lifecycle = crate::execution_quiesce::ExecutionLifecycle::default();
        lifecycle.bind(engine);
        let route: Arc<dyn RouteRepo> = db.clone();
        let ctx = SpawnCtx::new(
            route.clone(),
            repo.clone(),
            Arc::new(DaemonClient::new_stub()),
            TerminalRendererRegistry::new_with_repo(route),
            EventBus::new(),
            OperationCompletionBus::new(),
        )
        .with_execution(lifecycle);
        let root = executor._root.path().canonicalize().unwrap();
        admit(
            CONFIG,
            &op,
            &ctx,
            root,
            &track,
            ExecutionSpecification {
                ambient_environment: calm_session::control::execution::AmbientEnvironment::None,
                program: "/bin/sh".into(),
                args: vec![
                    "-c".into(),
                    if card_target {
                        "trap 'exit 0' TERM; printf ready; read -r go".into()
                    } else {
                        "read -r go".into()
                    },
                ],
                environment: vec![],
                io_mode: calm_session::control::IoMode::Pipe,
                replay_bytes: 8192,
            },
        )
        .await
        .unwrap();
        let op = repo.get_operation(&id).await.unwrap().unwrap();
        let record = record(CONFIG, &op).unwrap();
        let mut tx = crate::db::sqlite::begin_immediate_tx(db.db())
            .await
            .unwrap();
        let metadata: ExecutionMetadata = serde_json::from_str(
            &calm_truth::db::sqlite::workspace_execution::intent_tx(
                &mut tx,
                record.execution_id.as_str(),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        tx.commit().await.unwrap();
        // These cases exercise a running child. A Stop before Spawned legitimately
        // revokes the reservation and can clear secrets without an owner Retire.
        let running = ctx.execution.engine().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let generation = running.output_generation().await.unwrap();
                if matches!(
                    running.output(&metadata.id, None).await,
                    Ok(ExecutionReply::Observation {
                        state: ExecutionState::Spawned,
                        receipt: Some(_),
                        ref output,
                        ..
                    }) if !card_target || output == b"ready"
                ) {
                    break;
                }
                generation.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        let runtime = Arc::new(OperationRuntime::new_unchecked(
            repo.clone(),
            vec![],
            ctx.events.clone(),
            ctx.completion.clone(),
            ctx.clone(),
        ));
        Self {
            db,
            repo,
            ctx,
            runtime,
            store,
            op,
            metadata,
            _executor: executor,
        }
    }
    async fn stop(&self) {
        let mut tx = crate::db::sqlite::begin_immediate_tx(self.db.db())
            .await
            .unwrap();
        CONFIG
            .prepare_disposal_tx(&mut tx, &self.metadata)
            .await
            .unwrap();
        let _events = CONFIG.stop_tx(&mut tx, &self.metadata).await.unwrap();
        tx.commit().await.unwrap();
        self.ctx.execution.drive([self.metadata.id.clone()]);
    }
    async fn settle(&self) {
        self.ctx
            .execution
            .engine()
            .unwrap()
            .wait_settled(&self.metadata.id)
            .await
            .unwrap();
    }
}
struct Authority;
#[async_trait::async_trait]
impl ReleaseAuthorization for Authority {
    async fn authorize_tx(&self, _: &mut Tx<'_>) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn w_owned_pipe_disposal_fences_stale_release_completion_and_readmission() {
    let h = Harness::new().await;
    h.stop().await;
    let current = h.repo.get_operation(&h.op.id).await.unwrap().unwrap();
    assert_eq!(current.lease_owner, h.op.lease_owner);
    assert_eq!(current.lease_until_ms, h.op.lease_until_ms);
    assert_eq!(
        record(CONFIG, &current).unwrap().decision,
        Decision::Stopping {
            reason: StopReason::Disposed
        }
    );
    // A pre-disposal snapshot cannot release, acknowledge release, or retire a successful verdict.
    for op in [&h.op, &current] {
        assert!(
            release(
                CONFIG,
                &h.ctx,
                op,
                &Authority,
                ReleaseRequest {
                    token: b"go\n",
                    timeout: std::time::Duration::from_secs(1),
                },
                #[cfg(any(test, feature = "fixtures"))]
                None
            )
            .await
            .is_err()
        );
    }
    let mut tx = crate::db::sqlite::begin_immediate_tx(h.db.db())
        .await
        .unwrap();
    assert!(retire(CONFIG, &mut tx, &h.op).await.is_err());
    tx.rollback().await.unwrap();
    let root = h._executor._root.path().canonicalize().unwrap();
    let program = h.metadata.program.clone();
    assert!(
        admit(CONFIG, &h.op, &h.ctx, root, "track", program)
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspace_executions")
        .fetch_one(h.db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
    // Replayed disposal does not replace the live observer lease or change the reason.
    let mut tx = crate::db::sqlite::begin_immediate_tx(h.db.db())
        .await
        .unwrap();
    assert!(
        !CONFIG
            .stop_for_disposal_tx(&mut tx, &h.metadata)
            .await
            .unwrap()
    );
    tx.rollback().await.unwrap();
    h.stop().await;
    h.settle().await;
}

#[tokio::test]
async fn w_owned_pipe_disposal_is_atomic_and_preserves_natural_completion() {
    let h = Harness::new().await;
    let mut tx = crate::db::sqlite::begin_immediate_tx(h.db.db())
        .await
        .unwrap();
    assert!(
        CONFIG
            .stop_for_disposal_tx(&mut tx, &h.metadata)
            .await
            .unwrap()
    );
    tx.rollback().await.unwrap();
    let row =
        calm_truth::db::sqlite::workspace_execution::load(h.db.pool(), h.metadata.id.as_str())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(row.desired, "run");
    assert_eq!(
        record(
            CONFIG,
            &h.repo.get_operation(&h.op.id).await.unwrap().unwrap()
        )
        .unwrap()
        .decision,
        Decision::Observing
    );
    // Natural owner completion wins before the disposal transaction.
    sqlx::query("UPDATE operations SET phase='succeeded',completed_at_ms=1 WHERE id=?1")
        .bind(&h.op.id)
        .execute(h.db.pool())
        .await
        .unwrap();
    let mut tx = crate::db::sqlite::begin_immediate_tx(h.db.db())
        .await
        .unwrap();
    assert!(
        !CONFIG
            .stop_for_disposal_tx(&mut tx, &h.metadata)
            .await
            .unwrap()
    );
    tx.commit().await.unwrap();
    let row =
        calm_truth::db::sqlite::workspace_execution::load(h.db.pool(), h.metadata.id.as_str())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(row.desired, "run");
    assert!(CONFIG.finish(&h.runtime, &h.metadata).await.unwrap());
    // The owner would normally have already retired it. Stop our deliberately held fixture.
    let mut tx = crate::db::sqlite::begin_immediate_tx(h.db.db())
        .await
        .unwrap();
    calm_truth::db::sqlite::workspace_execution::request(&mut tx, h.metadata.id.as_str(), "stop")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    h.ctx.execution.drive([h.metadata.id.clone()]);
    h.settle().await;
}

#[tokio::test]
async fn w_owned_disposal_missing_ports_and_corrupt_owner_retain_execution() {
    let h = Harness::new().await;
    // A later unreadable owner prevents every earlier prepared owner from stopping.
    assert!(matches!(
        dispose(
            &h.store,
            &h.runtime,
            &[h.metadata.id.clone(), ExecutionId::fresh().unwrap()]
        )
        .await,
        Err(CalmError::Conflict(_))
    ));
    let mut damaged = h.metadata.clone();
    damaged.alias.0 = "wrong-alias".into();
    let mut tx = crate::db::sqlite::begin_immediate_tx(h.db.db())
        .await
        .unwrap();
    assert!(matches!(
        CONFIG.prepare_disposal_tx(&mut tx, &damaged).await,
        Err(CalmError::Conflict(_))
    ));
    tx.rollback().await.unwrap();
    let unregistered = SqliteExecutionStore::new(h.db.db().clone());
    let no_ports = SqliteExecutionStore::new(h.db.db().clone()).with_projection(
        CONFIG.consumer.into(),
        Arc::new(crate::execution_store::tests::NoopProjection),
    );
    for store in [&unregistered, &no_ports] {
        assert!(matches!(
            dispose(store, &h.runtime, std::slice::from_ref(&h.metadata.id)).await,
            Err(CalmError::Conflict(_))
        ));
    }
    sqlx::query("UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.data.terminal_launch.version',2) WHERE id=?1").bind(&h.op.id).execute(h.db.pool()).await.unwrap();
    assert!(matches!(
        dispose(&h.store, &h.runtime, std::slice::from_ref(&h.metadata.id)).await,
        Err(CalmError::Conflict(_))
    ));
    let row =
        calm_truth::db::sqlite::workspace_execution::load(h.db.pool(), h.metadata.id.as_str())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(row.desired, "run");
    assert!(row.secrets_json.is_some());
    assert_eq!(
        h.repo
            .get_operation(&h.op.id)
            .await
            .unwrap()
            .unwrap()
            .lease_owner,
        h.op.lease_owner
    );
    // Restore only the faulted record field before orderly fixture cleanup.
    sqlx::query("UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.data.terminal_launch.version',1) WHERE id=?1").bind(&h.op.id).execute(h.db.pool()).await.unwrap();
    h.stop().await;
    h.settle().await;
}

#[tokio::test]
async fn w_owned_disposal_waits_for_owner_completion_and_retirement() {
    let h = Harness::new().await;
    let original_lease = h.op.lease_owner.clone();
    let ids = [h.metadata.id.clone(), h.metadata.id.clone()];
    let mut disposed = Box::pin(dispose(&h.store, &h.runtime, &ids));
    // Keep an independently claimed live owner unfinished after Engine settlement.
    tokio::select! {
        result = &mut disposed => panic!("disposal completed before the owner: {result:?}"),
        () = async {
            h.settle().await;
            let row = calm_truth::db::sqlite::workspace_execution::load(h.db.pool(), h.metadata.id.as_str()).await.unwrap().unwrap();
            assert_eq!(row.desired, "stop");
            assert!(row.secrets_json.is_some());
            assert_eq!(h.repo.get_operation(&h.op.id).await.unwrap().unwrap().lease_owner, original_lease);
        } => {}
    }
    assert!(matches!(
        futures::poll!(disposed.as_mut()),
        std::task::Poll::Pending
    ));
    // Production owner completion/retire is the obligation being coordinated; use the
    // real completion record fence rather than replacing the owner's live claim.
    let current = h.repo.get_operation(&h.op.id).await.unwrap().unwrap();
    let mut tx = crate::db::sqlite::begin_immediate_tx(h.db.db())
        .await
        .unwrap();
    crate::operation::repo_sqlite::require_live_claim_tx(&mut tx, &current)
        .await
        .unwrap();
    retire(CONFIG, &mut tx, &current).await.unwrap();
    sqlx::query("UPDATE operations SET phase='succeeded',completed_at_ms=1 WHERE id=?1")
        .bind(&h.op.id)
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    h.ctx.execution.drive([h.metadata.id.clone()]);
    disposed.await.unwrap();
    let row =
        calm_truth::db::sqlite::workspace_execution::load(h.db.pool(), h.metadata.id.as_str())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(row.desired, "retire");
    assert!(row.settlement_json.is_some());
    assert!(row.secrets_json.is_none());
}

#[tokio::test]
async fn w_owned_disposal_timeout_retains_scope_and_original_claim() {
    let h = Harness::new().await;
    sqlx::query(
        "UPDATE operations SET phase='parked',parked_at_ms=1,parked_deadline_ms=?2 WHERE id=?1",
    )
    .bind(&h.op.id)
    .bind(crate::model::now_ms() + 60_000)
    .execute(h.db.pool())
    .await
    .unwrap();
    assert!(matches!(
        dispose(&h.store, &h.runtime, std::slice::from_ref(&h.metadata.id)).await,
        Err(CalmError::Conflict(_))
    ));
    let row =
        calm_truth::db::sqlite::workspace_execution::load(h.db.pool(), h.metadata.id.as_str())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(row.desired, "stop");
    assert!(row.settlement_json.is_some());
    assert!(row.secrets_json.is_some());
    let current = h.repo.get_operation(&h.op.id).await.unwrap().unwrap();
    assert_eq!(current.lease_owner, h.op.lease_owner);
    assert_eq!(current.lease_until_ms, h.op.lease_until_ms);
    assert_eq!(
        record(CONFIG, &current).unwrap().decision,
        Decision::Stopping {
            reason: StopReason::Disposed
        }
    );
    // A retry can continue the obligation; a timeout never converts it into success.
    assert!(matches!(
        dispose(&h.store, &h.runtime, std::slice::from_ref(&h.metadata.id)).await,
        Err(CalmError::Conflict(_))
    ));
}

#[cfg(test)]
mod card_tests;
