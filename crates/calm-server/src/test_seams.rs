//! `fixtures`-only seams for integration tests: deterministic crash injection and reach-through
//! to `pub(crate)` production paths. The production binary compiles none of this.

/// Crash the process here iff `CALM_TEST_CRASH_AT` equals `point` exactly. Aborts rather than
/// panics so no destructor runs (SIGKILL durability semantics). Call sites MUST be gated with
/// `#[cfg(feature = "fixtures")]` as a whole statement.
#[cfg(feature = "fixtures")]
pub fn crash_point(point: &str) {
    if std::env::var("CALM_TEST_CRASH_AT").is_ok_and(|v| v == point) {
        eprintln!("CALM_TEST_CRASH_AT={point}: aborting for crash-recovery test");
        std::process::abort();
    }
}

/// The one pause registry lives at the lowest layer that pauses (calm-truth); server points arm it too.
#[cfg(feature = "fixtures")]
pub use calm_truth::test_seam::{
    PausePoint, blocking_pause_point, install_pause_for_test, pause_point,
};

/// Worker-flow lifecycle points, keyed like calm-truth's capture persistence points by card and
/// source record index (`-1` for points before any record). Arm with
/// `calm_truth::capture_test_seam::install`.
#[cfg(feature = "fixtures")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerFlowPoint {
    /// A record left the checkpoint unchanged and wrote nothing.
    Idle,
    /// An attachment has read its durable checkpoint.
    CheckpointLoaded,
    /// An attach has settled any previous source of the card and is about to start the new one.
    ReplacementReady,
    /// Cancellation arrived while a capture was in flight; that capture is being settled.
    CancellationSettling,
}

#[cfg(feature = "fixtures")]
impl calm_truth::capture_test_seam::CaptureSeamPoint for WorkerFlowPoint {
    fn name(self) -> &'static str {
        match self {
            Self::Idle => "worker-flow-idle",
            Self::CheckpointLoaded => "worker-flow-checkpoint-loaded",
            Self::ReplacementReady => "worker-flow-replacement-ready",
            Self::CancellationSettling => "worker-flow-cancellation-settling",
        }
    }
}

/// Where a planner send has found no binding for its key (#2043, the UNIQUE backstop); keyed by card id.
#[cfg(feature = "fixtures")]
pub const PLANNER_INPUT_REPLAY_MISSED: &str = "planner-input-replay-missed";

/// Where a person's send has found, under the per-card recovery lock, a card with no thread to
/// preserve and is about to start it (#2184); keyed by card id.
#[cfg(feature = "fixtures")]
pub const PLANNER_FIRST_START: &str = "planner-first-start";

/// Where harness recovery has replayed its candidate and is about to claim the
/// track lifecycle fence; keyed by card id.
#[cfg(feature = "fixtures")]
pub const PLANNER_RECOVERY_INSTALL: &str = "planner-recovery-install";

/// Failed recovery has captured its original target and quiesced its harness,
/// but has not refetched the settled durable carrier; keyed by session id.
#[cfg(feature = "fixtures")]
pub const PLANNER_RECOVERY_REFETCH: &str = "planner-recovery-refetch";

/// Where a message-less track create has committed its cards and not yet submitted its
/// Planner's start (#2184); keyed by the track's area id, which the test knows in advance.
#[cfg(feature = "fixtures")]
pub const TRACK_CREATE_BEFORE_PLANNER_START: &str = "track-create-before-planner-start";

/// Where a viewer's lazy reattach has seen a live child and not yet attached to it (#2516);
/// keyed by terminal id.
#[cfg(feature = "fixtures")]
pub const VIEWER_REATTACH_PROBED: &str = "viewer-reattach-probed";

/// Where an operation insert has found no row under its `(kind, idempotency_key)` and is about to
/// write one; keyed by the idempotency key.
#[cfg(feature = "fixtures")]
pub const OPERATION_DEDUP_MISSED: &str = "operation-dedup-missed";

/// Where a keyed commit (`OperationRuntime::commit_keyed`) is about to begin the transaction that
/// checks its key, before any check; keyed by the idempotency key.
#[cfg(feature = "fixtures")]
pub const OPERATION_KEYED_COMMIT_BEGIN: &str = "operation-keyed-commit-begin";

