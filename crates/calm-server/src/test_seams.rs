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

/// Where a message-less track create has committed its cards and not yet submitted its
/// Planner's start (#2184); keyed by the track's area id, which the test knows in advance.
#[cfg(feature = "fixtures")]
pub const TRACK_CREATE_BEFORE_PLANNER_START: &str = "track-create-before-planner-start";

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
    pool: &sqlx::SqlitePool,
) -> crate::error::Result<usize> {
    crate::operation::workspace_lease::track_worktree_clean::clean_idle_closed_track_worktrees(
        pool,
        &mut std::collections::HashSet::new(),
    )
    .await
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
    attempt_id: &str,
    path: &std::path::Path,
) -> crate::error::Result<()> {
    let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
    crate::operation::workspace_lease::acquire_plain_workspace_lease_tx(
        &mut tx,
        card_id,
        track_id,
        lease_owner,
        attempt_id,
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
    attempt_id: &str,
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
    acquire_workspace_lease_tx(&mut tx, card_id, track_id, lease_owner, attempt_id, &plan).await?;
    tx.commit().await?;
    Ok(())
}

/// Stand where a worker op's `prepare_tx` would for a fixture worker card (#2493): the
/// `dispatched` attempt is bound to the card's active worker session through the production
/// `bind_attempt_tx`. Returns the session id.
#[cfg(feature = "fixtures")]
pub async fn bind_worker_for_test(
    pool: &sqlx::SqlitePool,
    attempt_id: &str,
    card_id: &str,
) -> crate::error::Result<String> {
    let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
    let session_id = bind_fixture_worker_tx(&mut tx, attempt_id, card_id).await?;
    tx.commit().await?;
    Ok(session_id)
}

/// A worker card that runs a task (#2493): a `codex` attempt `<track>:<key>` is inserted and
/// bound, running, to the card's worker session ([`bind_running_worker_for_test`]). Returns the
/// attempt id.
#[cfg(feature = "fixtures")]
pub async fn running_worker_attempt_for_test(
    pool: &sqlx::SqlitePool,
    track_id: &str,
    card_id: &str,
    key: &str,
) -> crate::error::Result<String> {
    let attempt_id = format!("{track_id}:{key}");
    let now = crate::model::now_ms();
    sqlx::query(
        "INSERT INTO tasks (id, track_id, key, kind, goal, context_json, depends_on_json, status, \
         declared_by, created_at_ms, updated_at_ms) \
         VALUES (?1, ?2, ?3, 'codex', 'fixture worker', 'null', '[]', 'dispatched', 'user', ?4, ?4)",
    )
    .bind(&attempt_id)
    .bind(track_id)
    .bind(key)
    .bind(now)
    .execute(pool)
    .await?;
    bind_running_worker_for_test(pool, &attempt_id, card_id).await?;
    Ok(attempt_id)
}

/// [`bind_worker_for_test`] for a `pending` or `dispatched` attempt, then the running stamp a
/// spawn's success makes, in one transaction.
#[cfg(feature = "fixtures")]
pub async fn bind_running_worker_for_test(
    pool: &sqlx::SqlitePool,
    attempt_id: &str,
    card_id: &str,
) -> crate::error::Result<String> {
    let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
    sqlx::query("UPDATE tasks SET status = 'dispatched' WHERE id = ?1 AND status = 'pending'")
        .bind(attempt_id)
        .execute(&mut *tx)
        .await?;
    let session_id = bind_fixture_worker_tx(&mut tx, attempt_id, card_id).await?;
    sqlx::query(
        "UPDATE tasks SET status = 'running', updated_at_ms = ?2 \
         WHERE id = ?1 AND status = 'dispatched'",
    )
    .bind(attempt_id)
    .bind(crate::model::now_ms())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(session_id)
}

/// For a fixture that seeds a `tasks` row with `worker_card_id` set, as the binding writer leaves
/// it (#2493): [`bind_task_to_card_for_test`] with that card. A row with no `worker_card_id`, one
/// naming a card the fixture never made, or one already bound, is left alone.
#[cfg(feature = "fixtures")]
pub async fn bind_seeded_task_for_test(
    pool: &sqlx::SqlitePool,
    attempt_id: &str,
) -> crate::error::Result<()> {
    let row: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT worker_card_id, worker_session_id FROM tasks WHERE id = ?1")
            .bind(attempt_id)
            .fetch_optional(pool)
            .await?;
    if let Some((Some(card_id), None)) = row {
        let card_exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cards WHERE id = ?1)")
                .bind(&card_id)
                .fetch_one(pool)
                .await?;
        if card_exists {
            bind_task_to_card_for_test(pool, attempt_id, &card_id).await?;
        }
    }
    Ok(())
}

