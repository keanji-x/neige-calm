//! Live parent authority through Report → Scheduler → child operation.
use super::*;

fn adapter(boot: &Boot, workspace_root: PathBuf) -> Arc<dyn ProviderAdapter> {
    Arc::new(ChildTrackAdapter::new(
        Arc::new(calm_server::acp_planner::config::AcpPlannerHost::unconfigured_scratch().unwrap()),
        boot.card_role_cache.clone(),
        boot.track_area_cache.clone(),
        workspace_root,
        PathBuf::from("/unused-objective-checkouts"),
        workspace_runtime::sweep::TeardownTargets::default(),
    ))
}

async fn inventory(boot: &Boot) -> (i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM tracks), (SELECT count(*) FROM cards)")
        .fetch_one(boot.repo.sqlite_db().unwrap().read_pool())
        .await
        .unwrap()
}

fn worktrees(repository: &std::path::Path) -> Vec<u8> {
    let output = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repository)
        .output()
        .unwrap();
    assert!(output.status.success());
    output.stdout
}

#[tokio::test]
async fn subtrack_admin_unprivileged_report_fails_terminal_without_allocation() {
    let (boot, repository) = child_track_config::boot_git_parent().await;
    let workspace_root = tempfile::tempdir().unwrap();
    let mut task = plan_task(&boot.track_id, "no-admin", TaskKind::Codex, &[]);
    task.spawn = "sub-track".into();
    let task_id = task.id.clone();
    seed_projected_task(&boot, task).await;
    let before = inventory(&boot).await;
    let before_worktrees = worktrees(repository.path());
    let (runtime, scheduler) = build_scheduler(
        &boot,
        vec![adapter(&boot, workspace_root.path().to_path_buf())],
    );
    scheduler.schedule_track(boot.track_id.clone()).await;
    let failed = boot.repo.task_get(&task_id).await.unwrap().unwrap();
    assert_eq!(
        failed.status,
        TaskStatus::Failed,
        "unauthorized allocation: before={before:?}, after={:?}",
        inventory(&boot).await
    );
    assert_eq!(
        status_detail_class(failed.status_detail.as_deref().unwrap()),
        "child-track-create-failed"
    );
    let db = boot.repo.sqlite_db().unwrap();
    let (op, phase, error): (String, String, String) =
        sqlx::query_as("SELECT id,phase,last_error FROM operations WHERE kind='child-track'")
            .fetch_one(db.read_pool())
            .await
            .unwrap();
    assert_eq!((phase, error), ("failed".into(), "not_admin".into()));
    assert!(matches!(runtime.wait(&op).await.unwrap().outcome,
        OperationOutcome::Failed { last_error, last_error_class: Some(class), .. }
        if last_error == "not_admin" && class == "forbidden"));
    for _ in 0..2 {
        runtime.drive().await.unwrap();
        scheduler.sweep_all().await;
    }
    assert_eq!(
        boot.repo.task_get(&task_id).await.unwrap().unwrap().status,
        TaskStatus::Failed
    );
    assert_eq!(operation_count(&boot, "child-track").await, 1);
    assert_eq!(inventory(&boot).await, before);
    assert_eq!(worktrees(repository.path()), before_worktrees);
    assert_eq!(std::fs::read_dir(workspace_root.path()).unwrap().count(), 0);
}

async fn declare(boot: &Boot, key: &str) -> Task {
    let mut task = plan_task(&boot.track_id, key, TaskKind::Codex, &[]);
    task.spawn = "sub-track".into();
    seed_projected_task(boot, task.clone()).await;
    boot.repo.task_get(&task.id).await.unwrap().unwrap()
}