/// Reach the production worker-lease preparation (#1830 S2: the track's checkout, materialized
/// for a managed track, checked clean) from an integration test. Returns the worker's directory.
#[cfg(feature = "fixtures")]
pub async fn prepare_worker_lease_for_test(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    track_id: &str,
    workspace_root: &std::path::Path,
) -> crate::error::Result<std::path::PathBuf> {
    crate::operation::workspace_lease::prepare_worker_lease_as_tx(
        tx,
        track_id,
        crate::model::TaskAccess::ReadWrite,
        workspace_root,
    )
    .await
    .map(|plan| plan.path)
}

/// One pass of the #2356 closed-track worktree clean with nothing cleaned before it. Returns how
/// many worktrees it cleaned.
#[cfg(any(test, feature = "fixtures"))]
pub async fn clean_idle_closed_track_worktrees_for_test(
    db: &calm_truth::db::sqlite::SqliteDb,
    runtime: &std::sync::Arc<crate::operation::OperationRuntime>,
) -> crate::error::Result<usize> {
    clean_closed_worktrees_with_history_for_test(db, runtime, &mut std::collections::HashSet::new())
        .await
}

#[cfg(any(test, feature = "fixtures"))]
pub async fn clean_closed_worktrees_with_history_for_test(
    db: &calm_truth::db::sqlite::SqliteDb,
    runtime: &std::sync::Arc<crate::operation::OperationRuntime>,
    cleaned: &mut std::collections::HashSet<(String, i64)>,
) -> crate::error::Result<usize> {
    workspace_runtime::worktree_clean::clean_idle_closed_track_worktrees(
        &crate::workspace_service::ports::ServerClosedWorktreeFacts {
            receipts: crate::workspace_service::reclaim::ServerReclaimPorts {
                runtime: runtime.clone(),
                db: db.clone(),
            },
            runtime: runtime.clone(),
        },
        cleaned,
        crate::workspace_service::ports::observe,
    )
    .await
}

/// Create a track with its initial workspace decision through the production transaction.
/// Callers choose the plan before the freeze latch closes; no existing track is re-pointed.
/// Track worktrees follow the route's post-commit ensure and kernel registration paths.
#[cfg(any(test, feature = "fixtures"))]
pub async fn create_track_with_workspace_for_test(
    pool: &crate::db::sqlite::SqliteDb,
    params: crate::model::NewTrack,
    plan: &crate::db::sqlite::TrackWorkspacePlan,
    cache: &crate::track_area_cache::TrackAreaCache,
) -> crate::error::Result<crate::model::Track> {
    let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
    let track =
        crate::db::sqlite::track_create_tx(&mut tx, params, None, plan, None, cache).await?;
    tx.commit().await?;
    if track.workspace.worktree.is_some() {
        ensure_track_worktree_for_test(pool, track.id.as_str()).await?;
    }
    Ok(track)
}

/// Finish an already-declared track worktree without changing its stored selection.
#[cfg(any(test, feature = "fixtures"))]
pub async fn ensure_track_worktree_for_test(
    pool: &crate::db::sqlite::SqliteDb,
    track_id: &str,
) -> crate::error::Result<std::path::PathBuf> {
    let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
    let track = calm_truth::db::sqlite::track_get_tx(
        &mut tx,
        &crate::ids::TrackId::from(track_id.to_string()),
    )
    .await?;
    tx.commit().await?;
    let path = track.workspace.worktree.as_deref().ok_or_else(|| {
        crate::error::CalmError::BadRequest("track creation did not declare a worktree".into())
    })?;
    workspace_runtime::track_worktree::ensure_track_worktree(
        &track,
        crate::workspace_service::ports::ServerWorkspacePorts,
        crate::workspace_service::ports::observe,
    )
    .await?;
    crate::workspace_service::identity::observe_for_task(pool, track_id).await?;
    Ok(path.into())
}

