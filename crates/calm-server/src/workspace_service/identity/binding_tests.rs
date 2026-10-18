use std::{os::unix::fs::symlink, path::Path};

use crate::{
    db::sqlite::{SqlxRepo, TrackWorkspacePlan, track_workspace_write_tx},
    model::{Track, TrackWorkspace, TrackWorkspaceKind},
};

use super::tests::{init, track};

async fn binding(repo: &SqlxRepo, track: &Track) -> (String, String, i64) {
    sqlx::query_as(
        "SELECT b.workspace_id,r.observed_cwd,b.selection_version
         FROM track_workspace_bindings b
         JOIN workspace_resources r ON r.id=b.workspace_id
         WHERE b.track_id=?1",
    )
    .bind(track.id.as_str())
    .fetch_one(repo.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn workspace_identity_new_binding_registers_its_own_subdir() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("repo");
    init(&source);
    let selected = source.join("subdir");
    std::fs::create_dir(&selected).unwrap();
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let first = track(&repo, &selected, TrackWorkspacePlan::AttachedFromCwd).await;
    super::observe_for_task(repo.db(), first.id.as_str())
        .await
        .unwrap();
    let second = track(&repo, &selected, TrackWorkspacePlan::AttachedFromCwd).await;
    super::observe_for_task(repo.db(), second.id.as_str())
        .await
        .unwrap();
    let a = binding(&repo, &first).await;
    let b = binding(&repo, &second).await;
    assert_ne!(
        a.0, b.0,
        "each logical selection starts independently unobserved"
    );
    assert_eq!(a.1, "subdir");
    assert_eq!(b.1, "subdir", "each binding must observe its own cwd");
}

#[tokio::test]
async fn workspace_identity_registered_alias_retarget_requires_recovery() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("repo");
    init(&source);
    for subdir in ["a", "b"] {
        std::fs::create_dir(source.join(subdir)).unwrap();
    }
    let alias = tmp.path().join("alias");
    symlink(source.join("a"), &alias).unwrap();
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let current = track(&repo, &alias, TrackWorkspacePlan::AttachedFromCwd).await;
    super::observe_for_task(repo.db(), current.id.as_str())
        .await
        .unwrap();
    let before = binding(&repo, &current).await;
    std::fs::remove_file(&alias).unwrap();
    symlink(source.join("b"), &alias).unwrap();
    let result = super::observe_for_task(repo.db(), current.id.as_str()).await;
    assert!(
        result.is_err(),
        "same root with another cwd is a changed registered binding"
    );
    assert_eq!(binding(&repo, &current).await, before);
}

#[tokio::test]
async fn workspace_identity_repeated_selection_writer_preserves_registered_alias() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("repo");
    init(&source);
    std::fs::create_dir(source.join("subdir")).unwrap();
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let first = track(&repo, &source, TrackWorkspacePlan::AttachedFromCwd).await;
    super::observe_for_task(repo.db(), first.id.as_str())
        .await
        .unwrap();
    let current = track(
        &repo,
        Path::new(""),
        TrackWorkspacePlan::ManagedUnder(tmp.path().into()),
    )
    .await;
    let selected = TrackWorkspace {
        kind: TrackWorkspaceKind::Attached,
        path: source.join("subdir").to_str().unwrap().into(),
        frozen_at: None,
        worktree: None,
    };
    let mut tx = repo.pool().begin().await.unwrap();
    track_workspace_write_tx(&mut tx, current.id.as_str(), &selected)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    super::observe_for_task(repo.db(), current.id.as_str())
        .await
        .unwrap();
    let before = binding(&repo, &current).await;
    assert_ne!(before.0, binding(&repo, &first).await.0);
    assert_eq!(before.1, "subdir");
    let mut tx = repo.pool().begin().await.unwrap();
    track_workspace_write_tx(&mut tx, current.id.as_str(), &selected)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        binding(&repo, &current).await,
        before,
        "same selection must preserve normalized binding"
    );
}