/// A fixture's "this card is the attempt's worker" (#2493): the attempt is bound, through
/// [`bind_fixture_worker_tx`], to the worker session it runs in on the card; an attempt already
/// bound to this card is left as it is. Whatever status the row has, it is bound
/// while `dispatched` and given its status back. Returns the session id.
#[cfg(feature = "fixtures")]
pub async fn bind_task_to_card_for_test(
    pool: &sqlx::SqlitePool,
    attempt_id: &str,
    card_id: &str,
) -> crate::error::Result<String> {
    let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
    if let Some(bound) = crate::db::sqlite::attempt_binding_tx(&mut tx, attempt_id).await? {
        tx.rollback().await?;
        return if bound.card_id.as_deref() == Some(card_id) {
            Ok(bound.session_id)
        } else {
            Err(crate::error::CalmError::Conflict(format!(
                "fixture attempt {attempt_id} is already bound to another card"
            )))
        };
    }
    let status: String = sqlx::query_scalar("SELECT status FROM tasks WHERE id = ?1")
        .bind(attempt_id)
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query("UPDATE tasks SET status = 'dispatched' WHERE id = ?1")
        .bind(attempt_id)
        .execute(&mut *tx)
        .await?;
    let session_id = bind_fixture_worker_tx(&mut tx, attempt_id, card_id).await?;
    sqlx::query("UPDATE tasks SET status = ?1 WHERE id = ?2")
        .bind(&status)
        .bind(attempt_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(session_id)
}

/// The bind a stub worker adapter's `prepare_tx` makes, as the real ones do (#2493): `attempt_id`
/// (dispatched) to `card_id`'s active worker session, in the caller's transaction. Returns the
/// session id.
#[cfg(feature = "fixtures")]
pub async fn bind_fixture_worker_tx(
    tx: &mut crate::operation::Tx<'_>,
    attempt_id: &str,
    card_id: &str,
) -> crate::error::Result<String> {
    let session_id = fixture_worker_session_tx(tx, attempt_id, card_id).await?;
    crate::db::sqlite::bind_attempt_tx(tx, attempt_id, &session_id, card_id).await?;
    Ok(session_id)
}

/// The worker session a fixture attempt runs in on `card_id`, through the production session
/// writers: the card's active session while it serves no attempt; otherwise a new running one,
/// as a new spawn mints one (superseding an active session that already serves an attempt).
#[cfg(feature = "fixtures")]
async fn fixture_worker_session_tx(
    tx: &mut crate::operation::Tx<'_>,
    attempt_id: &str,
    card_id: &str,
) -> crate::error::Result<String> {
    use crate::session_projection_repo::{
        AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    let active: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT session_id, attempt_id FROM worker_session_binding \
         WHERE card_id = ?1 AND session_active",
    )
    .bind(card_id)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some((session_id, None)) = &active {
        return Ok(session_id.clone());
    }
    let card_kind: Option<String> = sqlx::query_scalar("SELECT kind FROM cards WHERE id = ?1")
        .bind(card_id)
        .fetch_optional(&mut **tx)
        .await?;
    let (kind, agent_provider) = match card_kind.as_deref() {
        Some("claude") => (WorkerSessionKind::ClaudeCard, Some(AgentProvider::Claude)),
        Some("terminal") => (WorkerSessionKind::Terminal, None),
        _ => (WorkerSessionKind::CodexCard, Some(AgentProvider::Codex)),
    };
    let terminal_run_id: Option<String> =
        sqlx::query_scalar("SELECT id FROM terminals WHERE card_id = ?1")
            .bind(card_id)
            .fetch_optional(&mut **tx)
            .await?;
    let init = WorkerSessionInit {
        id: format!("{attempt_id}-worker-session"),
        card_id: card_id.to_string(),
        kind,
        agent_provider,
        status: WorkerSessionState::Running,
        terminal_run_id,
        thread_id: None,
        session_id: None,
        active_turn_id: None,
        handle_state_json: None,
        spawn_op_id: None,
        now_ms: crate::model::now_ms(),
    };
    let started = match active {
        Some((old, Some(_))) => {
            crate::db::sqlite::session_supersede_and_start_tx(tx, &old, init).await?
        }
        _ => crate::db::sqlite::session_start_runtime_tx(tx, init).await?,
    };
    Ok(started.id)
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
    attempt_id: &str,
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
        acquire_workspace_lease_tx(&mut tx, card_id, track_id, "op-test", attempt_id, &plan)
            .await?;
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
    pool: &sqlx::SqlitePool,
    attempt_id: &str,
    card_id: &str,
    session_id: &str,
    track_id: &str,
) -> crate::error::Result<String> {
    let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
    let admitted = crate::operation::task_gate_run::admit_run_tx(
        &mut tx, attempt_id, card_id, session_id, track_id, None,
    )
    .await?;
    tx.commit().await?;
    match admitted {
        crate::operation::task_gate_run::Admitted::New { key, .. }
        | crate::operation::task_gate_run::Admitted::Joined { key, .. } => Ok(key),
    }
}
