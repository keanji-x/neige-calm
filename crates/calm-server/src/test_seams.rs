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

/// A one-shot pause a race test arms on a production path: the path calls [`pause_point`] with a
/// named point and the key it is working on, signals `entered`, and waits for `release`.
#[cfg(feature = "fixtures")]
#[derive(Clone)]
pub struct PausePoint {
    pub entered: std::sync::Arc<tokio::sync::Notify>,
    pub release: std::sync::Arc<tokio::sync::Notify>,
}

#[cfg(feature = "fixtures")]
type PauseRegistry = std::sync::Mutex<std::collections::HashMap<(String, String), PausePoint>>;

#[cfg(feature = "fixtures")]
fn pause_points() -> &'static PauseRegistry {
    static POINTS: std::sync::OnceLock<PauseRegistry> = std::sync::OnceLock::new();
    POINTS.get_or_init(Default::default)
}

/// Arm `hook` for the next request that reaches `point` working on `key`; the first one consumes it.
#[cfg(feature = "fixtures")]
pub fn install_pause_for_test(point: &str, key: &str, hook: PausePoint) {
    pause_points()
        .lock()
        .expect("pause point mutex")
        .insert((point.to_owned(), key.to_owned()), hook);
}

/// Pause here when a test armed `point` for `key`. Call sites MUST be gated with
/// `#[cfg(feature = "fixtures")]` as a whole statement.
#[cfg(feature = "fixtures")]
pub async fn pause_point(point: &str, key: &str) {
    let hook = pause_points()
        .lock()
        .expect("pause point mutex")
        .remove(&(point.to_owned(), key.to_owned()));
    if let Some(hook) = hook {
        hook.entered.notify_one();
        hook.release.notified().await;
    }
}

/// Where a planner send has found no binding for its key (#2043, the UNIQUE backstop); keyed by card id.
#[cfg(feature = "fixtures")]
pub const PLANNER_INPUT_REPLAY_MISSED: &str = "planner-input-replay-missed";

/// Where an operation insert has found no row under its `(kind, idempotency_key)` and is about to
/// write one; keyed by the idempotency key.
#[cfg(feature = "fixtures")]
pub const OPERATION_DEDUP_MISSED: &str = "operation-dedup-missed";

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

/// Give an attached track its #1830 track worktree the way the create route does: the path
/// `track_worktree_path_for(checkout, id)` on the row (the create transaction's write), then the
/// production `ensure_track_worktree` (the route's post-commit step: fetch the upstream, start
/// `neige/track-<id>` where the checkout's HEAD / upstream says, exclude `.claude/worktrees/`).
/// Returns the worktree path.
#[cfg(any(test, feature = "fixtures"))]
pub async fn attach_track_worktree_for_test(
    pool: &sqlx::SqlitePool,
    track_id: &str,
    checkout: &std::path::Path,
) -> crate::error::Result<std::path::PathBuf> {
    let path = crate::db::sqlite::track_worktree_path_for(checkout, track_id);
    let path_str = path.to_str().ok_or_else(|| {
        crate::error::CalmError::Internal(format!("{} is not UTF-8", path.display()))
    })?;
    let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
    sqlx::query("UPDATE tracks SET workspace_worktree_path = ?1 WHERE id = ?2")
        .bind(path_str)
        .bind(track_id)
        .execute(&mut *tx)
        .await?;
    let track =
        crate::db::sqlite::track_get_tx(&mut tx, &crate::ids::TrackId::from(track_id.to_string()))
            .await?;
    tx.commit().await?;
    crate::operation::workspace_lease::track_worktree::ensure_track_worktree(&track).await?;
    Ok(path)
}

/// Reach the production workspace-lease acquisition from an integration test.
#[cfg(feature = "fixtures")]
pub async fn acquire_workspace_lease_for_test(
    pool: &sqlx::SqlitePool,
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
    pool: &sqlx::SqlitePool,
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
    crate::workspace_materialize::neige_git_command()
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
    pool: &sqlx::SqlitePool,
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