async fn assert_refused(
    boot: &Boot,
    task: &Task,
    runtime: &OperationRuntime,
    scheduler: &Arc<Scheduler>,
) {
    let failed = boot.repo.task_get(&task.id).await.unwrap().unwrap();
    assert_eq!(failed.status, TaskStatus::Failed);
    assert!(failed.finished_at_ms.is_some());
    assert_eq!(
        status_detail_class(failed.status_detail.as_deref().unwrap()),
        "child-track-create-failed"
    );
    let db = boot.repo.sqlite_db().unwrap();
    let (op, phase, error, child): (String, String, String, Option<String>) = sqlx::query_as(
        "SELECT o.id,o.phase,o.last_error,t.child_track_id FROM operations o \
         JOIN tasks t ON t.id=o.idempotency_key WHERE o.kind='child-track' AND t.id=?1",
    )
    .bind(&task.id)
    .fetch_one(db.read_pool())
    .await
    .unwrap();
    assert_eq!((phase, error), ("failed".into(), "not_admin".into()));
    assert_eq!(child, None);
    let payload = build_child_track_payload(task).unwrap();
    let replay = runtime
        .submit(
            "child-track",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(task.id.clone()),
                payload_hash: stable_payload_hash(&payload).unwrap(),
            },
            payload,
        )
        .await
        .unwrap();
    assert_eq!(replay, op);
    assert!(matches!(runtime.wait(&replay).await.unwrap().outcome,
        OperationOutcome::Failed { last_error, last_error_class: Some(class), .. }
        if last_error == "not_admin" && class == "forbidden"));
    for _ in 0..2 {
        runtime.drive().await.unwrap();
        scheduler.sweep_all().await;
    }
    assert_eq!(
        boot.repo.task_get(&task.id).await.unwrap().unwrap().status,
        TaskStatus::Failed
    );
    let operations: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operations WHERE kind='child-track' AND idempotency_key=?1",
    )
    .bind(&task.id)
    .fetch_one(db.read_pool())
    .await
    .unwrap();
    assert_eq!(operations, 1);
    let notices = event_rows(boot, "task.failed").await;
    assert_eq!(
        notices
            .iter()
            .filter(|(_, event)| event["idempotency_key"] == task.id)
            .count(),
        1
    );
}

#[tokio::test]
async fn subtrack_admin_queued_declaration_loses_authority_before_dispatch() {
    let (boot, repository) = child_track_config::boot_git_parent().await;
    crate::support::child_track::set_admin(boot.repo.as_ref(), boot.track_id.as_str(), true).await;
    let task = declare(&boot, "revoked-queued").await;
    assert_eq!(task.status, TaskStatus::Pending);
    crate::support::child_track::set_admin(boot.repo.as_ref(), boot.track_id.as_str(), false).await;
    let before = inventory(&boot).await;
    let before_worktrees = worktrees(repository.path());
    let workspace = tempfile::tempdir().unwrap();
    let (runtime, scheduler) =
        build_scheduler(&boot, vec![adapter(&boot, workspace.path().into())]);
    scheduler.schedule_track(boot.track_id.clone()).await;
    assert_refused(&boot, &task, &runtime, &scheduler).await;
    assert_eq!(inventory(&boot).await, before);
    assert_eq!(worktrees(repository.path()), before_worktrees);
    assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 0);
}

/// Change only the live parent after the real adapter has captured FrozenParent.
/// The returned PreparedTx is untouched and executes through the real operation repository.
struct ChangeAfterSnapshot {
    inner: Arc<dyn ProviderAdapter>,
    close: bool,
    snapshots: Arc<AtomicUsize>,
}