/// Reach the production workspace-lease acquisition from an integration test.
#[cfg(feature = "fixtures")]
pub async fn acquire_workspace_lease_for_test(
    pool: &crate::db::sqlite::SqliteDb,
    card_id: &str,
    track_id: &str,
    lease_owner: &str,
    path: &std::path::Path,
) -> crate::error::Result<()> {
    let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
    crate::operation::workspace_lease::acquire_plain_workspace_lease_tx(
        &mut tx,
        card_id,
        track_id,
        lease_owner,
        path,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Take a lease that RECORDS ITS BASE through the production `acquire_workspace_lease_tx` (the
/// five base columns in the one INSERT) at `path`, the sibling of
/// `acquire_workspace_lease_for_test` whose plain lease writes the legacy all-NULL tuple.
/// `plan.list.worktree.base_sha` is read from that
/// row. `canonical_path` is `path` as given and `git_common_dir` is `<path>/.git`: the directory
/// need not be a git repository, the columns only have to be the shape the CHECK accepts.
/// `fixtures`-only.
#[cfg(feature = "fixtures")]
pub async fn acquire_based_workspace_lease_for_test(
    pool: &crate::db::sqlite::SqliteDb,
    card_id: &str,
    track_id: &str,
    lease_owner: &str,
    path: &std::path::Path,
    base_sha: &str,
) -> crate::error::Result<()> {
    use crate::operation::workspace_lease::{
        LeaseBase, WorkerLeasePlan, acquire_workspace_lease_tx, base,
    };
    let plan = WorkerLeasePlan {
        path: path.to_path_buf(),
        branch: "main".into(),
        base: LeaseBase {
            base_sha: base_sha.to_string(),
            base_source: base::BaseSource::Commit,
            base_attempt_id: None,
            canonical_path: path.to_path_buf(),
            git_common_dir: path.join(".git"),
        },
        superseded: Vec::new(),
        reader: None,
        catch_up: None,
    };
    let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
    acquire_workspace_lease_tx(&mut tx, card_id, track_id, lease_owner, &plan).await?;
    tx.commit().await?;
    Ok(())
}

/// Release a card's active workspace lease through the production release (#1830 S2 D7), with
/// the attempt committed as its terminal `tasks.status` says.
#[cfg(feature = "fixtures")]
pub async fn release_workspace_lease_for_card_for_test(
    repo: &dyn crate::db::RepoEventWrite,
    events: &crate::event::EventBus,
    card_id: &str,
) -> crate::error::Result<bool> {
    crate::operation::workspace_lease::release_workspace_lease_for_card_repo(
        repo,
        events,
        card_id,
        crate::operation::workspace_lease::ReleaseDelivery::CommitAsTaskEnded,
    )
    .await
}

/// Build git commands in a test exactly the way the server does: a bare `git` is redirected by
/// `GIT_DIR`, `GIT_WORK_TREE`, `GIT_CEILING_DIRECTORIES` and `GIT_CONFIG_*` in hook and CI
/// environments, so a probe that does not scrub them can disagree with the server.
#[cfg(feature = "fixtures")]
pub fn neige_git_command_for_test() -> std::process::Command {
    workspace::command::neige_git_command()
}

/// What [`take_kernel_workspace_lease_for_test`] returns: the lease row's identity and the
/// base facts the delivery reads from it.
#[cfg(feature = "fixtures")]
#[derive(Clone, Debug)]
pub struct KernelWorkspaceLease {
    pub lease_id: String,
    /// The track's checkout (`agent_cwd()`), where the worker runs.
    pub path: std::path::PathBuf,
    pub branch: String,
    pub base_sha: String,
    pub git_common_dir: std::path::PathBuf,
}

/// The lease sequence the worker op's `prepare_tx` runs, in production order and through the
/// production functions (#1830 S2): prepare the track's checkout (materialize a managed one,
/// refuse a dirty tree), then INSERT the lease row (`delivery_policy = 'kernel'`, its HEAD as the
/// base) in one immediate transaction. The one seam an integration test needs to stand where a
/// Codex/Claude worker would.
#[cfg(feature = "fixtures")]
pub async fn take_kernel_workspace_lease_for_test(
    pool: &crate::db::sqlite::SqliteDb,
    track_id: &str,
    card_id: &str,
    workspace_root: &std::path::Path,
) -> crate::error::Result<KernelWorkspaceLease> {
    use crate::operation::workspace_lease::{
        acquire_workspace_lease_tx, prepare_worker_lease_as_tx,
    };
    let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
    let plan = prepare_worker_lease_as_tx(
        &mut tx,
        track_id,
        crate::model::TaskAccess::ReadWrite,
        workspace_root,
    )
    .await?;
    let (lease, _event) =
        acquire_workspace_lease_tx(&mut tx, card_id, track_id, "op-test", &plan).await?;
    tx.commit().await?;
    Ok(KernelWorkspaceLease {
        lease_id: lease.lease_id,
        path: plan.path,
        branch: plan.branch,
        base_sha: plan.base.base_sha,
        git_common_dir: plan.base.git_common_dir,
    })
}

/// Admit one gate run (#2464) through the production admission transaction, without driving it:
/// the out-of-process restart tests let the launched kernel's boot recovery drive the op. Returns
/// the run's key.
#[cfg(feature = "fixtures")]
pub async fn admit_gate_run_for_test(
    pool: &crate::db::sqlite::SqliteDb,
    attempt_id: &str,
    card_id: &str,
    track_id: &str,
    message: crate::git_candidate::commit_message::CommitMessage,
) -> crate::error::Result<String> {
    let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
    let admitted = crate::operation::task_gate_run::admit_run_tx(
        &mut tx,
        attempt_id,
        card_id,
        track_id,
        Some(message),
    )
    .await?;
    tx.commit().await?;
    match admitted {
        crate::operation::task_gate_run::Admitted::New { key, .. }
        | crate::operation::task_gate_run::Admitted::Joined { key, .. } => Ok(key),
    }
}

/// Where an idle Planner is about to persist its hibernation snapshot; keyed by card id.
#[cfg(feature = "fixtures")]
pub const PLANNER_HIBERNATION_QUIESCE: &str = "planner-hibernation-quiesce";

/// Where the loop decided a hibernation request, before replying; keyed by card id.
#[cfg(feature = "fixtures")]
pub const PLANNER_HIBERNATION_DECIDED: &str = "planner-hibernation-decided";

/// Fail the existing projection DELETE so all input-owner checkpoints exercise rollback.
#[cfg(feature = "fixtures")]
pub async fn reject_projection_retirement_for_test(db: &crate::db::sqlite::SqliteDb) {
    sqlx::query(
        "CREATE TRIGGER refuse_projection_cleanup BEFORE DELETE ON harness_items \
         BEGIN SELECT RAISE(ABORT, 'forced projection cleanup failure'); END",
    )
    .execute(db.writer_pool())
    .await
    .unwrap();
}

#[cfg(feature = "fixtures")]
pub use crate::workspace_service::sweep::{
    WorkspaceSweepHook, install_workspace_sweep_hook_for_test,
};

/// One-shot repository failures, keyed by method and operation; absent from production builds.
#[cfg(feature = "fixtures")]
mod operation_faults {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    use tokio::sync::Notify;
    type Faults = Mutex<HashMap<(String, String), Arc<Notify>>>;
    fn faults() -> &'static Faults {
        static FAULTS: OnceLock<Faults> = OnceLock::new();
        FAULTS.get_or_init(Default::default)
    }
    pub fn install_operation_fault_for_test(method: &str, id: &str) -> Arc<Notify> {
        let fired = Arc::new(Notify::new());
        faults()
            .lock()
            .unwrap()
            .insert((method.into(), id.into()), fired.clone());
        fired
    }
    pub(crate) fn operation_fault(method: &str, id: &str) -> crate::error::Result<()> {
        if let Some(fired) = faults().lock().unwrap().remove(&(method.into(), id.into())) {
            fired.notify_one();
            return Err(crate::error::CalmError::Internal(format!(
                "injected {method} failure"
            )));
        }
        Ok(())
    }
}
#[cfg(feature = "fixtures")]
pub use operation_faults::install_operation_fault_for_test;
#[cfg(feature = "fixtures")]
pub(crate) use operation_faults::operation_fault;

/// Per-engine one-shot StoreError injection; no executor errors are synthesized.
#[cfg(feature = "fixtures")]
#[derive(Debug, Default)]
pub struct ExecutionRecoveryFaults {
    armed: std::sync::Mutex<Option<&'static str>>,
    pub pending_calls: std::sync::atomic::AtomicUsize,
    pub failures: std::sync::atomic::AtomicUsize,
}
#[cfg(feature = "fixtures")]
impl ExecutionRecoveryFaults {
    pub fn arm(&self, point: &'static str) {
        assert!(self.armed.lock().unwrap().replace(point).is_none());
    }
    pub(crate) fn check(
        &self,
        point: &'static str,
    ) -> Result<(), workspace_runtime::execution::StoreError> {
        use std::sync::atomic::Ordering::SeqCst;
        if point == "pending" {
            self.pending_calls.fetch_add(1, SeqCst);
        }
        let mut armed = self.armed.lock().unwrap();
        if *armed == Some(point) {
            *armed = None;
            self.failures.fetch_add(1, SeqCst);
            return Err(workspace_runtime::execution::StoreError(format!(
                "one-shot {point}"
            )));
        }
        Ok(())
    }
}

/// Exercise the real builtin lowering, forge admission, executor and event policy in route fixtures.
#[cfg(feature = "fixtures")]
pub async fn gitforge_worktree_add_for_test(
    runtime: &std::sync::Arc<crate::operation::OperationRuntime>,
    logs: &std::path::Path,
    track: &str,
    card: &str,
    cwd: &std::path::Path,
    target: &std::path::Path,
    branch: &str,
) -> crate::error::Result<()> {
    let lowered = crate::builtin_plugins::dev::git_actions::lower(
        "git_worktree_add",
        &serde_json::json!({"target":target,"branch":branch}),
    )
    .map_err(crate::error::CalmError::Internal)?;
    let payload = serde_json::from_value(lowered)?;
    let submitted = crate::mcp_server::transport::submit_forge_action_with_key(
        runtime,
        logs,
        None,
        "gitforge",
        track.into(),
        card.into(),
        cwd.into(),
        payload,
        crate::model::new_id(),
    )
    .await
    .map_err(|e| crate::error::CalmError::Internal(format!("{e:?}")))?
    .map_err(crate::error::CalmError::Internal)?
    .submitted()
    .map_err(crate::error::CalmError::Internal)?;
    let result = runtime.wait(&submitted.op_id).await?;
    if !matches!(
        result.outcome,
        crate::operation::OperationOutcome::Succeeded { .. }
            | crate::operation::OperationOutcome::SucceededViaCollision { .. }
    ) {
        return Err(crate::error::CalmError::Internal(format!(
            "forge add failed: {result:?}"
        )));
    }
    Ok(())
}

#[cfg(feature = "fixtures")]
type WorkspaceObservers = std::sync::Mutex<
    std::collections::HashMap<(String, String), std::sync::Arc<std::sync::atomic::AtomicUsize>>,
>;
#[cfg(feature = "fixtures")]
fn workspace_observers() -> &'static WorkspaceObservers {
    static OBSERVERS: std::sync::OnceLock<WorkspaceObservers> = std::sync::OnceLock::new();
    OBSERVERS.get_or_init(Default::default)
}

/// Counter arms only a specified path; no global environment or production command replacement.
#[cfg(feature = "fixtures")]
pub fn count_workspace_observation_for_test(
    point: &str,
    path: &std::path::Path,
) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
    let counter = std::sync::Arc::default();
    workspace_observers().lock().unwrap().insert(
        (point.into(), path.to_str().unwrap().into()),
        std::sync::Arc::clone(&counter),
    );
    counter
}

#[cfg(feature = "fixtures")]
pub(crate) fn record_workspace_observation(point: &str, path: &std::path::Path) {
    if let Some(counter) = workspace_observers()
        .lock()
        .unwrap()
        .get(&(point.into(), path.to_str().unwrap().into()))
    {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Allocate a released daily fixture through the real Historical structure owner.
/// Fresh reconcile now owns a canonical carrier; REST owns ordinary creator start.
#[cfg(feature = "fixtures")]
pub async fn historical_daily_for_test(
    state: &crate::state::RouteState,
    now: chrono::DateTime<chrono::Utc>,
) -> crate::error::Result<crate::model::Track> {
    crate::daily_planner::historical_daily_for_test(state, now).await
}

/// Allocate or replay a released creator receipt through the Historical owner.
#[cfg(feature = "fixtures")]
pub async fn historical_creator_receipt_for_test(
    route: &crate::state::RouteState,
    max_open: u32,
    request: crate::mcp_server::tools::track_add::TrackAddRequest,
) -> crate::error::Result<crate::model::Track> {
    crate::routes::tracks::RouteTrackCreator::new(route.clone(), max_open)
        .historical_receipt_for_test(request)
        .await
}
