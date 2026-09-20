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

/// Reach the production worker-lease preparation from an integration test.
#[cfg(feature = "fixtures")]
pub async fn prepare_workspace_lease_target_for_test(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    track_id: &str,
    card_id: &str,
    workspace_root: &std::path::Path,
) -> crate::error::Result<std::path::PathBuf> {
    crate::operation::workspace_lease::prepare_workspace_lease_target_tx(
        tx,
        track_id,
        card_id,
        workspace_root,
    )
    .await
    .map(|target| target.repo_root)
}

/// Take a whole first-worker workspace lease: prepare → commit → provision, in that order.
/// Returns the provisioned worktree path.
#[cfg(feature = "fixtures")]
pub async fn provision_workspace_lease_for_test(
    pool: &sqlx::SqlitePool,
    track_id: &str,
    card_id: &str,
    workspace_root: &std::path::Path,
) -> crate::error::Result<std::path::PathBuf> {
    let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
    let target = crate::operation::workspace_lease::prepare_workspace_lease_target_tx(
        &mut tx,
        track_id,
        card_id,
        workspace_root,
    )
    .await?;
    tx.commit().await?;
    crate::operation::workspace_lease::provision_workspace_worktree(&target)?;
    Ok(target.path)
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

/// Release a card's active workspace lease through the production "flip the row only" path
/// (checkout left on disk).
#[cfg(feature = "fixtures")]
pub async fn release_workspace_lease_for_card_for_test(
    repo: &dyn crate::db::RepoEventWrite,
    events: &crate::event::EventBus,
    card_id: &str,
) -> crate::error::Result<bool> {
    crate::operation::workspace_lease::release_workspace_lease_for_card_repo(repo, events, card_id)
        .await
}

/// Release a lease through the production "remove the worktree" path; on a non-git lease root
/// this removes the directory.
#[cfg(feature = "fixtures")]
pub async fn release_workspace_lease_by_id_for_test(
    pool: &sqlx::SqlitePool,
    events: &crate::event::EventBus,
    lease_id: &str,
) -> crate::error::Result<bool> {
    crate::operation::workspace_lease::release_workspace_lease_by_id(pool, events, lease_id).await
}

/// Build git commands in a test exactly the way the server does: a bare `git` is redirected by
/// `GIT_DIR`, `GIT_WORK_TREE`, `GIT_CEILING_DIRECTORIES` and `GIT_CONFIG_*` in hook and CI
/// environments, so a probe that does not scrub them can disagree with the server.
#[cfg(feature = "fixtures")]
pub fn neige_git_command_for_test() -> std::process::Command {
    crate::workspace_materialize::neige_git_command()
}