#[tokio::test]
async fn workspace_identity_observation_cas_rejects_older_probe() {
    use crate::db::sqlite::{workspace_observation_snapshot, workspace_observe_tx};
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("repo");
    init(&source);
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    for subdir in ["a", "b"] {
        std::fs::create_dir(source.join(subdir)).unwrap();
    }
    let selected = tmp.path().join("alias");
    symlink(source.join("a"), &selected).unwrap();
    let current = track(&repo, &selected, TrackWorkspacePlan::AttachedFromCwd).await;
    let before = workspace_observation_snapshot(
        &mut repo.pool().acquire().await.unwrap(),
        current.id.as_str(),
    )
    .await
    .unwrap();
    let probe = workspace::directory::DirectoryIdentity::open(&selected, false).unwrap();
    std::fs::remove_file(&selected).unwrap();
    symlink(source.join("b"), &selected).unwrap();
    super::observe_for_task(repo.db(), current.id.as_str())
        .await
        .unwrap();
    let observed = workspace_observation_snapshot(
        &mut repo.pool().acquire().await.unwrap(),
        current.id.as_str(),
    )
    .await
    .unwrap();
    let mut tx = repo.pool().begin().await.unwrap();
    assert!(
        workspace_observe_tx(&mut tx, &before, &probe)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    let after = workspace_observation_snapshot(
        &mut repo.pool().acquire().await.unwrap(),
        current.id.as_str(),
    )
    .await
    .unwrap();
    assert_eq!(observed.state, after.state);
    assert_eq!(observed.workspace_id, after.workspace_id);
}

#[tokio::test]
async fn workspace_identity_deleted_selection_is_not_revived_by_same_path_recreation() {
    use crate::db::sqlite::{DirectoryObservation, workspace_observation_snapshot};
    use std::os::unix::fs::MetadataExt;
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("repo");
    init(&source);
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let old = track(&repo, &source, TrackWorkspacePlan::AttachedFromCwd).await;
    super::observe_for_task(repo.db(), old.id.as_str())
        .await
        .unwrap();
    let before =
        workspace_observation_snapshot(&mut repo.pool().acquire().await.unwrap(), old.id.as_str())
            .await
            .unwrap();
    let original_inode = std::fs::metadata(&source).unwrap().ino();
    delete_through_sweep(&repo, &old).await;
    assert!(
        source.is_dir(),
        "borrowed teardown never destroys its directory"
    );
    std::fs::remove_dir_all(&source).unwrap();
    init(&source);
    eprintln!(
        "real same-path delete/create: old inode={original_inode}, new inode={}",
        std::fs::metadata(&source).unwrap().ino()
    );
    let fresh = track(&repo, &source, TrackWorkspacePlan::AttachedFromCwd).await;
    let pending = workspace_observation_snapshot(
        &mut repo.pool().acquire().await.unwrap(),
        fresh.id.as_str(),
    )
    .await
    .unwrap();
    assert_ne!(
        before.workspace_id, pending.workspace_id,
        "a new track cannot revive an isolated historical selection"
    );
    assert_eq!(
        pending.state,
        DirectoryObservation::Pending,
        "no historical cwd or observation is inherited"
    );
    super::observe_for_task(repo.db(), fresh.id.as_str())
        .await
        .unwrap();
    let retained: (i64, i64) = sqlx::query_as(
        "SELECT observation_version,directory_inode FROM workspace_resources WHERE id=?1",
    )
    .bind(before.workspace_id.as_str())
    .fetch_one(repo.pool())
    .await
    .unwrap();
    let DirectoryObservation::Observed(before) = before.state else {
        panic!("old observation");
    };
    assert_eq!(
        retained,
        (before.version, before.inode),
        "history is audit, not an inode reservation"
    );
}

async fn delete_through_sweep(repo: &SqlxRepo, current: &Track) {
    let mut tx = repo.pool().begin().await.unwrap();
    let sweep = crate::workspace_service::sweep::workspace_track_sweep_for_track_tx(
        &mut tx,
        current.id.as_str(),
    )
    .await
    .unwrap()
    .unwrap();
    sqlx::query("DELETE FROM areas WHERE id=?1")
        .bind(current.area_id.as_str())
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    workspace_runtime::sweep::sweep_workspace_worktrees_for_tracks(
        vec![sweep],
        crate::workspace_service::ports::ServerWorkspacePorts,
        crate::workspace_service::ports::observe,
    )
    .await;
}

#[tokio::test]
async fn workspace_identity_owned_create_delete_create_keeps_history_without_reserving_inode() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("repo");
    init(&source);
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let old = track(
        &repo,
        &source,
        TrackWorkspacePlan::AttachedWithTrackWorktree(source.clone()),
    )
    .await;
    workspace_runtime::track_worktree::ensure_track_worktree(
        &old,
        crate::workspace_service::ports::ServerWorkspacePorts,
        crate::workspace_service::ports::observe,
    )
    .await
    .unwrap();
    super::observe_for_task(repo.db(), old.id.as_str())
        .await
        .unwrap();
    let before = binding(&repo, &old).await;
    delete_through_sweep(&repo, &old).await;
    assert!(!Path::new(old.workspace.worktree.as_deref().unwrap()).exists());
    let fresh = track(
        &repo,
        &source,
        TrackWorkspacePlan::AttachedWithTrackWorktree(source.clone()),
    )
    .await;
    workspace_runtime::track_worktree::ensure_track_worktree(
        &fresh,
        crate::workspace_service::ports::ServerWorkspacePorts,
        crate::workspace_service::ports::observe,
    )
    .await
    .unwrap();
    super::observe_for_task(repo.db(), fresh.id.as_str())
        .await
        .unwrap();
    assert_ne!(before.0, binding(&repo, &fresh).await.0);
    let retained: i64 = sqlx::query_scalar("SELECT count(*) FROM workspace_resources WHERE id=?1")
        .bind(before.0)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(retained, 1);
}