#[async_trait]
impl ProviderAdapter for ChangeAfterSnapshot {
    fn kind(&self) -> &'static str {
        self.inner.kind()
    }
    fn phases(&self) -> &'static [PhaseTag] {
        self.inner.phases()
    }
    async fn validate(&self, input: &Value) -> CalmResult<()> {
        self.inner.validate(input).await
    }
    async fn preparation_track(&self, input: &Value, ctx: &SpawnCtx) -> CalmResult<Option<String>> {
        self.inner.preparation_track(input, ctx).await
    }
    async fn prepare_external(
        &self,
        input: &Value,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> CalmResult<Option<Box<dyn calm_server::operation::PreparedTx>>> {
        let prepared = self.inner.prepare_external(input, op, ctx).await?;
        assert!(prepared.is_some(), "real parent snapshot was captured");
        let parent = input["parent_track_id"].as_str().unwrap();
        assert!(
            ctx.repo
                .track_get(parent)
                .await?
                .unwrap()
                .admin_granted_at
                .is_some()
        );
        if self.close {
            let parent = parent.to_owned();
            calm_server::db::write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
                Box::pin(async move {
                    calm_truth::db::sqlite::track_update_tx(
                        tx,
                        &parent,
                        TrackPatch {
                            closed: Some(true),
                            ..Default::default()
                        },
                    )
                    .await?;
                    Ok(())
                })
            })
            .await?;
        } else {
            crate::support::child_track::set_admin(ctx.repo.as_ref(), parent, false).await;
        }
        self.snapshots.fetch_add(1, Ordering::SeqCst);
        Ok(prepared)
    }
    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        op: &Operation,
    ) -> CalmResult<TxOutput> {
        self.inner.prepare_tx(tx, input, op).await
    }
    fn app_server_interact_kind(
        &self,
        output: &TxOutput,
        op: &Operation,
    ) -> CalmResult<calm_server::operation::AppServerInteractKind> {
        self.inner.app_server_interact_kind(output, op)
    }
    async fn app_server_interact(
        &self,
        output: &mut TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> CalmResult<AppServerInteractOutcome> {
        self.inner.app_server_interact(output, op, ctx).await
    }
    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> CalmResult<SpawnOutcome> {
        self.inner.spawn_side_effect(output, op, ctx).await
    }
    async fn plan_compensation(
        &self,
        phase: PhaseTag,
        reason: &str,
        output: &TxOutput,
        op: &Operation,
    ) -> CalmResult<CompensationStateVersioned> {
        self.inner
            .plan_compensation(phase, reason, output, op)
            .await
    }
    async fn compensate_step(
        &self,
        step: &calm_server::operation::CompensationStep,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> CalmResult<()> {
        self.inner.compensate_step(step, output, op, ctx).await
    }
}

async fn snapshot_change_refuses(close: bool) {
    let (boot, repository) = child_track_config::boot_git_parent().await;
    crate::support::child_track::set_admin(boot.repo.as_ref(), boot.track_id.as_str(), true).await;
    let task = declare(&boot, "snapshot-race").await;
    let before = inventory(&boot).await;
    let before_worktrees = worktrees(repository.path());
    let workspace = tempfile::tempdir().unwrap();
    let snapshots = Arc::new(AtomicUsize::new(0));
    let (runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(ChangeAfterSnapshot {
            inner: adapter(&boot, workspace.path().into()),
            close,
            snapshots: snapshots.clone(),
        })],
    );
    scheduler.schedule_track(boot.track_id.clone()).await;
    assert_refused(&boot, &task, &runtime, &scheduler).await;
    assert_eq!(
        snapshots.load(Ordering::SeqCst),
        1,
        "refusal cannot repeat preparation"
    );
    assert_eq!(inventory(&boot).await, before);
    assert_eq!(worktrees(repository.path()), before_worktrees);
    assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn subtrack_admin_revoked_after_frozen_snapshot_is_refused_in_create_transaction() {
    snapshot_change_refuses(false).await;
}

#[tokio::test]
async fn subtrack_admin_closed_after_frozen_snapshot_is_refused_in_create_transaction() {
    snapshot_change_refuses(true).await;
}

#[tokio::test]
async fn subtrack_admin_uncreated_recovery_and_same_key_retry_cannot_reuse_grant() {
    let (boot, repository) = child_track_config::boot_git_parent().await;
    crate::support::child_track::set_admin(boot.repo.as_ref(), boot.track_id.as_str(), true).await;
    let task = declare(&boot, "recover-before-create").await;
    // The production scheduler claims the Report task, then loses its runtime before op insertion.
    let (runtime, scheduler) = build_scheduler(&boot, vec![]);
    drop(runtime);
    scheduler.schedule_track(boot.track_id.clone()).await;
    assert_eq!(
        boot.repo.task_get(&task.id).await.unwrap().unwrap().status,
        TaskStatus::Dispatched
    );
    // Persist the submitted Pending operation as at a crash before its creation transaction.
    let payload = build_child_track_payload(&task).unwrap();
    let operations = SqlxOperationRepo::new(boot.repo.sqlite_db().unwrap());
    let op = operations
        .insert_operation(
            "child-track",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(task.id.clone()),
                payload_hash: stable_payload_hash(&payload).unwrap(),
            },
            payload,
        )
        .await
        .unwrap();
    crate::support::child_track::set_admin(boot.repo.as_ref(), boot.track_id.as_str(), false).await;
    let before = inventory(&boot).await;
    let before_worktrees = worktrees(repository.path());
    let workspace = tempfile::tempdir().unwrap();
    let (runtime, scheduler) =
        build_scheduler(&boot, vec![adapter(&boot, workspace.path().into())]);
    let plan = runtime.recover_on_boot().await.unwrap();
    runtime.apply_recovery(plan).await.unwrap();
    scheduler.sweep_all().await;
    assert_refused(&boot, &task, &runtime, &scheduler).await;
    // A later grant also cannot revive the failed operation on a same-key replay.
    crate::support::child_track::set_admin(boot.repo.as_ref(), boot.track_id.as_str(), true).await;
    assert_refused(&boot, &task, &runtime, &scheduler).await;
    assert!(matches!(
        runtime.wait(&op).await.unwrap().outcome,
        OperationOutcome::Failed { .. }
    ));
    assert_eq!(inventory(&boot).await, before);
    assert_eq!(worktrees(repository.path()), before_worktrees);
    assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn subtrack_admin_authorized_creation_does_not_inherit_admin_and_refuses_nested_declaration()
{
    let (boot, repository) = child_track_config::boot_git_parent().await;
    crate::support::child_track::set_admin(boot.repo.as_ref(), boot.track_id.as_str(), true).await;
    let first = declare(&boot, "authorized").await;
    let workspace = tempfile::tempdir().unwrap();
    let (runtime, scheduler) =
        build_scheduler(&boot, vec![adapter(&boot, workspace.path().into())]);
    let before = inventory(&boot).await;
    scheduler.schedule_track(boot.track_id.clone()).await;
    let payload = build_child_track_payload(&first).unwrap();
    let op = runtime
        .submit(
            "child-track",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(first.id.clone()),
                payload_hash: stable_payload_hash(&payload).unwrap(),
            },
            payload,
        )
        .await
        .unwrap();
    let OperationOutcome::Succeeded { result } = runtime.wait(&op).await.unwrap().outcome else {
        panic!("authorized child creation must succeed")
    };
    let child = result["child_track_id"].as_str().unwrap();
    let row = boot.repo.track_get(child).await.unwrap().unwrap();
    assert_eq!(row.area_id, boot.area_id);
    let parent: String = sqlx::query_scalar("SELECT parent_track_id FROM tracks WHERE id=?1")
        .bind(child)
        .fetch_one(boot.repo.sqlite_db().unwrap().read_pool())
        .await
        .unwrap();
    assert_eq!(parent, boot.track_id.as_str());
    assert_eq!(row.admin_granted_at, None, "Admin must never be inherited");
    assert!(std::path::Path::new(row.workspace.agent_cwd()).is_dir());
    assert_eq!(inventory(&boot).await, (before.0 + 1, before.1 + 2));
    let nested = plan_task(&row.id, "nested", TaskKind::Codex, &[]);
    let mut nested = Task {
        spawn: "sub-track".into(),
        ..nested
    };
    let identity = ToolCallIdentity {
        card_id: result["planner_card_id"].as_str().unwrap().into(),
        session_id: result["canonical_worker"].as_str().unwrap().into(),
        track_id: Some(child.into()),
        ..planner_identity(&boot)
    };
    seed_projected_task_for(&boot, child, identity, nested.clone()).await;
    nested = boot.repo.task_get(&nested.id).await.unwrap().unwrap();
    let before = inventory(&boot).await;
    let before_worktrees = worktrees(repository.path());
    scheduler.schedule_track(child.into()).await;
    assert_refused(&boot, &nested, &runtime, &scheduler).await;
    assert_eq!(inventory(&boot).await, before);
    assert_eq!(worktrees(repository.path()), before_worktrees);
    assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 0);
    assert!(boot.repo.track_get(child).await.unwrap().unwrap().is_open());
}
