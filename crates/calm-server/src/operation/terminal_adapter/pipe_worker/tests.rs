use super::*;
use crate::db::RepoOutOfDomain;
use crate::operation::forge_action_adapter::tests::executor::Executor;
use crate::{
    db::{RepoRead, sqlite::SqlxRepo},
    event::EventBus,
    operation::{
        OperationCompletionBus, OperationKey, OperationRepo, OperationRuntime, Phase,
        SqlxOperationRepo,
    },
    state::DaemonClient,
    terminal_renderer::TerminalRendererRegistry,
};

struct Harness {
    db: Arc<SqlxRepo>,
    ops: Arc<SqlxOperationRepo>,
    adapter: TerminalWorkerAdapter,
    ctx: SpawnCtx,
    runtime: OperationRuntime,
    task: crate::model::Task,
    id: String,
    _executor: Executor,
    root: tempfile::TempDir,
}
impl Harness {
    async fn new(command: &str) -> Self {
        Self::with_plan(command, None).await
    }
    async fn with_plan(command: &str, plan: Option<crate::db::sqlite::TrackWorkspacePlan>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = if let Some(plan) = plan {
            super::super::tests::terminal_worker_harness_with_plan(
                Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap()),
                root.path().to_str().unwrap(),
                &plan,
            )
            .await
        } else {
            super::super::tests::terminal_worker_harness_with_workspace(
                root.path().to_str().unwrap(),
            )
            .await
        };
        let db = base.repo;
        let ops = Arc::new(SqlxOperationRepo::new(db.db().clone()));
        let events = EventBus::new();
        let write = WriteContext::new(CardRoleCache::new(), TrackAreaCache::new());
        let task = crate::operation::launch_cleanup_test_support::claimed_task(db.clone(), events.clone(), write,
            &base.track_id, json!({"key":"pipe","kind":"terminal","command":command,"ready":true,"declared_by":"user"})).await;
        let executor = Executor::new(db.db().clone()).await;
        let ctx = SpawnCtx::new(
            db.clone(),
            ops.clone(),
            Arc::new(DaemonClient::new_stub()),
            TerminalRendererRegistry::new_with_repo(db.clone()),
            events.clone(),
            OperationCompletionBus::new(),
        )
        .with_execution(executor.lifecycle.clone());
        let adapter =
            TerminalWorkerAdapter::new(db.clone(), CardRoleCache::new(), TrackAreaCache::new())
                .with_log_directory(root.path().join("logs"));
        let runtime = OperationRuntime::new(
            ops.clone(),
            vec![Arc::new(adapter.clone())],
            events,
            ctx.completion.clone(),
            ctx.clone(),
        )
        .await
        .unwrap();
        let (_, payload) = crate::scheduler::build_worker_payload(&task).unwrap();
        let id = ops
            .insert_operation(
                "terminal-worker",
                OperationKey {
                    operation_key: new_id(),
                    idempotency_key: Some(task.id.clone()),
                    payload_hash: crate::routes::idempotency_key::stable_payload_hash(&payload)
                        .unwrap(),
                },
                payload,
            )
            .await
            .unwrap();
        Self {
            db,
            ops,
            adapter,
            ctx,
            runtime,
            task,
            id,
            _executor: executor,
            root,
        }
    }
    async fn op(&self) -> Operation {
        self.ops.get_operation(&self.id).await.unwrap().unwrap()
    }
    async fn prepare(&self) -> Operation {
        let op = self
            .ops
            .claim_drive_operation(&self.id)
            .await
            .unwrap()
            .unwrap();
        let prepared = self
            .adapter
            .prepare_external(&op.payload, &op, &self.ctx)
            .await
            .unwrap();
        self.ops
            .prepare_tx_and_advance(&op, &self.adapter, prepared)
            .await
            .unwrap()
            .committed()
            .unwrap();
        self.op().await
    }
    async fn park(&self) {
        self.prepare().await;
        // Production driver invokes spawn/admission and persists Parked before starting the observer.
        // Drop only the observer future to model the crash at that durable handoff.
        let op = self
            .ops
            .claim_drive_operation(&self.id)
            .await
            .unwrap()
            .unwrap();
        self.ops
            .set_phase(&op, Phase::SpawnStarted, None)
            .await
            .unwrap()
            .applied()
            .unwrap();
        let op = self
            .ops
            .claim_drive_operation(&self.id)
            .await
            .unwrap()
            .unwrap();
        let result = self
            .adapter
            .spawn_side_effect(
                super::super::super::required_output(&op).unwrap(),
                &op,
                &self.ctx,
            )
            .await
            .unwrap();
        assert!(matches!(result, SpawnOutcome::Parked { .. }));
        drop(result);
        self.ops
            .set_parked(&self.op().await, i64::MAX, Some(KEY))
            .await
            .unwrap()
            .unwrap();
    }
    async fn release(&self) {
        let op = self.ops.claim_parked(&self.id).await.unwrap().unwrap();
        self.adapter.act_pipe(&op, &self.ctx).await.unwrap();
        self.clear_claim().await;
    }
    async fn clear_claim(&self) {
        sqlx::query("UPDATE operations SET lease_owner=NULL,lease_until_ms=NULL WHERE id=?1")
            .bind(&self.id)
            .execute(self.db.pool())
            .await
            .unwrap();
    }
    async fn settled(&self) {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let op = self.ops.claim_parked(&self.id).await.unwrap().unwrap();
                let record = pipe::record(CONFIG, &op).unwrap();
                owned::drain_exited(&self.ctx, &op, &record.execution_id)
                    .await
                    .unwrap();
                self.clear_claim().await;
                self.ctx.execution.engine().unwrap().recover().await;
                if owned::settlement(&self.ctx, &record.execution_id)
                    .await
                    .unwrap()
                    .is_some()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
    }
    async fn complete(&self) {
        self.runtime.sweep_parked().await.unwrap();
    }
    async fn status(&self) -> crate::model::TaskStatus {
        self.db
            .task_get(&self.task.id)
            .await
            .unwrap()
            .unwrap()
            .status
    }
    async fn log_path(&self) -> PathBuf {
        Prepared::read(super::super::super::required_output(&self.op().await).unwrap())
            .unwrap()
            .log_path
    }
    async fn event_output(&self) -> Value {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT payload FROM events WHERE kind IN ('task.completed','task.failed')",
        )
        .fetch_all(self.db.pool())
        .await
        .unwrap();
        assert_eq!(rows.len(), 1);
        serde_json::from_str(&rows[0]).unwrap()
    }
    async fn boot(&self) {
        let plan = self.runtime.recover_on_boot().await.unwrap();
        self.runtime.apply_recovery(plan).await.unwrap();
    }
    async fn wait_done(&self) {
        tokio::time::timeout(Duration::from_secs(8), async {
            while self.op().await.phase != Phase::Succeeded {
                self.complete().await;
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn w_terminal_worker_exit_code_completes_task() {
    for (command, status) in [
        ("exit 0", crate::model::TaskStatus::Done),
        ("exit 17", crate::model::TaskStatus::Failed),
    ] {
        let h = Harness::new(command).await;
        h.park().await;
        let output = h.op().await.tx_output.unwrap();
        assert!(output.data.get("terminal_id").is_none());
        assert!(
            h.db.terminal_get_by_card(&output_card_id(&output).unwrap())
                .await
                .unwrap()
                .is_none()
        );
        h.release().await;
        h.settled().await;
        h.complete().await;
        assert_eq!(h.status().await, status);
        h.complete().await; // first writer only
        let event = h.event_output().await;
        assert_eq!(
            event
                .get("result")
                .or_else(|| event.get("details"))
                .unwrap()["source"],
            "terminal-exit"
        );
    }
}

#[tokio::test]
async fn w_terminal_worker_release_refused_when_superseded() {
    let h = Harness::new("printf forbidden > ran").await;
    h.park().await;
    // Recreate a released recovery allocation, then expose the stale live writer snapshot.
    let refs: String = sqlx::query_scalar("SELECT claim_context_json FROM tasks WHERE id=?1")
        .bind(&h.task.id)
        .fetch_one(h.db.pool())
        .await
        .unwrap();
    let origin = calm_types::task_recovery::TaskAttemptOrigin::Recovery {
        previous_attempt_id: h.task.id.clone(),
        idempotency_key: "retry".into(),
        request_fingerprint: "retry-fingerprint".into(),
        reason: "retry unchanged work".into(),
        actor: ActorId::Kernel,
        constraint: calm_types::task_recovery::TaskRecoveryConstraint::V1 {
            refs: serde_json::from_str(&refs).unwrap(),
            spawn: calm_types::task_recovery::TASK_IN_TRACK_ROUTE.into(),
            declared_by: "user".into(),
        },
    };
    sqlx::query("UPDATE tasks SET status='failed' WHERE id=?1")
        .bind(&h.task.id)
        .execute(h.db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO task_attempt_allocations(attempt_id,track_id,key,generation,origin_json,created_at_ms) \
         SELECT attempt_id||'-replacement',track_id,key,2,?2,created_at_ms \
         FROM task_attempt_allocations WHERE attempt_id=?1")
        .bind(&h.task.id).bind(serde_json::to_string(&origin).unwrap()).execute(h.db.pool()).await.unwrap();
    sqlx::query("UPDATE tasks SET status='dispatched' WHERE id=?1")
        .bind(&h.task.id)
        .execute(h.db.pool())
        .await
        .unwrap();
    h.release().await;
    h.settled().await;
    h.complete().await;
    assert!(!h.root.path().join("ran").exists());
    assert!(matches!(
        pipe::record(CONFIG, &h.op().await).unwrap().decision,
        Decision::Stopping {
            reason: StopReason::ReleaseRefused
        }
    ));
}

#[tokio::test]
async fn w_terminal_worker_secret_proxy_refused_before_admit() {
    let h = Harness::new("exit 0").await;
    h.db.settings_upsert("http_proxy", "http://user:password@proxy.example")
        .await
        .unwrap();
    let op = h.ops.claim_drive_operation(&h.id).await.unwrap().unwrap();
    assert!(
        h.adapter
            .prepare_external(&op.payload, &op, &h.ctx)
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM workspace_executions")
        .fetch_one(h.db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn w_terminal_worker_crash_after_settle_before_complete() {
    let h = Harness::new("printf 'persisted-tail\\n'; printf 'stderr-tail\\n' >&2").await;
    h.park().await;
    h.release().await;
    h.settled().await;
    assert_eq!(h.op().await.phase, Phase::Parked);
    h.boot().await;
    h.wait_done().await;
    let event = h.event_output().await;
    let text = event["result"]["pty_output"].as_str().unwrap();
    assert!(text.contains("persisted-tail"));
    assert!(text.contains("stderr-tail"));
    h.boot().await;
    h.complete().await;
    h.event_output().await;
}

#[tokio::test]
async fn w_terminal_worker_log_read_failure_retries() {
    let h = Harness::new("printf recovered-tail").await;
    h.park().await;
    h.release().await;
    h.settled().await;
    let log = h.log_path().await;
    let saved = std::fs::read(&log).unwrap();
    std::fs::remove_file(&log).unwrap();
    h.complete().await;
    assert_eq!(h.op().await.phase, Phase::Parked);
    assert_eq!(h.status().await, crate::model::TaskStatus::Dispatched);
    std::fs::write(log, saved).unwrap();
    h.clear_claim().await;
    h.complete().await;
    assert_eq!(h.status().await, crate::model::TaskStatus::Done);
}

#[tokio::test]
async fn w_terminal_worker_log_tail_truncated_flag() {
    let h = Harness::new("head -c 16000 /dev/zero | tr '\\000' x; printf final-tail").await;
    h.park().await;
    h.release().await;
    h.settled().await;
    h.complete().await;
    let event = h.event_output().await;
    assert_eq!(event["result"]["pty_output_truncated"], true);
    assert!(
        event["result"]["pty_output"]
            .as_str()
            .unwrap()
            .ends_with("final-tail")
    );
}

#[tokio::test]
async fn w_terminal_worker_kernel_restart_held_releases_only_with_authority() {
    for valid in [true, false] {
        let h = Harness::new("printf launched > ran").await;
        h.park().await;
        if !valid {
            sqlx::query("UPDATE tasks SET status='verifying' WHERE id=?1")
                .bind(&h.task.id)
                .execute(h.db.pool())
                .await
                .unwrap();
        }
        h.boot().await;
        h.wait_done().await;
        assert_eq!(h.root.path().join("ran").exists(), valid);
    }
}

#[tokio::test]
async fn w_terminal_worker_kernel_restart_releasing_stops_without_resend() {
    let h = Harness::new("printf forbidden > ran").await;
    h.park().await;
    let op = h.ops.claim_parked(&h.id).await.unwrap().unwrap();
    let old = pipe::record(CONFIG, &op).unwrap();
    let mut new = old.clone();
    new.release = Release::Releasing {
        authorized_at_ms: crate::model::now_ms(),
    };
    pipe::transition(
        CONFIG,
        &h.ctx,
        &op,
        &old,
        &new,
        Some(&Authority(&h.task.id)),
    )
    .await
    .unwrap();
    h.clear_claim().await;
    h.boot().await;
    h.wait_done().await;
    assert!(!h.root.path().join("ran").exists());
    assert_eq!(
        pipe::record(CONFIG, &h.op().await).unwrap().release,
        new.release
    );
}

#[tokio::test]
async fn w_terminal_worker_kernel_restart_released_keeps_observing() {
    let h = Harness::new("sleep .3; printf continued > ran").await;
    h.park().await;
    h.release().await;
    h.boot().await;
    h.wait_done().await;
    assert!(h.root.path().join("ran").exists());
    assert_eq!(h.status().await, crate::model::TaskStatus::Done);
}

#[tokio::test]
async fn w_terminal_worker_track_view_reads_log() {
    use calm_truth::track_fs_view::{TrackFsError, TrackFsView};
    let h = Harness::new("printf view-log").await;
    h.park().await;
    h.release().await;
    h.settled().await;
    let track = h.db.track_get(&h.task.track_id).await.unwrap().unwrap();
    let write = WriteContext::new(CardRoleCache::new(), TrackAreaCache::new());
    let view = TrackFsView::new(h.db.as_ref(), &write).with_gate_log_access(h.root.path().into());
    let path = format!("runs/{}/terminal-worker.log", h.task.id);
    assert!(
        view.cat(&track, &path)
            .await
            .unwrap()
            .content
            .contains("view-log")
    );
    let mut foreign = track.clone();
    foreign.id = "another-track".into();
    assert!(matches!(
        view.cat(&foreign, &path).await,
        Err(TrackFsError::Forbidden(_))
    ));
    assert!(matches!(
        TrackFsView::new(h.db.as_ref(), &write)
            .cat(&track, &path)
            .await,
        Err(TrackFsError::Forbidden(_))
    ));
}

#[tokio::test]
async fn w_pre_upgrade_terminal_worker_stays_legacy() {
    use crate::operation::terminal_adapter::legacy_test_adapter::LegacyPreparedWorker;
    let h = Harness::new("exit 0").await;
    let claimed = h.ops.claim_drive_operation(&h.id).await.unwrap().unwrap();
    h.ops
        .prepare_tx_and_advance(&claimed, &LegacyPreparedWorker(h.adapter.clone()), None)
        .await
        .unwrap()
        .committed()
        .unwrap();
    let output = h.op().await.tx_output.unwrap();
    let terminal = output
        .output_string("terminal_id", "legacy fixture")
        .unwrap();
    // An old kernel may have already observed the command's exit before its operation ack.
    h.db.terminal_set_exit_with_output(&terminal, Some(0), false, "legacy", false)
        .await
        .unwrap();
    h.clear_claim().await;
    // Re-drive with the real new adapter: it must preserve the old terminal, without Engine admission.
    h.runtime.drive().await.unwrap();
    assert_eq!(h.op().await.phase, Phase::Succeeded);
    assert!(h.db.terminal_get(&terminal).await.unwrap().is_some());
    assert!(h.op().await.tx_output.unwrap().data.get(KEY).is_none());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM workspace_executions")
        .fetch_one(h.db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn w_terminal_worker_admission_refuses_verifying() {
    let h = Harness::new("printf forbidden > ran").await;
    h.prepare().await;
    let op = h.ops.claim_drive_operation(&h.id).await.unwrap().unwrap();
    h.ops
        .set_phase(&op, Phase::SpawnStarted, None)
        .await
        .unwrap()
        .applied()
        .unwrap();
    let op = h.ops.claim_drive_operation(&h.id).await.unwrap().unwrap();
    sqlx::query("UPDATE tasks SET status='verifying' WHERE id=?1")
        .bind(&h.task.id)
        .execute(h.db.pool())
        .await
        .unwrap();
    let result = h
        .adapter
        .spawn_side_effect(
            super::super::super::required_output(&op).unwrap(),
            &op,
            &h.ctx,
        )
        .await;
    assert!(matches!(result, Err(CalmError::Conflict(_))));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM workspace_executions")
        .fetch_one(h.db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert!(!h.root.path().join("ran").exists());
}

mod disposal;

mod preparation;

mod handoff;