#[tokio::test]
async fn workspace_identity_identical_facts_stale_probe_is_idempotent() {
    use crate::db::sqlite::{workspace_observation_snapshot, workspace_observe_tx};
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("repo");
    init(&source);
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let current = track(&repo, &source, TrackWorkspacePlan::AttachedFromCwd).await;
    let before = workspace_observation_snapshot(
        &mut repo.pool().acquire().await.unwrap(),
        current.id.as_str(),
    )
    .await
    .unwrap();
    let probe = workspace::directory::DirectoryIdentity::open(&source, false).unwrap();
    super::observe_for_task(repo.db(), current.id.as_str())
        .await
        .unwrap();
    let observed = workspace_observation_snapshot(
        &mut repo.pool().acquire().await.unwrap(),
        current.id.as_str(),
    )
    .await
    .unwrap();
    let mut tx = repo.pool().begin().await.unwrap();
    workspace_observe_tx(&mut tx, &before, &probe)
        .await
        .expect("a completed identical observation is not a launch conflict");
    tx.commit().await.unwrap();
    let after = workspace_observation_snapshot(
        &mut repo.pool().acquire().await.unwrap(),
        current.id.as_str(),
    )
    .await
    .unwrap();
    assert_eq!(
        observed.state, after.state,
        "idempotent convergence does not change observation version"
    );
}

#[tokio::test]
async fn workspace_identity_concurrent_terminal_hooks_converge_identical_facts() {
    use crate::{
        db::sqlite::{DirectoryObservation, workspace_observation_snapshot},
        ids::ActorId,
        operation::{
            ProviderAdapter, SqlxOperationRepo,
            terminal_adapter::{TerminalWorkerAdapter, TerminalWorkerOperationPayload},
        },
    };
    use std::sync::Arc;
    let tmp = tempfile::tempdir().unwrap();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let current = track(
        &repo,
        tmp.path(),
        TrackWorkspacePlan::ManagedUnder(tmp.path().into()),
    )
    .await;
    workspace_runtime::selection::materialize_workspace(
        &current.workspace,
        tmp.path(),
        current.id.as_str(),
    )
    .unwrap();
    let ctx = super::legacy_tests::context(
        repo.clone(),
        Arc::new(SqlxOperationRepo::new(repo.db().clone())),
    );
    let adapter = TerminalWorkerAdapter::new(
        repo.clone(),
        repo.card_role_cache().clone(),
        repo.track_area_cache().clone(),
    );
    let payload = |key: &str| {
        serde_json::to_value(TerminalWorkerOperationPayload {
            actor: ActorId::KernelDispatcher,
            track_id: current.id.to_string(),
            idempotency_key: key.into(),
            cmd: "true".into(),
            cwd: None,
        })
        .unwrap()
    };
    let first = payload("first");
    let second = payload("second");
    let first_op = super::legacy_tests::pending_terminal_operation(&ctx, &first).await;
    let second_op = super::legacy_tests::pending_terminal_operation(&ctx, &second).await;
    let (a, b) = tokio::join!(
        adapter.prepare_external(&first, &first_op, &ctx),
        adapter.prepare_external(&second, &second_op, &ctx)
    );
    a.expect("first legitimate pending terminal observation");
    b.expect("second legitimate pending terminal observation");
    let observed = workspace_observation_snapshot(
        &mut repo.pool().acquire().await.unwrap(),
        current.id.as_str(),
    )
    .await
    .unwrap();
    let DirectoryObservation::Observed(observed) = observed.state else {
        panic!("observed by actual terminal hooks")
    };
    assert_eq!(
        observed.version, 1,
        "identical hooks converge without metadata version churn"
    );
}
