use std::process::Command;

use super::base::BaseSource;
use super::*;
use crate::db::sqlite::begin_immediate_tx;

#[test]
fn remove_workspace_dir_if_exists_treats_missing_as_success() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("already-gone");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::remove_dir_all(&path).unwrap();

    remove_workspace_dir_if_exists(path.to_str().unwrap()).unwrap();
}

#[tokio::test]
async fn acquire_workspace_lease_anchors_under_git_root_without_creating_leaf() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    let (repo, track_id, card_id) = lease_fixture(tmp.path()).await;

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let target = prepare_workspace_lease_target_tx(
        &mut tx,
        &track_id,
        &card_id,
        &std::env::temp_dir().join("neige-calm-test-unused-workspace-root"),
    )
    .await
    .unwrap();
    assert!(target.repo_root.is_absolute());
    assert_eq!(
        target.repo_root.canonicalize().unwrap(),
        tmp.path().canonicalize().unwrap()
    );
    assert!(target.path.is_absolute());
    assert!(target.path.starts_with(&target.repo_root));

    let base = head_base(&target);
    let (lease, _event) =
        acquire_workspace_lease_tx(&mut tx, &card_id, &track_id, "op-test", &target, &base)
            .await
            .unwrap();
    assert_eq!(lease.path, target.path_string());
    assert!(
        target.path.parent().unwrap().is_dir(),
        "lease acquisition creates the worktree parent"
    );
    assert!(
        !target.path.exists(),
        "lease acquisition must leave the worktree leaf for git worktree add"
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn worktree_mode_workspace_leased_is_not_ready_until_worktree_provisioned() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    let (repo, track_id, card_id) = lease_fixture(tmp.path()).await;

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let target = prepare_workspace_lease_target_tx(
        &mut tx,
        &track_id,
        &card_id,
        &std::env::temp_dir().join("neige-calm-test-unused-workspace-root"),
    )
    .await
    .unwrap();
    let base = head_base(&target);
    let (lease, leased) =
        acquire_workspace_lease_tx(&mut tx, &card_id, &track_id, "op-test", &target, &base)
            .await
            .unwrap();
    tx.commit().await.unwrap();

    assert!(matches!(leased.event, Event::WorkspaceLeased { .. }));
    assert_eq!(lease.path, target.path_string());
    assert!(
        !Path::new(&lease.path).exists(),
        "workspace.leased carries the future worktree leaf, not a usable cwd"
    );
    assert_eq!(event_kind_count(&repo, "workspace.leased").await, 1);
    assert_eq!(event_kind_count(&repo, "worktree.provisioned").await, 0);

    provision_workspace_worktree(&target, &pinned(&base)).unwrap();
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let scope = workspace_scope_tx(&mut tx, &card_id, &track_id)
        .await
        .unwrap();
    append_workspace_events_tx(
        &mut tx,
        vec![(
            ActorId::KernelDispatcher,
            scope,
            Event::WorktreeProvisioned {
                track_id: TrackId::from(track_id.clone()),
                card_id: CardId::from(card_id.clone()),
                path: target.path_string(),
            },
        )],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    assert!(
        target.path.is_dir(),
        "worktree.provisioned is the ready-cwd signal"
    );
    assert_eq!(event_kind_count(&repo, "worktree.provisioned").await, 1);
}

#[tokio::test]
async fn workspace_lease_target_rejects_non_git_track_cwd_without_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, track_id, card_id) = lease_fixture(tmp.path()).await;

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let err = prepare_workspace_lease_target_tx(
        &mut tx,
        &track_id,
        &card_id,
        &std::env::temp_dir().join("neige-calm-test-unused-workspace-root"),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CalmError::BadRequest(_)));
    tx.rollback().await.unwrap();

    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspace_leases")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(rows, 0);
}

#[tokio::test]
async fn acquire_plain_workspace_lease_creates_leaf_for_non_git_track_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, track_id, card_id) = lease_fixture(tmp.path()).await;
    let path = plain_workspace_lease_path_for(&track_id, &card_id).unwrap();
    assert!(
        !path.is_absolute(),
        "plain workspace lease path is legacy-relative"
    );

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let (lease, _event) =
        acquire_plain_workspace_lease_tx(&mut tx, &card_id, &track_id, "op-test", &path)
            .await
            .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(lease.path, path.to_string_lossy().to_string());
    assert!(path.is_dir(), "plain lease acquisition creates the leaf");

    let events = EventBus::new();
    assert!(
        release_workspace_lease_for_card_repo(&repo, &events, &card_id)
            .await
            .unwrap()
    );
    assert!(path.exists(), "plain lease release preserves the leaf");
}

#[test]
fn workspace_worktree_remove_deletes_branch_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    let target = WorkspaceLeaseTarget {
        repo_root: tmp.path().to_path_buf(),
        path: tmp.path().join(".claude/worktrees/track-a/card-a"),
        branch: workspace_slice_branch_for("track-a", "card-a").unwrap(),
    };

    provision_workspace_worktree(&target, &pinned(&head_base(&target))).unwrap();
    assert!(target.path.is_dir(), "provisioned worktree exists");
    assert!(
        git_ref_exists(&target.repo_root, &format!("refs/heads/{}", target.branch)).unwrap(),
        "slice branch exists"
    );

    remove_workspace_worktree(&target).unwrap();
    assert!(!target.path.exists(), "worktree path removed");
    assert!(
        !git_ref_exists(&target.repo_root, &format!("refs/heads/{}", target.branch)).unwrap(),
        "slice branch removed"
    );

    remove_workspace_worktree(&target).unwrap();
}

#[test]
fn workspace_worktree_provision_excludes_root_from_base_status() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    let target = WorkspaceLeaseTarget {
        repo_root: tmp.path().to_path_buf(),
        path: tmp.path().join(".claude/worktrees/track-clean/card-clean"),
        branch: workspace_slice_branch_for("track-clean", "card-clean").unwrap(),
    };

    provision_workspace_worktree(&target, &pinned(&head_base(&target))).unwrap();

    let status = git_stdout(tmp.path(), ["status", "--short", "--untracked-files=all"]);
    assert_eq!(status, "", "base repo must stay clean after provisioning");

    provision_workspace_worktree(&target, &pinned(&head_base(&target))).unwrap();
    let exclude = std::fs::read_to_string(tmp.path().join(".git/info/exclude")).unwrap();
    assert_eq!(
        exclude
            .lines()
            .filter(|line| line.trim() == ".claude/worktrees/")
            .count(),
        1,
        "worktree exclude entry is idempotent"
    );
}

#[test]
fn workspace_worktree_provision_recreates_stale_registered_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    let target = WorkspaceLeaseTarget {
        repo_root: tmp.path().to_path_buf(),
        path: tmp.path().join(".claude/worktrees/track-stale/card-stale"),
        branch: workspace_slice_branch_for("track-stale", "card-stale").unwrap(),
    };

    provision_workspace_worktree(&target, &pinned(&head_base(&target))).unwrap();
    assert!(target.path.is_dir(), "initial worktree exists");
    std::fs::remove_dir_all(&target.path).unwrap();
    assert!(
        !target.path.exists(),
        "test setup leaves a registered but missing worktree path"
    );
    assert_ne!(
        git_worktree_registration(&target).unwrap(),
        GitWorktreeRegistration::Absent,
        "git still has a stale worktree registration"
    );

    provision_workspace_worktree(&target, &pinned(&head_base(&target))).unwrap();

    assert!(
        target.path.is_dir(),
        "stale registration is re-provisioned as a real worktree"
    );
    assert_eq!(
        git_worktree_registration(&target).unwrap(),
        GitWorktreeRegistration::Present
    );
    let top_level = git_stdout(&target.path, ["rev-parse", "--show-toplevel"]);
    assert_eq!(
        PathBuf::from(top_level.trim()).canonicalize().unwrap(),
        target.path.canonicalize().unwrap(),
        "re-provisioned path is a usable git worktree"
    );
}

#[test]
fn workspace_worktree_provision_clears_stale_unregistered_non_empty_dir() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    let target = WorkspaceLeaseTarget {
        repo_root: tmp.path().to_path_buf(),
        path: tmp
            .path()
            .join(".claude/worktrees/track-unregistered/card-unregistered"),
        branch: workspace_slice_branch_for("track-unregistered", "card-unregistered").unwrap(),
    };
    std::fs::create_dir_all(&target.path).unwrap();
    std::fs::write(target.path.join("stale.txt"), "partial worktree add\n").unwrap();
    assert_eq!(
        git_worktree_registration(&target).unwrap(),
        GitWorktreeRegistration::Absent,
        "test setup leaves a non-empty directory without worktree registration"
    );

    provision_workspace_worktree(&target, &pinned(&head_base(&target))).unwrap();

    assert!(
        target.path.is_dir(),
        "stale unregistered directory is re-provisioned as a real worktree"
    );
    assert!(
        !target.path.join("stale.txt").exists(),
        "stale unregistered contents are cleared before git worktree add"
    );
    assert_eq!(
        git_worktree_registration(&target).unwrap(),
        GitWorktreeRegistration::Present
    );
    let top_level = git_stdout(&target.path, ["rev-parse", "--show-toplevel"]);
    assert_eq!(
        PathBuf::from(top_level.trim()).canonicalize().unwrap(),
        target.path.canonicalize().unwrap(),
        "re-provisioned path is a usable git worktree"
    );
}

#[tokio::test]
async fn workspace_worktree_provision_resolves_exclude_for_linked_track_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("primary");
    init_git_repo(&primary);
    let linked = tmp.path().join("linked-track");
    let linked_str = linked.to_str().unwrap();
    run_git(
        &primary,
        ["worktree", "add", "-b", "linked-track", linked_str],
    );
    assert!(
        linked.join(".git").is_file(),
        "linked worktree .git is a gitdir file"
    );

    let (repo, track_id, card_id) = lease_fixture(&linked).await;
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let target = prepare_workspace_lease_target_tx(
        &mut tx,
        &track_id,
        &card_id,
        &std::env::temp_dir().join("neige-calm-test-unused-workspace-root"),
    )
    .await
    .unwrap();
    assert_eq!(
        target.repo_root.canonicalize().unwrap(),
        linked.canonicalize().unwrap()
    );
    let base = head_base(&target);
    let (_lease, _event) =
        acquire_workspace_lease_tx(&mut tx, &card_id, &track_id, "op-test", &target, &base)
            .await
            .unwrap();
    tx.commit().await.unwrap();

    provision_workspace_worktree(&target, &pinned(&base)).unwrap();

    assert!(target.path.is_dir(), "provisioned worktree exists");
    assert_eq!(
        git_stdout(&linked, ["status", "--short", "--untracked-files=all"]),
        "",
        "linked track worktree must stay clean after provisioning"
    );
    let exclude_path = git_exclude_path(&linked).unwrap();
    assert_eq!(
        exclude_path.canonicalize().unwrap(),
        primary.join(".git/info/exclude").canonicalize().unwrap()
    );
    let exclude = std::fs::read_to_string(&exclude_path).unwrap();
    assert_eq!(
        exclude
            .lines()
            .filter(|line| line.trim() == ".claude/worktrees/")
            .count(),
        1,
        "linked worktree exclude entry is written once"
    );

    provision_workspace_worktree(&target, &pinned(&base)).unwrap();
    let exclude = std::fs::read_to_string(&exclude_path).unwrap();
    assert_eq!(
        exclude
            .lines()
            .filter(|line| line.trim() == ".claude/worktrees/")
            .count(),
        1,
        "linked worktree exclude entry remains idempotent"
    );
}

#[tokio::test]
async fn card_release_preserves_worktree_branch_and_emits_no_removed_event() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    let (repo, track_id, card_id) = lease_fixture(tmp.path()).await;

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let target = prepare_workspace_lease_target_tx(
        &mut tx,
        &track_id,
        &card_id,
        &std::env::temp_dir().join("neige-calm-test-unused-workspace-root"),
    )
    .await
    .unwrap();
    let base = head_base(&target);
    let (lease, _event) =
        acquire_workspace_lease_tx(&mut tx, &card_id, &track_id, "op-test", &target, &base)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    provision_workspace_worktree(&target, &pinned(&base)).unwrap();
    std::fs::write(target.path.join("worker-output.txt"), "worker commit\n").unwrap();
    run_git(&target.path, ["add", "worker-output.txt"]);
    run_git(&target.path, ["commit", "-m", "worker output"]);

    let events = EventBus::new();
    assert!(
        release_workspace_lease_for_card_repo(&repo, &events, &card_id)
            .await
            .unwrap()
    );

    let state: String =
        sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
            .bind(&lease.lease_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(state, "released");
    assert!(
        target.path.is_dir(),
        "normal card release preserves the worker worktree"
    );
    assert!(
        git_ref_exists(&target.repo_root, &format!("refs/heads/{}", target.branch)).unwrap(),
        "normal card release preserves the slice branch"
    );
    assert_eq!(event_kind_count(&repo, "workspace.released").await, 1);
    assert_eq!(event_kind_count(&repo, "worktree.removed").await, 0);
}

#[tokio::test]
async fn rollback_removes_worktree_before_releasing_lease_row() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    let (repo, track_id, card_id) = lease_fixture(tmp.path()).await;

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let target = prepare_workspace_lease_target_tx(
        &mut tx,
        &track_id,
        &card_id,
        &std::env::temp_dir().join("neige-calm-test-unused-workspace-root"),
    )
    .await
    .unwrap();
    let base = head_base(&target);
    let (lease, _event) =
        acquire_workspace_lease_tx(&mut tx, &card_id, &track_id, "op-test", &target, &base)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    provision_workspace_worktree(&target, &pinned(&base)).unwrap();

    let events = EventBus::new();
    assert!(
        remove_workspace_artifact_for_lease_by_id(repo.pool(), &events, &lease.lease_id)
            .await
            .unwrap()
    );
    assert!(
        release_workspace_lease_by_id(repo.pool(), &events, &lease.lease_id)
            .await
            .unwrap()
    );

    let state: String =
        sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
            .bind(&lease.lease_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(state, "released");
    assert!(
        !target.path.exists(),
        "rollback removal deletes the just-provisioned worktree"
    );
    assert!(
        !git_ref_exists(&target.repo_root, &format!("refs/heads/{}", target.branch)).unwrap(),
        "rollback removal deletes the just-created slice branch"
    );
    assert_eq!(event_kind_count(&repo, "workspace.released").await, 1);
    assert_eq!(event_kind_count(&repo, "worktree.removed").await, 1);
}

#[tokio::test]
async fn release_by_id_removes_artifact_before_workspace_released() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    let (repo, track_id, card_id) = lease_fixture(tmp.path()).await;

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let target = prepare_workspace_lease_target_tx(
        &mut tx,
        &track_id,
        &card_id,
        &std::env::temp_dir().join("neige-calm-test-unused-workspace-root"),
    )
    .await
    .unwrap();
    let base = head_base(&target);
    let (lease, _event) =
        acquire_workspace_lease_tx(&mut tx, &card_id, &track_id, "op-test", &target, &base)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    provision_workspace_worktree(&target, &pinned(&base)).unwrap();

    let events = EventBus::new();
    assert!(
        release_workspace_lease_by_id(repo.pool(), &events, &lease.lease_id)
            .await
            .unwrap()
    );

    let state: String =
        sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
            .bind(&lease.lease_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(state, "released");
    assert!(
        !target.path.exists(),
        "by-id compensating release removes the worktree artifact"
    );
    assert!(
        !git_ref_exists(&target.repo_root, &format!("refs/heads/{}", target.branch)).unwrap(),
        "by-id compensating release removes the slice branch"
    );
    let kinds: Vec<String> = sqlx::query_scalar(
        "SELECT kind FROM events \
             WHERE kind IN ('worktree.removed', 'workspace.released') \
             ORDER BY id ASC",
    )
    .fetch_all(repo.pool())
    .await
    .unwrap();
    assert_eq!(kinds, vec!["worktree.removed", "workspace.released"]);
}

#[tokio::test]
async fn track_release_sweeps_worktrees_plain_dirs_and_branches_post_commit() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    let (repo, track_id, card_id) = lease_fixture(tmp.path()).await;

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let target = prepare_workspace_lease_target_tx(
        &mut tx,
        &track_id,
        &card_id,
        &std::env::temp_dir().join("neige-calm-test-unused-workspace-root"),
    )
    .await
    .unwrap();
    let base = head_base(&target);
    let (_lease, _event) =
        acquire_workspace_lease_tx(&mut tx, &card_id, &track_id, "op-test", &target, &base)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    provision_workspace_worktree(&target, &pinned(&base)).unwrap();

    let events = EventBus::new();
    release_workspace_lease_for_card_repo(&repo, &events, &card_id)
        .await
        .unwrap();
    assert!(
        target.path.is_dir(),
        "preserved worktree exists after normal release"
    );
    assert!(
        git_ref_exists(&target.repo_root, &format!("refs/heads/{}", target.branch)).unwrap(),
        "preserved branch exists after normal release"
    );
    let plain_card_id = "plain-card";
    let plain_path = tmp
        .path()
        .join(".claude")
        .join("worktrees")
        .join(&track_id)
        .join(plain_card_id);
    std::fs::create_dir_all(&plain_path).unwrap();
    std::fs::write(plain_path.join("leftover.txt"), "plain leftover\n").unwrap();
    let plain_branch = workspace_slice_branch_for(&track_id, plain_card_id).unwrap();
    run_git(tmp.path(), ["branch", &plain_branch]);
    let branch_only = workspace_slice_branch_for(&track_id, "branch-only").unwrap();
    run_git(tmp.path(), ["branch", &branch_only]);

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let release = release_workspace_leases_for_track_tx(&mut tx, &track_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert!(
        release.events.is_empty(),
        "released lease rows do not emit another workspace release"
    );
    let sweep = release.sweep.expect("track sweep plan");
    assert_eq!(
        sweep_workspace_worktrees_for_track_repo(&repo, &events, sweep.clone())
            .await
            .unwrap(),
        2
    );
    assert!(
        !target.path.exists(),
        "track teardown sweeps preserved worktree paths"
    );
    assert!(
        !plain_path.exists(),
        "track teardown sweeps leftover plain workspace dirs"
    );
    assert!(
        !git_ref_exists(&target.repo_root, &format!("refs/heads/{}", target.branch)).unwrap(),
        "track teardown sweeps preserved slice branches"
    );
    assert!(
        !git_ref_exists(&target.repo_root, &format!("refs/heads/{plain_branch}")).unwrap(),
        "track teardown sweeps plain-dir slice branches"
    );
    assert!(
        !git_ref_exists(&target.repo_root, &format!("refs/heads/{branch_only}")).unwrap(),
        "track teardown sweeps branch-only slice branches"
    );
    assert_eq!(event_kind_count(&repo, "worktree.removed").await, 2);
    // Review 5 (A5-m3): the second sweep runs with the track root gone (the
    // first sweep `remove_dir`ed it once empty; removed by hand otherwise) —
    // the `NotFound` arm of the root identity check, which must not read as
    // a mismatch: a branch minted after the first sweep is still swept, where
    // a refusal (also `Ok(0)`) would leave it.
    let track_root = tmp.path().join(".claude").join("worktrees").join(&track_id);
    if track_root.exists() {
        std::fs::remove_dir_all(&track_root).unwrap();
    }
    assert!(
        !track_root.exists(),
        "the track root is gone before the second sweep"
    );
    let late_branch = workspace_slice_branch_for(&track_id, "late").unwrap();
    run_git(tmp.path(), ["branch", &late_branch]);
    assert_eq!(
        sweep_workspace_worktrees_for_track_repo(&repo, &events, sweep)
            .await
            .unwrap(),
        0,
        "track sweep is idempotent after paths are gone"
    );
    assert_eq!(
        event_kind_count(&repo, "worktree.removed").await,
        2,
        "idempotent sweep emits no duplicate removal events"
    );
    assert!(
        !git_ref_exists(&target.repo_root, &format!("refs/heads/{late_branch}")).unwrap(),
        "a sweep with the track root gone still deletes the track's slice branches"
    );
}

#[tokio::test]
async fn track_sweep_uses_persisted_lease_paths_when_track_cwd_is_deleted() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    let track_cwd = tmp.path().join("deleted-track-cwd");
    std::fs::create_dir_all(&track_cwd).unwrap();
    let (repo, track_id, card_id) = lease_fixture(&track_cwd).await;

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let target = prepare_workspace_lease_target_tx(
        &mut tx,
        &track_id,
        &card_id,
        &std::env::temp_dir().join("neige-calm-test-unused-workspace-root"),
    )
    .await
    .unwrap();
    let base = head_base(&target);
    let (_lease, _event) =
        acquire_workspace_lease_tx(&mut tx, &card_id, &track_id, "op-test", &target, &base)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    provision_workspace_worktree(&target, &pinned(&base)).unwrap();
    assert!(target.path.is_dir(), "test setup provisioned worktree");

    let events = EventBus::new();
    release_workspace_lease_for_card_repo(&repo, &events, &card_id)
        .await
        .unwrap();
    std::fs::remove_dir_all(&track_cwd).unwrap();
    assert!(
        git_repo_root_for_track_cwd(&track_id, track_cwd.to_str().unwrap()).is_err(),
        "test setup leaves track.cwd unusable for git -C"
    );

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let release = release_workspace_leases_for_track_tx(&mut tx, &track_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert!(
        release.events.is_empty(),
        "released lease rows do not emit another workspace release"
    );
    let sweep = release.sweep.expect("track sweep plan");
    assert_eq!(
        sweep_workspace_worktrees_for_track_repo(&repo, &events, sweep)
            .await
            .unwrap(),
        1
    );
    assert!(
        !target.path.exists(),
        "sweep removes worktree using repo root recovered from persisted lease path"
    );
    assert!(
        !git_ref_exists(&target.repo_root, &format!("refs/heads/{}", target.branch)).unwrap(),
        "sweep removes branch using repo root recovered from persisted lease path"
    );
    assert_eq!(event_kind_count(&repo, "worktree.removed").await, 1);
}

/// The base production records for a fresh lease: the repository's HEAD now,
/// resolved the way the worker op's prepare tx resolves it.
fn head_base(target: &WorkspaceLeaseTarget) -> LeaseBase {
    base::resolve_head_lease_base(target).unwrap()
}

fn pinned(base: &LeaseBase) -> WorktreeBase {
    WorktreeBase::from_lease_base(base)
}

async fn lease_fixture(track_cwd: &Path) -> (crate::db::sqlite::SqlxRepo, String, String) {
    let repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let area = crate::db::RepoSyncDomainRaw::area_create(
        &repo,
        crate::model::NewArea {
            name: "lease fixture".into(),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let track = crate::db::RepoSyncDomainRaw::track_create(
        &repo,
        crate::model::NewTrack {
            template_input: None,
            area_id: area.id,
            title: "lease fixture".into(),
            sort: None,
            cwd: track_cwd.display().to_string(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        },
    )
    .await
    .unwrap();
    let card = crate::db::RepoSyncDomainRaw::card_create(
        &repo,
        crate::model::NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: serde_json::Value::Null,
        },
    )
    .await
    .unwrap();
    (repo, track.id.to_string(), card.id.to_string())
}

async fn event_kind_count(repo: &crate::db::sqlite::SqlxRepo, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = ?1")
        .bind(kind)
        .fetch_one(repo.pool())
        .await
        .unwrap()
}

fn init_git_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    run_git(path, ["init"]);
    run_git(path, ["config", "user.email", "lease@example.test"]);
    run_git(path, ["config", "user.name", "Lease Test"]);
    std::fs::write(path.join("README.md"), "initial\n").unwrap();
    run_git(path, ["add", "README.md"]);
    run_git(path, ["commit", "-m", "initial"]);
}

fn run_git<const N: usize>(repo: &Path, args: [&str; N]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed in {}\nstdout:\n{}\nstderr:\n{}",
        args,
        repo.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout<const N: usize>(repo: &Path, args: [&str; N]) -> String {
    String::from_utf8_lossy(&git_stdout_bytes(repo, args)).to_string()
}

fn git_stdout_bytes<const N: usize>(repo: &Path, args: [&str; N]) -> Vec<u8> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed in {}\nstdout:\n{}\nstderr:\n{}",
        args,
        repo.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

// ---------------------------------------------------------------------------
// #1727 S4 slice 1 — lease base: recorded in the prepare tx, pinned at
// provisioning, read back by every reader (design §6 A1 / A1b / A2 / A2b, 3n).
// ---------------------------------------------------------------------------

/// A1b — one lease row with `base_sha / canonical_path / git_common_dir` set,
/// read through each of the seven readers of `workspace_leases` rows. Every
/// reader spells its SELECT through `WORKSPACE_LEASE_COLUMNS` and decodes via
/// `row_to_workspace_lease`, which takes the base columns by name: a SELECT
/// that fell back to the old six-column list would fail here at run time
/// (`ColumnNotFound`), which is what this fixture exists to catch.
#[tokio::test]
async fn every_lease_reader_returns_base_and_policy() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    let (repo, track_id, card_for_card_repo) = lease_fixture(tmp.path()).await;
    let events = EventBus::new();
    let head = git_stdout(tmp.path(), ["rev-parse", "HEAD"])
        .trim()
        .to_string();

    // One held lease per reader that consumes a row; all through production
    // acquisition so the row carries exactly what the prepare tx writes.
    let take = |card_id: String| {
        let repo = &repo;
        let track_id = &track_id;
        async move {
            let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
            let target = prepare_workspace_lease_target_tx(
                &mut tx,
                track_id,
                &card_id,
                &std::env::temp_dir().join("neige-calm-test-unused-workspace-root"),
            )
            .await
            .unwrap();
            let base = head_base(&target);
            let (lease, _event) =
                acquire_workspace_lease_tx(&mut tx, &card_id, track_id, "op-test", &target, &base)
                    .await
                    .unwrap();
            tx.commit().await.unwrap();
            assert_eq!(lease.base.as_ref(), Some(&base));
            (card_id, lease)
        }
    };
    let (card_repo, _) = take(card_for_card_repo).await;
    let (card_tx, _) = take(new_card(&repo, &track_id).await).await;
    let (_card_by_id, lease_by_id) = take(new_card(&repo, &track_id).await).await;
    let (_card_boot, lease_boot) = take(new_card(&repo, &track_id).await).await;
    let (card_facts, _) = take(new_card(&repo, &track_id).await).await;
    let state_of = |lease_id: String| {
        let repo = &repo;
        async move {
            sqlx::query_scalar::<_, String>(
                "SELECT state FROM workspace_leases WHERE lease_id = ?1",
            )
            .bind(lease_id)
            .fetch_one(repo.pool())
            .await
            .unwrap()
        }
    };

    // 1. `release_workspace_lease_for_card_repo` (`:344`).
    assert!(
        release_workspace_lease_for_card_repo(&repo, &events, &card_repo)
            .await
            .unwrap(),
        "card-repo release decodes and releases the based row"
    );

    // 2. `release_workspace_lease_for_card_tx` (`:377`).
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let released = release_workspace_lease_for_card_tx(&mut tx, &card_tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(released.len(), 1, "card-tx release decodes the based row");

    // 5. by-id: `workspace_lease_by_id` (`:1047`) — the worker op's
    //    compensation step `release_workspace_lease`.
    let by_id = workspace_lease_by_id(repo.pool(), &lease_by_id.lease_id)
        .await
        .unwrap()
        .expect("held row by id");
    assert!(by_id.base.is_some(), "by-id read carries the base");
    assert!(
        crate::test_seams::release_workspace_lease_by_id_for_test(
            repo.pool(),
            &events,
            &lease_by_id.lease_id
        )
        .await
        .unwrap(),
        "by-id compensating release decodes and releases the based row"
    );
    assert_eq!(state_of(lease_by_id.lease_id.clone()).await, "released");

    // 7. `worker_worktree_facts_tx` (`facts.rs:75`).
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let facts = facts::worker_worktree_facts_tx(&mut tx, &card_facts)
        .await
        .unwrap()
        .expect("facts for a leased card");
    tx.commit().await.unwrap();
    assert_eq!(facts.base_sha.as_deref(), Some(head.as_str()));

    // 6. all-active: `active_workspace_leases` (`:1060`) via boot reclaim —
    //    the row is stamped with another boot so it is the one reclaimed;
    //    the facts card's lease (this boot) is left alone.
    sqlx::query("UPDATE workspace_leases SET boot_id = 'boot-from-before' WHERE lease_id = ?1")
        .bind(&lease_boot.lease_id)
        .execute(repo.pool())
        .await
        .unwrap();
    let all_active = active_workspace_leases(repo.pool()).await.unwrap();
    assert_eq!(
        all_active.len(),
        2,
        "boot + facts leases are the active rows"
    );
    assert!(all_active.iter().all(|lease| lease.base.is_some()));
    let reclaimed = reclaim_dead_workspace_leases_on_boot(repo.pool(), &events)
        .await
        .unwrap();
    assert_eq!(reclaimed, 1, "boot reclaim releases the based row");
    assert_eq!(state_of(lease_boot.lease_id.clone()).await, "released");

    // 3. `release_workspace_leases_for_track_tx` (`:509`) — the facts lease
    //    is the one still held — and 4. the Track sweep's
    //    `workspace_track_sweep_for_track_tx` (`:631`), every state.
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let release = release_workspace_leases_for_track_tx(&mut tx, &track_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        release.events.len(),
        1,
        "track release decodes the held based row"
    );
    let sweep = release.sweep.expect("track sweep plan");
    assert_eq!(sweep.leases.len(), 5, "sweep reads every row of the track");
    assert!(
        sweep.leases.iter().all(|lease| lease.base.is_some()),
        "sweep rows carry the base"
    );
    for lease in &sweep.leases {
        let base = lease.base.as_ref().unwrap();
        assert_eq!(base.base_sha, head);
        assert_eq!(base.base_source, BaseSource::Head);
        assert_eq!(base.base_attempt_id, None);
        assert_eq!(
            base.git_common_dir,
            tmp.path().join(".git").canonicalize().unwrap()
        );
        assert_eq!(
            base.canonical_path,
            tmp.path()
                .join(".claude/worktrees")
                .join(&track_id)
                .canonicalize()
                .unwrap()
                .join(&lease.card_id)
        );
    }
}

/// A1 — the base is decided in the prepare tx; the repository's HEAD moving
/// between prepare and spawn does not move the worktree's starting point.
#[tokio::test]
async fn worktree_starts_at_recorded_base_not_moving_head() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    let (repo, track_id, card_id) = lease_fixture(tmp.path()).await;

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let target = prepare_workspace_lease_target_tx(
        &mut tx,
        &track_id,
        &card_id,
        &std::env::temp_dir().join("neige-calm-test-unused-workspace-root"),
    )
    .await
    .unwrap();
    let base = head_base(&target);
    let (lease, _event) =
        acquire_workspace_lease_tx(&mut tx, &card_id, &track_id, "op-test", &target, &base)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    let recorded = workspace_lease_by_id(repo.pool(), &lease.lease_id)
        .await
        .unwrap()
        .unwrap()
        .base
        .expect("prepared lease records its base");
    assert_eq!(recorded.base_sha, base.base_sha);

    // The repository moves on after prepare and before spawn.
    run_git(
        tmp.path(),
        ["commit", "--allow-empty", "-m", "moved after prepare"],
    );
    let moved_head = git_stdout(tmp.path(), ["rev-parse", "HEAD"])
        .trim()
        .to_string();
    assert_ne!(moved_head, base.base_sha, "test setup moved HEAD");

    provision_workspace_worktree(&target, &pinned(&base)).unwrap();

    let worktree_head = git_stdout(&target.path, ["rev-parse", "HEAD"])
        .trim()
        .to_string();
    assert_eq!(
        worktree_head, recorded.base_sha,
        "worktree starts at the recorded base"
    );
    assert_ne!(worktree_head, moved_head, "not at the moved HEAD");
    assert_eq!(
        target.path.canonicalize().unwrap(),
        recorded.canonical_path,
        "worktree is at the recorded realpath"
    );
}

/// A2 — a slice branch that already exists (crash residue, a hand-made
/// branch) at a tip other than the recorded base is not trusted: the
/// registration is made from it and then refused, naming both shas.
#[test]
fn existing_branch_with_foreign_tip_refuses_launch() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    let target = WorkspaceLeaseTarget {
        repo_root: tmp.path().to_path_buf(),
        path: tmp
            .path()
            .join(".claude/worktrees/track-foreign/card-foreign"),
        branch: workspace_slice_branch_for("track-foreign", "card-foreign").unwrap(),
    };
    let base = head_base(&target);
    let foreign = git_stdout(
        tmp.path(),
        [
            "commit-tree",
            "HEAD^{tree}",
            "-p",
            "HEAD",
            "-m",
            "foreign tip",
        ],
    )
    .trim()
    .to_string();
    assert_ne!(foreign, base.base_sha);
    let branch_ref = format!("refs/heads/{}", target.branch);
    run_git(tmp.path(), ["update-ref", &branch_ref, &foreign]);
    assert!(git_ref_exists(&target.repo_root, &branch_ref).unwrap());

    let err = provision_workspace_worktree(&target, &pinned(&base)).unwrap_err();

    assert!(matches!(err, CalmError::Internal(_)), "{err}");
    let message = err.to_string();
    assert!(
        message.contains(&base.base_sha) && message.contains(&foreign),
        "refusal names expected and found: {message}"
    );
}

/// 3n / Present — a worktree already registered at the lease path whose HEAD
/// has moved off the recorded base is refused instead of being handed to a
/// worker as-is.
#[test]
fn existing_registration_with_moved_tip_refuses_launch() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    let target = WorkspaceLeaseTarget {
        repo_root: tmp.path().to_path_buf(),
        path: tmp.path().join(".claude/worktrees/track-moved/card-moved"),
        branch: workspace_slice_branch_for("track-moved", "card-moved").unwrap(),
    };
    let base = head_base(&target);
    provision_workspace_worktree(&target, &pinned(&base)).unwrap();
    assert_eq!(
        git_worktree_registration(&target).unwrap(),
        GitWorktreeRegistration::Present
    );
    run_git(
        &target.path,
        ["commit", "--allow-empty", "-m", "moved inside the worktree"],
    );
    let moved = git_stdout(&target.path, ["rev-parse", "HEAD"])
        .trim()
        .to_string();
    assert_ne!(moved, base.base_sha, "test setup moved the worktree HEAD");

    let err = provision_workspace_worktree(&target, &pinned(&base)).unwrap_err();

    assert!(matches!(err, CalmError::Internal(_)), "{err}");
    let message = err.to_string();
    assert!(
        message.contains(&base.base_sha) && message.contains(&moved),
        "refusal names expected and found: {message}"
    );
    assert!(target.path.is_dir(), "refusal repairs nothing");
}

/// Review 1 F1 — `.claude/worktrees` is a symlink (design `r6/symlinkwt`).
/// Git registers the worktree at its realpath; a second provisioning (crash
/// recovery re-spawn) must find that registration and keep the worker's
/// uncommitted files, not read it as absent and rebuild the directory.
/// Review 2 (B2-M1): the realpath may contain a newline — `worktree list
/// --porcelain` then prints it over two lines and a line parser never
/// matches it; the `-z` listing is parsed by NUL-terminated records.
#[test]
fn symlinked_parent_second_provision_keeps_existing_worktree() {
    for real_name in ["real-wt", &format!("real{}wt", '\n')] {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_git_repo(&repo);
        let real = tmp.path().join(real_name);
        std::fs::create_dir_all(&real).unwrap();
        std::fs::create_dir_all(repo.join(".claude")).unwrap();
        std::os::unix::fs::symlink(&real, repo.join(".claude/worktrees")).unwrap();
        let target = WorkspaceLeaseTarget {
            repo_root: repo.clone(),
            path: repo.join(".claude/worktrees/track-sym/card-sym"),
            branch: workspace_slice_branch_for("track-sym", "card-sym").unwrap(),
        };
        let base = head_base(&target);
        assert_eq!(
            base.canonical_path,
            real.canonicalize().unwrap().join("track-sym/card-sym"),
            "{real_name:?}: canonical path resolves through the symlinked parent"
        );

        provision_workspace_worktree(&target, &pinned(&base)).unwrap();
        let sentinel = target.path.join("UNCOMMITTED.txt");
        std::fs::write(&sentinel, "worker output not yet committed\n").unwrap();
        assert_eq!(
            git_worktree_registration(&target).unwrap(),
            GitWorktreeRegistration::Present,
            "{real_name:?}: the registration at the realpath is found"
        );

        provision_workspace_worktree(&target, &pinned(&base)).unwrap();

        assert!(
            sentinel.is_file(),
            "{real_name:?}: second provisioning keeps the registered worktree's uncommitted file"
        );
        let listing = git_stdout_bytes(&repo, ["worktree", "list", "--porcelain", "-z"]);
        let registered: Vec<&[u8]> = listing
            .split(|byte| *byte == 0)
            .filter_map(|attribute| attribute.strip_prefix(b"worktree "))
            .filter(|listed| listed.ends_with(b"/card-sym"))
            .collect();
        assert_eq!(
            registered,
            vec![base.canonical_path.as_os_str().as_encoded_bytes()],
            "{real_name:?}: exactly one registration, at the realpath"
        );
    }
}

/// Review 2 (B2-M2) — the server process inherits `GIT_DIR=<repo>/.git`
/// (a hook, a CI step, a shell that exported it). Every git the lease module
/// spawns must scrub it: `git -C <worktree> rev-parse HEAD` would otherwise
/// answer with the main repository's HEAD and a worktree sitting on a foreign
/// tip would pass the base check.
///
/// The variable is set in this test's own process. `cargo nextest` (what CI
/// and the local gate run) gives each test its own process, which is what
/// makes this safe for the other tests in the binary; the guard restores the
/// previous value on drop for a shared-process runner.
#[test]
fn hostile_git_env_does_not_leak_into_lease_git() {
    struct EnvVar(&'static str, Option<std::ffi::OsString>);
    impl EnvVar {
        fn set(key: &'static str, value: &Path) -> Self {
            let previous = std::env::var_os(key);
            // SAFETY: one test, one process under nextest (see the test
            // docstring); no other thread of this test reads the environment.
            unsafe { std::env::set_var(key, value) };
            Self(key, previous)
        }
    }
    impl Drop for EnvVar {
        fn drop(&mut self) {
            // SAFETY: see `set`.
            unsafe {
                match self.1.take() {
                    Some(previous) => std::env::set_var(self.0, previous),
                    None => std::env::remove_var(self.0),
                }
            }
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);
    let target = WorkspaceLeaseTarget {
        repo_root: repo.clone(),
        path: repo.join(".claude/worktrees/track-env/card-env"),
        branch: workspace_slice_branch_for("track-env", "card-env").unwrap(),
    };
    // Main repository at C0; a registered worktree whose HEAD is C1.
    let base = head_base(&target);
    let c0 = base.base_sha.clone();
    provision_workspace_worktree(&target, &pinned(&base)).unwrap();
    run_git(
        &target.path,
        ["commit", "--allow-empty", "-m", "C1 inside the worktree"],
    );
    let c1 = git_stdout(&target.path, ["rev-parse", "HEAD"])
        .trim()
        .to_string();
    assert_ne!(c1, c0, "test setup moved the worktree HEAD");
    assert_eq!(
        git_stdout(&repo, ["rev-parse", "HEAD"]).trim(),
        c0,
        "test setup left the main repository at C0"
    );
    // A second repository the hostile variable can point at (an identical
    // init commit in the same second hashes the same as C0, so move it).
    let foreign = tmp.path().join("foreign");
    init_git_repo(&foreign);
    run_git(&foreign, ["commit", "--allow-empty", "-m", "foreign tip"]);
    let foreign_head = git_stdout(&foreign, ["rev-parse", "HEAD"])
        .trim()
        .to_string();
    assert_ne!(foreign_head, c0);

    {
        let _git_dir = EnvVar::set("GIT_DIR", &repo.join(".git"));
        let err = provision_workspace_worktree(&target, &pinned(&base))
            .expect_err("the worktree at C1 is refused against base C0");
        let message = err.to_string();
        assert!(
            message.contains(&c0) && message.contains(&c1),
            "refusal names expected C0 and found C1: {message}"
        );
        let resolved = base::resolve_head_lease_base(&target).unwrap();
        assert_eq!(resolved.base_sha, c0);
        assert_eq!(
            resolved.git_common_dir,
            repo.join(".git").canonicalize().unwrap()
        );
    }
    {
        let _git_dir = EnvVar::set("GIT_DIR", &foreign.join(".git"));
        let resolved = base::resolve_head_lease_base(&target).unwrap();
        assert_eq!(
            resolved.base_sha, c0,
            "base is the attached repository's HEAD, not the foreign GIT_DIR's"
        );
        assert_eq!(
            resolved.git_common_dir,
            repo.join(".git").canonicalize().unwrap(),
            "git_common_dir is the attached repository's, not the foreign GIT_DIR"
        );
        let err = provision_workspace_worktree(&target, &pinned(&base)).unwrap_err();
        assert!(
            err.to_string().contains(&c1),
            "still refused on the worktree's own HEAD: {err}"
        );
    }
}

/// Review 1 F2 — a repository whose absolute path contains whitespace: the
/// common-dir reader must hand the path through as git printed it (only the
/// line terminator removed), so prepare resolves and provisioning pins.
#[test]
fn lease_base_resolves_in_repo_path_with_spaces() {
    let tmp = tempfile::Builder::new()
        .prefix("neige lease ")
        .tempdir()
        .unwrap();
    let repo = tmp.path().join("my repo");
    init_git_repo(&repo);
    assert!(
        repo.to_str().unwrap().contains(' '),
        "test setup has a space"
    );
    let target = WorkspaceLeaseTarget {
        repo_root: repo.clone(),
        path: repo.join(".claude/worktrees/track-space/card-space"),
        branch: workspace_slice_branch_for("track-space", "card-space").unwrap(),
    };

    let base = base::resolve_head_lease_base(&target).unwrap();

    assert_eq!(
        base.git_common_dir,
        repo.join(".git").canonicalize().unwrap(),
        "git_common_dir is the repository's .git, spaces and all"
    );
    assert!(base.git_common_dir.to_str().unwrap().contains(' '));
    provision_workspace_worktree(&target, &pinned(&base)).unwrap();
    base::verify_worktree_base(&target, &pinned(&base)).unwrap();
    assert_eq!(
        git_stdout(&target.path, ["rev-parse", "HEAD"]).trim(),
        base.base_sha
    );
}

/// The shapes a hand-planted leaf symlink can take before the first
/// provisioning. Each is unlinked, never followed.
#[derive(Clone, Copy, Debug)]
enum LeafLinkTarget {
    NonEmptyDir,
    EmptyDir,
    File,
    Dangling,
}

/// Review 2 (B2-M3 / A2-m1) — before the first provisioning the lease leaf is
/// a symlink and nothing is registered there. Whatever it points at — a
/// non-empty directory, an empty one (the shape `worktree add` used to follow,
/// writing `.git` and the checkout into the external directory), a file, or
/// nothing — only the link is removed; `worktree add` then creates the real
/// directory and both post-add checks hold. The target is untouched.
#[test]
fn unregistered_leaf_symlink_is_unlinked_never_followed() {
    for shape in [
        LeafLinkTarget::NonEmptyDir,
        LeafLinkTarget::EmptyDir,
        LeafLinkTarget::File,
        LeafLinkTarget::Dangling,
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_git_repo(&repo);
        let external = tmp.path().join("external");
        let sentinel = external.join("SENTINEL.txt");
        match shape {
            LeafLinkTarget::NonEmptyDir => {
                std::fs::create_dir_all(&external).unwrap();
                std::fs::write(&sentinel, "external data\n").unwrap();
            }
            LeafLinkTarget::EmptyDir => std::fs::create_dir_all(&external).unwrap(),
            LeafLinkTarget::File => std::fs::write(&external, "a file, not a directory\n").unwrap(),
            LeafLinkTarget::Dangling => {}
        }
        let target = WorkspaceLeaseTarget {
            repo_root: repo.clone(),
            path: repo.join(".claude/worktrees/track-leaf/card-leaf"),
            branch: workspace_slice_branch_for("track-leaf", "card-leaf").unwrap(),
        };
        std::fs::create_dir_all(target.path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&external, &target.path).unwrap();
        let base = head_base(&target);
        assert_eq!(
            git_worktree_registration(&target).unwrap(),
            GitWorktreeRegistration::Absent,
            "{shape:?}: nothing is registered at the leaf"
        );

        provision_workspace_worktree(&target, &pinned(&base)).unwrap();

        match shape {
            LeafLinkTarget::NonEmptyDir => {
                assert_eq!(
                    std::fs::read_to_string(&sentinel).unwrap(),
                    "external data\n",
                    "{shape:?}: the sentinel is untouched"
                );
                assert_eq!(
                    std::fs::read_dir(&external).unwrap().count(),
                    1,
                    "{shape:?}: nothing was added to the external directory"
                );
            }
            LeafLinkTarget::EmptyDir => {
                assert_eq!(
                    std::fs::read_dir(&external).unwrap().count(),
                    0,
                    "{shape:?}: the external directory is still empty (no .git, no checkout)"
                );
            }
            LeafLinkTarget::File => {
                assert_eq!(
                    std::fs::read_to_string(&external).unwrap(),
                    "a file, not a directory\n",
                    "{shape:?}: the file target has the same content"
                );
            }
            LeafLinkTarget::Dangling => {
                assert!(
                    std::fs::symlink_metadata(&external).is_err(),
                    "{shape:?}: nothing was created at the dangling target"
                );
            }
        }
        assert!(
            !target
                .path
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
                && target.path.is_dir(),
            "{shape:?}: the lease path is now a real directory"
        );
        assert_eq!(
            git_stdout(&target.path, ["rev-parse", "HEAD"]).trim(),
            base.base_sha,
            "{shape:?}: HEAD is the recorded base"
        );
        assert_eq!(
            target.path.canonicalize().unwrap(),
            base.canonical_path,
            "{shape:?}: realpath is the recorded canonical path"
        );
    }
}

/// Review 2 (A2-m2) — a leaf symlink that resolves to a registered worktree
/// of the repository (here: the main checkout) reads as that registration
/// under the realpath match (`Foreign` since review 4: recorded under the
/// main checkout's path, not the lease's). It is refused in both arms —
/// `Pinned` and the pre-slice `LegacyUnpinned` recovery shape, which would
/// otherwise launch the worker inside the main checkout — and nothing is
/// touched: the link stays, the main checkout is unchanged.
#[test]
fn leaf_symlink_to_registered_worktree_refuses_both_arms() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);
    let target = WorkspaceLeaseTarget {
        repo_root: repo.clone(),
        path: repo.join(".claude/worktrees/track-root/card-root"),
        branch: workspace_slice_branch_for("track-root", "card-root").unwrap(),
    };
    let base = head_base(&target);
    std::os::unix::fs::symlink(&repo, &target.path).unwrap();
    let main_branch = git_stdout(&repo, ["symbolic-ref", "HEAD"])
        .trim()
        .to_string();
    assert_eq!(
        git_worktree_registration(&target).unwrap(),
        GitWorktreeRegistration::Foreign {
            registered_as: repo.canonicalize().unwrap(),
            branch: Some(main_branch)
        },
        "the leaf resolves to the main checkout's registration, recorded under its own path"
    );
    // Provisioning's first step writes `.claude/worktrees/` into
    // `info/exclude`; take the baseline after it so the comparison below
    // sees only what the refused arms did.
    ensure_workspace_worktree_root_excluded(&repo).unwrap();
    let listing_before = git_stdout(&repo, ["worktree", "list", "--porcelain"]);
    let status_before = git_stdout(&repo, ["status", "--porcelain"]);

    for arm in [pinned(&base), WorktreeBase::LegacyUnpinned] {
        let err = provision_workspace_worktree(&target, &arm).unwrap_err();
        assert!(matches!(err, CalmError::Internal(_)), "{arm:?}: {err}");
        let message = err.to_string();
        assert!(
            message.contains(target.path.to_str().unwrap())
                && message.contains(repo.canonicalize().unwrap().to_str().unwrap()),
            "{arm:?}: refusal names the lease path and where it resolves: {message}"
        );
        assert!(
            target
                .path
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink(),
            "{arm:?}: the link is left in place"
        );
        assert_eq!(
            git_stdout(&repo, ["worktree", "list", "--porcelain"]),
            listing_before,
            "{arm:?}: registrations unchanged"
        );
        assert_eq!(
            git_stdout(&repo, ["status", "--porcelain"]),
            status_before,
            "{arm:?}: the main checkout is unchanged"
        );
        assert!(
            !git_ref_exists(&repo, &format!("refs/heads/{}", target.branch)).unwrap(),
            "{arm:?}: no slice branch was created"
        );
    }
}

/// Review 2 (A2-m1) — `remove_workspace_worktree` called directly on a leaf
/// symlink unlinks the link and nothing else. Two shapes of the external
/// directory: unregistered (git's `worktree remove --force <leaf>` would
/// refuse it as "not a working tree", so the link alone is what there is to
/// remove), and registered as a worktree of the repository on the slice
/// branch (the residue the pre-fix provisioning left when `worktree add`
/// followed the link) — there `worktree remove --force <leaf>` follows the
/// link and deletes the external directory itself. The rule never runs it:
/// the link is unlinked, the external directory and its registration are
/// left alone, and `branch -D` is refused by git because the branch is
/// checked out there — reported as the error it is, not repaired by deleting
/// someone else's directory.
///
/// Since review 3 no production lease-aware entry reaches this path for a
/// link that resolves: the identity check runs first and refuses (the row
/// stays held, the link stays — `moved_worktree_behind_symlink_refuses_launch`
/// drives the compensation entry). The unlink is what a legacy row (no base),
/// a dangling link, or a sweep entry no row names gets.
#[test]
fn removal_of_symlink_leaf_unlinks_only() {
    for registered in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_git_repo(&repo);
        let external = tmp.path().join("external");
        let target = WorkspaceLeaseTarget {
            repo_root: repo.clone(),
            path: repo.join(".claude/worktrees/track-rm/card-rm"),
            branch: workspace_slice_branch_for("track-rm", "card-rm").unwrap(),
        };
        if registered {
            run_git(
                &repo,
                [
                    "worktree",
                    "add",
                    "-b",
                    &target.branch,
                    external.to_str().unwrap(),
                    "HEAD",
                ],
            );
        } else {
            std::fs::create_dir_all(&external).unwrap();
        }
        let sentinel = external.join("SENTINEL.txt");
        std::fs::write(&sentinel, "external data\n").unwrap();
        let entries_before = std::fs::read_dir(&external).unwrap().count();
        std::fs::create_dir_all(target.path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&external, &target.path).unwrap();
        let listing_before = git_stdout(&repo, ["worktree", "list", "--porcelain"]);
        if registered {
            assert!(
                listing_before.contains(external.canonicalize().unwrap().to_str().unwrap()),
                "test setup registered the external directory"
            );
        }

        let result = remove_workspace_worktree(&target);

        assert!(
            std::fs::symlink_metadata(&target.path).is_err(),
            "registered={registered}: the link is gone"
        );
        assert!(
            external.is_dir(),
            "registered={registered}: the external directory still exists"
        );
        assert_eq!(
            std::fs::read_to_string(&sentinel).unwrap(),
            "external data\n",
            "registered={registered}: the sentinel is untouched"
        );
        assert_eq!(
            std::fs::read_dir(&external).unwrap().count(),
            entries_before,
            "registered={registered}: nothing added or removed in the external directory"
        );
        assert_eq!(
            git_stdout(&repo, ["worktree", "list", "--porcelain"]),
            listing_before,
            "registered={registered}: registrations are untouched"
        );
        let branch_ref = format!("refs/heads/{}", target.branch);
        if registered {
            let err = result.expect_err("the branch is checked out in the external worktree");
            assert!(
                matches!(err, CalmError::Internal(_)) && err.to_string().contains("git branch -D"),
                "registered=true: the refused branch delete is the reported error: {err}"
            );
            assert!(git_ref_exists(&repo, &branch_ref).unwrap());
        } else {
            assert!(
                result.unwrap(),
                "registered=false: the link counted as removed"
            );
            assert!(!git_ref_exists(&repo, &branch_ref).unwrap());
        }
    }
}

/// Review 1 F4, re-pinned under review 2's rule — a provisioned worktree is
/// moved away and a symlink left at the lease path. The link resolves to the
/// registration's realpath (git lists the old path, which is now the link),
/// so the leaf is a symlink to a registered worktree: refused before any
/// arm runs, naming the lease path and where it resolves; the moved
/// directory is not touched. The production compensation entry
/// (`remove_workspace_artifact_for_lease_by_id`) refuses too: the row's
/// `canonical_path` is not where the link resolves, so the identity check
/// (review 3) returns `Err` before anything is unlinked — the link and the
/// moved directory stay, the row stays held. Only `remove_workspace_worktree`
/// called directly (the legacy-row / no-row path) unlinks the leaf, prunes
/// the registration git kept at the now-missing path and deletes the branch
/// — the moved directory survives that too.
#[tokio::test]
async fn moved_worktree_behind_symlink_refuses_launch() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);
    let (db, track_id, card_id) = lease_fixture(&repo).await;
    let mut tx = begin_immediate_tx(db.pool()).await.unwrap();
    let target = prepare_workspace_lease_target_tx(
        &mut tx,
        &track_id,
        &card_id,
        &std::env::temp_dir().join("neige-calm-test-unused-workspace-root"),
    )
    .await
    .unwrap();
    let base = head_base(&target);
    let (lease, _event) =
        acquire_workspace_lease_tx(&mut tx, &card_id, &track_id, "op-test", &target, &base)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    provision_workspace_worktree(&target, &pinned(&base)).unwrap();
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::rename(&target.path, &elsewhere).unwrap();
    let sentinel = elsewhere.join("UNCOMMITTED.txt");
    std::fs::write(&sentinel, "moved with the worktree\n").unwrap();
    std::os::unix::fs::symlink(&elsewhere, &target.path).unwrap();
    assert_eq!(
        git_worktree_registration(&target).unwrap(),
        GitWorktreeRegistration::Present,
        "the registration at the realpath is still found behind the leaf symlink"
    );

    let err = provision_workspace_worktree(&target, &pinned(&base)).unwrap_err();

    assert!(matches!(err, CalmError::Internal(_)), "{err}");
    let message = err.to_string();
    let found = elsewhere.canonicalize().unwrap();
    assert!(
        message.contains(target.path.to_str().unwrap())
            && message.contains(found.to_str().unwrap()),
        "refusal names the lease path and where it resolves: {message}"
    );
    assert!(sentinel.is_file(), "the moved worktree is untouched");
    assert!(
        target
            .path
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "the leaf symlink is untouched"
    );

    let events = EventBus::new();
    let err = remove_workspace_artifact_for_lease_by_id(db.pool(), &events, &lease.lease_id)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("refused: lease path")
            && err
                .to_string()
                .contains(base.canonical_path.to_str().unwrap())
            && err.to_string().contains(found.to_str().unwrap()),
        "compensation refuses on identity, naming expected and found: {err}"
    );
    assert!(
        target
            .path
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "compensation left the leaf symlink in place"
    );
    assert!(
        sentinel.is_file(),
        "compensation left the moved worktree alone"
    );
    let state: String =
        sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
            .bind(&lease.lease_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(state, "held", "a refused compensation releases nothing");
    assert_eq!(event_kind_count(&db, "worktree.removed").await, 0);

    assert!(remove_workspace_worktree(&target).unwrap());
    assert!(
        std::fs::symlink_metadata(&target.path).is_err(),
        "removal unlinked the leaf"
    );
    assert!(
        sentinel.is_file(),
        "removal did not follow the link into the moved worktree"
    );
    assert_eq!(
        git_worktree_registration(&target).unwrap(),
        GitWorktreeRegistration::Absent,
        "removal pruned the registration at the missing path"
    );
    assert!(
        !git_ref_exists(&repo, &format!("refs/heads/{}", target.branch)).unwrap(),
        "removal deleted the slice branch"
    );
}

/// The `canonical_path` half of `verify_worktree_base`, pinned by a shape a
/// symlink leaf cannot shadow and the identity check before provisioning does
/// not cover (it runs before the worktree exists; the post-add check is what
/// catches a parent retargeted between the two): under the retargeted
/// symlinked `.claude/worktrees` the lease path is a real directory whose
/// HEAD is the base — someone's worktree at the same SHA — so only the
/// realpath comparison can refuse, naming expected and found. Provisioning
/// as a whole refuses earlier, before anything is touched
/// (`retargeted_parent_refuses_before_any_destruction`).
#[test]
fn retargeted_parent_symlink_refuses_launch() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);
    let real_a = tmp.path().join("real-a");
    let real_b = tmp.path().join("real-b");
    std::fs::create_dir_all(&real_a).unwrap();
    std::fs::create_dir_all(&real_b).unwrap();
    std::fs::create_dir_all(repo.join(".claude")).unwrap();
    let link = repo.join(".claude/worktrees");
    std::os::unix::fs::symlink(&real_a, &link).unwrap();
    let target = WorkspaceLeaseTarget {
        repo_root: repo.clone(),
        path: repo.join(".claude/worktrees/track-rt/card-rt"),
        branch: workspace_slice_branch_for("track-rt", "card-rt").unwrap(),
    };
    let base = head_base(&target);
    assert_eq!(
        base.canonical_path,
        real_a.canonicalize().unwrap().join("track-rt/card-rt")
    );
    let foreign = real_b.join("track-rt/card-rt");
    run_git(
        &repo,
        [
            "worktree",
            "add",
            "--detach",
            foreign.to_str().unwrap(),
            &base.base_sha,
        ],
    );
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&real_b, &link).unwrap();
    let found = real_b.canonicalize().unwrap().join("track-rt/card-rt");

    let err = base::verify_worktree_base(&target, &pinned(&base)).unwrap_err();

    assert!(matches!(err, CalmError::Internal(_)), "{err}");
    let message = err.to_string();
    assert!(
        message.contains(base.canonical_path.to_str().unwrap())
            && message.contains(found.to_str().unwrap()),
        "refusal names expected and found paths: {message}"
    );
    assert_eq!(
        git_stdout(&target.path, ["rev-parse", "HEAD"]).trim(),
        base.base_sha,
        "HEAD is the base: only the path check refused"
    );
    let err = provision_workspace_worktree(&target, &pinned(&base)).unwrap_err();
    assert!(
        err.to_string().contains("refused: lease path")
            && err.to_string().contains(found.to_str().unwrap()),
        "provisioning refuses on identity before any arm runs: {err}"
    );
}

/// Review 3 (B3-M1) — identity before any destructive step. A lease is
/// frozen under symlinked parent A (its `canonical_path` resolves through
/// A); before the spawn the parent symlink is retargeted at B, where the
/// same `<track>/<card>` belongs to someone else: a registered worktree of
/// the repository at the same SHA with an uncommitted file, or an
/// unregistered non-empty directory. Provisioning refuses naming both paths
/// and touches nothing in B — the stale-directory cleanup that used to run
/// before the realpath check deleted the unregistered directory, and the
/// compensation that followed the refusal ran `worktree remove --force` on
/// the registered one. The production compensation entry
/// (`remove_workspace_artifact_for_lease_by_id`, the `remove_workspace_artifact`
/// step of both adapters) and the by-id release refuse the same way: B's
/// directory, its file and its registration are exactly as they were, the
/// row stays held, no `worktree.removed` is appended.
#[tokio::test]
async fn retargeted_parent_refuses_before_any_destruction() {
    for registered in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_git_repo(&repo);
        let (db, track_id, card_id) = lease_fixture(&repo).await;
        let real_a = tmp.path().join("real-a");
        let real_b = tmp.path().join("real-b");
        std::fs::create_dir_all(&real_a).unwrap();
        std::fs::create_dir_all(&real_b).unwrap();
        std::fs::create_dir_all(repo.join(".claude")).unwrap();
        let link = repo.join(".claude/worktrees");
        std::os::unix::fs::symlink(&real_a, &link).unwrap();

        let mut tx = begin_immediate_tx(db.pool()).await.unwrap();
        let target = prepare_workspace_lease_target_tx(
            &mut tx,
            &track_id,
            &card_id,
            &std::env::temp_dir().join("neige-calm-test-unused-workspace-root"),
        )
        .await
        .unwrap();
        let base = head_base(&target);
        let (lease, _event) =
            acquire_workspace_lease_tx(&mut tx, &card_id, &track_id, "op-test", &target, &base)
                .await
                .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            base.canonical_path,
            real_a
                .canonicalize()
                .unwrap()
                .join(&track_id)
                .join(&card_id),
            "registered={registered}: the frozen identity resolves through A"
        );

        let foreign = real_b.join(&track_id).join(&card_id);
        if registered {
            run_git(
                &repo,
                [
                    "worktree",
                    "add",
                    "-b",
                    "someone-elses-branch",
                    foreign.to_str().unwrap(),
                    &base.base_sha,
                ],
            );
        } else {
            std::fs::create_dir_all(&foreign).unwrap();
        }
        let sentinel = foreign.join("UNCOMMITTED.txt");
        std::fs::write(&sentinel, "someone else's uncommitted work\n").unwrap();
        let entries_before = std::fs::read_dir(&foreign).unwrap().count();
        let listing_before = git_stdout_bytes(&repo, ["worktree", "list", "--porcelain", "-z"]);
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&real_b, &link).unwrap();
        let found = real_b
            .canonicalize()
            .unwrap()
            .join(&track_id)
            .join(&card_id);
        let untouched = |step: &str| {
            assert_eq!(
                std::fs::read_to_string(&sentinel).unwrap(),
                "someone else's uncommitted work\n",
                "registered={registered}: after {step}: B's uncommitted file is untouched"
            );
            assert_eq!(
                std::fs::read_dir(&foreign).unwrap().count(),
                entries_before,
                "registered={registered}: after {step}: nothing added or removed in B's directory"
            );
            assert_eq!(
                git_stdout_bytes(&repo, ["worktree", "list", "--porcelain", "-z"]),
                listing_before,
                "registered={registered}: after {step}: registrations are untouched"
            );
        };
        let names_both = |message: &str| {
            message.contains("refused: lease path")
                && message.contains(base.canonical_path.to_str().unwrap())
                && message.contains(found.to_str().unwrap())
        };

        let err = provision_workspace_worktree(&target, &pinned(&base)).unwrap_err();

        assert!(matches!(err, CalmError::Internal(_)), "{err}");
        assert!(
            names_both(&err.to_string()),
            "registered={registered}: provisioning refusal names expected and found: {err}"
        );
        untouched("provisioning");
        assert!(
            !git_ref_exists(&repo, &format!("refs/heads/{}", target.branch)).unwrap(),
            "registered={registered}: no slice branch was created"
        );

        let events = EventBus::new();
        let err = remove_workspace_artifact_for_lease_by_id(db.pool(), &events, &lease.lease_id)
            .await
            .unwrap_err();
        assert!(
            names_both(&err.to_string()),
            "registered={registered}: compensation refusal names expected and found: {err}"
        );
        untouched("compensation");

        let err = release_workspace_lease_by_id(db.pool(), &events, &lease.lease_id)
            .await
            .unwrap_err();
        assert!(
            names_both(&err.to_string()),
            "registered={registered}: by-id release refusal names expected and found: {err}"
        );
        untouched("release");
        let state: String =
            sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
                .bind(&lease.lease_id)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(
            state, "held",
            "registered={registered}: a refused removal releases nothing"
        );
        assert_eq!(event_kind_count(&db, "worktree.removed").await, 0);
    }
}

/// Review 4 (B4-M1 / A4-m1) — track deletion under a retargeted parent. A
/// lease is frozen and provisioned under symlinked parent A, the track is
/// released (the sweep captures the rows), then `.claude/worktrees` is
/// retargeted at B, where the same track directory holds someone else's
/// worktrees: the same `<card>` registered on another branch, an unrelated
/// registered worktree `other` no row names (the per-entry identity check
/// has nothing to check it against), and an unregistered non-empty `plain`
/// whose slice branch exists. The sweep verifies the track root's identity
/// before enumerating — `canonicalize(track root)` is B's, the rows recorded
/// A's — and refuses as a whole: nothing in B is removed, no registration
/// changes, `plain`'s branch survives the branch sweep, `removed == 0`, no
/// `worktree.removed`.
#[tokio::test]
async fn retargeted_parent_refuses_track_sweep() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);
    let (db, track_id, card_id) = lease_fixture(&repo).await;
    let real_a = tmp.path().join("real-a");
    let real_b = tmp.path().join("real-b");
    std::fs::create_dir_all(&real_a).unwrap();
    std::fs::create_dir_all(&real_b).unwrap();
    std::fs::create_dir_all(repo.join(".claude")).unwrap();
    let link = repo.join(".claude/worktrees");
    std::os::unix::fs::symlink(&real_a, &link).unwrap();

    let mut tx = begin_immediate_tx(db.pool()).await.unwrap();
    let target = prepare_workspace_lease_target_tx(
        &mut tx,
        &track_id,
        &card_id,
        &std::env::temp_dir().join("neige-calm-test-unused-workspace-root"),
    )
    .await
    .unwrap();
    let base = head_base(&target);
    let (_lease, _event) =
        acquire_workspace_lease_tx(&mut tx, &card_id, &track_id, "op-test", &target, &base)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    provision_workspace_worktree(&target, &pinned(&base)).unwrap();
    let ours = real_a.join(&track_id).join(&card_id).join("OURS.txt");
    std::fs::write(&ours, "the lease's own worktree under A\n").unwrap();

    let mut tx = begin_immediate_tx(db.pool()).await.unwrap();
    let release = release_workspace_leases_for_track_tx(&mut tx, &track_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let sweep = release.sweep.expect("track sweep plan");

    let track_b = real_b.join(&track_id);
    let same_card = track_b.join(&card_id);
    let other = track_b.join("other");
    let plain = track_b.join("plain");
    run_git(
        &repo,
        [
            "worktree",
            "add",
            "-b",
            "someone-elses-branch",
            same_card.to_str().unwrap(),
            &base.base_sha,
        ],
    );
    run_git(
        &repo,
        [
            "worktree",
            "add",
            "--detach",
            other.to_str().unwrap(),
            &base.base_sha,
        ],
    );
    std::fs::create_dir_all(&plain).unwrap();
    let plain_branch = workspace_slice_branch_for(&track_id, "plain").unwrap();
    run_git(&repo, ["branch", &plain_branch, &base.base_sha]);
    let sentinels = [
        same_card.join("UNCOMMITTED.txt"),
        other.join("UNCOMMITTED.txt"),
        plain.join("leftover.txt"),
    ];
    for sentinel in &sentinels {
        std::fs::write(sentinel, "someone else's uncommitted work\n").unwrap();
    }
    let entries_before: Vec<usize> = [&same_card, &other, &plain]
        .iter()
        .map(|dir| std::fs::read_dir(dir).unwrap().count())
        .collect();
    let listing_before = git_stdout_bytes(&repo, ["worktree", "list", "--porcelain", "-z"]);
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&real_b, &link).unwrap();
    assert_eq!(
        repo.join(".claude/worktrees")
            .join(&track_id)
            .canonicalize()
            .unwrap(),
        track_b.canonicalize().unwrap(),
        "test setup: the track root now resolves into B"
    );

    let events = EventBus::new();
    let removed = sweep_workspace_worktrees_for_track_repo(&db, &events, sweep)
        .await
        .unwrap();

    for sentinel in &sentinels {
        assert!(
            sentinel.is_file(),
            "{}: B's uncommitted file was deleted (removed = {removed})",
            sentinel.display()
        );
        assert_eq!(
            std::fs::read_to_string(sentinel).unwrap(),
            "someone else's uncommitted work\n",
            "{}: B's uncommitted file is untouched",
            sentinel.display()
        );
    }
    assert_eq!(removed, 0, "the sweep refused as a whole");
    let entries_after: Vec<usize> = [&same_card, &other, &plain]
        .iter()
        .map(|dir| std::fs::read_dir(dir).unwrap().count())
        .collect();
    assert_eq!(
        entries_after, entries_before,
        "nothing added or removed in B"
    );
    assert_eq!(
        git_stdout_bytes(&repo, ["worktree", "list", "--porcelain", "-z"]),
        listing_before,
        "registrations are untouched"
    );
    assert!(
        git_ref_exists(&repo, &format!("refs/heads/{plain_branch}")).unwrap(),
        "the branch sweep did not run: B's plain-dir slice branch survives"
    );
    assert!(
        git_ref_exists(&repo, "refs/heads/someone-elses-branch").unwrap()
            && git_ref_exists(&repo, &format!("refs/heads/{}", target.branch)).unwrap(),
        "no branch was deleted"
    );
    assert!(track_b.is_dir(), "B's track root was not removed");
    assert!(
        ours.is_file(),
        "the lease's own worktree under A is untouched"
    );
    assert_eq!(event_kind_count(&db, "worktree.removed").await, 0);
}

/// Review 4 (B4-M2) — lease A's worktree is moved to lease B's path and A's
/// path linked to it. Under the realpath match A's registration reads as one
/// at B's path, with B's base as its HEAD and B's `canonical_path` as its
/// realpath, so both post-add checks would pass and B would start its
/// worker in A's directory on A's branch. The registration's identity is the
/// path git recorded it under: the record is `Foreign` (recorded as
/// `<real>/t/a`, not `<real>/t/b`), provisioning refuses in both arms naming
/// both registrations and both branches, and removal refuses rather than
/// `worktree remove --force` through the alias (git resolves the alias and
/// deletes A's directory). A's directory and its uncommitted file are
/// exactly as they were.
#[test]
fn alias_of_another_lease_worktree_refuses_launch() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);
    let target_a = WorkspaceLeaseTarget {
        repo_root: repo.clone(),
        path: repo.join(".claude/worktrees/track-al/card-a"),
        branch: workspace_slice_branch_for("track-al", "card-a").unwrap(),
    };
    let target_b = WorkspaceLeaseTarget {
        repo_root: repo.clone(),
        path: repo.join(".claude/worktrees/track-al/card-b"),
        branch: workspace_slice_branch_for("track-al", "card-b").unwrap(),
    };
    let base_a = head_base(&target_a);
    provision_workspace_worktree(&target_a, &pinned(&base_a)).unwrap();
    let base_b = head_base(&target_b);
    assert_eq!(base_a.base_sha, base_b.base_sha, "test setup: same base");
    std::fs::rename(&target_a.path, &target_b.path).unwrap();
    std::os::unix::fs::symlink(&target_b.path, &target_a.path).unwrap();
    let sentinel = target_b.path.join("UNCOMMITTED.txt");
    std::fs::write(&sentinel, "A's uncommitted work\n").unwrap();
    let entries_before = std::fs::read_dir(&target_b.path).unwrap().count();
    let listing_before = git_stdout_bytes(&repo, ["worktree", "list", "--porcelain", "-z"]);
    assert_eq!(
        git_stdout(&target_b.path, ["rev-parse", "HEAD"]).trim(),
        base_b.base_sha,
        "test setup: the aliased worktree is at B's base"
    );
    assert_eq!(
        target_b.path.canonicalize().unwrap(),
        base_b.canonical_path,
        "test setup: the aliased worktree is at B's canonical path"
    );

    assert_eq!(
        git_worktree_registration(&target_b).unwrap(),
        GitWorktreeRegistration::Foreign {
            registered_as: base_a.canonical_path.clone(),
            branch: Some(format!("refs/heads/{}", target_a.branch))
        },
        "the record at B's realpath is A's, identified by the path it was recorded under"
    );
    let untouched = |step: &str| {
        assert_eq!(
            std::fs::read_to_string(&sentinel).unwrap(),
            "A's uncommitted work\n",
            "after {step}: A's uncommitted file is untouched"
        );
        assert_eq!(
            std::fs::read_dir(&target_b.path).unwrap().count(),
            entries_before,
            "after {step}: nothing added or removed in A's directory"
        );
        assert_eq!(
            git_stdout_bytes(&repo, ["worktree", "list", "--porcelain", "-z"]),
            listing_before,
            "after {step}: registrations are untouched"
        );
        assert!(
            !git_ref_exists(&repo, &format!("refs/heads/{}", target_b.branch)).unwrap(),
            "after {step}: no slice branch for B was created"
        );
    };
    for arm in [pinned(&base_b), WorktreeBase::LegacyUnpinned] {
        let err = provision_workspace_worktree(&target_b, &arm).unwrap_err();
        assert!(matches!(err, CalmError::Internal(_)), "{arm:?}: {err}");
        let message = err.to_string();
        assert!(
            message.contains("registered as")
                && message.contains(base_a.canonical_path.to_str().unwrap())
                && message.contains(base_b.canonical_path.to_str().unwrap())
                && message.contains(&target_a.branch)
                && message.contains(&target_b.branch),
            "{arm:?}: refusal names both registrations and both branches: {message}"
        );
        untouched("provisioning");
    }

    let err = remove_workspace_worktree(&target_b).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("registered as")
            && message.contains(base_a.canonical_path.to_str().unwrap())
            && message.contains(&target_a.branch)
            && message.contains(&target_b.branch),
        "removal refuses through the alias, naming both branches: {message}"
    );
    untouched("removal");
}

/// Review 4 (B4-M2), the direct shape — a worktree registered at the lease
/// path, at the base commit and the recorded realpath, that someone checked
/// out onto another branch or detached before the worker ever ran in it.
/// `verify_worktree_base` refuses on its third check (`symbolic-ref HEAD`),
/// naming the branch found and the lease's; provisioning as a whole reads
/// the registration as `Present` (it is recorded under the lease's own
/// path) and refuses in that arm's `verify_worktree_base`. The worktree is
/// not touched either way. Provisioning only ever meets a worktree no
/// worker has run in (both adapters provision before the worker exists and
/// skip it on recovery once it does), which is what makes the branch a
/// provisioning-time check and not a registration identity —
/// `worker_switched_branch_is_still_removed`.
#[test]
fn present_worktree_on_foreign_branch_refuses_launch() {
    for detached in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_git_repo(&repo);
        let target = WorkspaceLeaseTarget {
            repo_root: repo.clone(),
            path: repo.join(".claude/worktrees/track-fb/card-fb"),
            branch: workspace_slice_branch_for("track-fb", "card-fb").unwrap(),
        };
        let base = head_base(&target);
        provision_workspace_worktree(&target, &pinned(&base)).unwrap();
        let sentinel = target.path.join("UNCOMMITTED.txt");
        std::fs::write(&sentinel, "worker output not yet committed\n").unwrap();
        if detached {
            run_git(&target.path, ["checkout", "--detach"]);
        } else {
            run_git(&target.path, ["checkout", "-b", "other"]);
        }
        let found = if detached {
            "a detached HEAD".to_string()
        } else {
            "refs/heads/other".to_string()
        };
        assert_eq!(
            git_stdout(&target.path, ["rev-parse", "HEAD"]).trim(),
            base.base_sha,
            "detached={detached}: HEAD is still the base; only the branch differs"
        );

        let err = base::verify_worktree_base(&target, &pinned(&base)).unwrap_err();

        assert!(matches!(err, CalmError::Internal(_)), "{err}");
        let message = err.to_string();
        assert!(
            message.contains("is not on the lease branch")
                && message.contains(&format!("refs/heads/{}", target.branch))
                && message.contains(&found),
            "detached={detached}: refusal names expected and found: {message}"
        );
        assert_eq!(
            git_worktree_registration(&target).unwrap(),
            GitWorktreeRegistration::Present,
            "detached={detached}: the registration is the lease's own, whatever branch it is on"
        );
        let err = provision_workspace_worktree(&target, &pinned(&base)).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("is not on the lease branch")
                && message.contains(&target.branch)
                && message.contains(&found),
            "detached={detached}: provisioning refuses in verify_worktree_base: {message}"
        );
        assert_eq!(
            std::fs::read_to_string(&sentinel).unwrap(),
            "worker output not yet committed\n",
            "detached={detached}: the worktree is untouched"
        );
    }
}

/// Review 4, the case that rules out "registration identity = branch": a
/// worker delivers on a branch of its own — `git checkout -b slice-…`,
/// commit, push — inside the worktree the kernel provisioned for it (the
/// forge e2e `fu4_teardown_releases_after_merge_close_and_fences_in_flight_forge_op`
/// does exactly this and then deletes the track). At release and teardown
/// that worktree is still the lease's: the registration is recorded under
/// the lease's path, so it reads `Present` and is removed — the checkout,
/// its registration and the slice branch are gone, whatever branch the
/// worker left it on.
#[test]
fn worker_switched_branch_is_still_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);
    let target = WorkspaceLeaseTarget {
        repo_root: repo.clone(),
        path: repo.join(".claude/worktrees/track-sw/card-sw"),
        branch: workspace_slice_branch_for("track-sw", "card-sw").unwrap(),
    };
    let base = head_base(&target);
    provision_workspace_worktree(&target, &pinned(&base)).unwrap();
    run_git(&target.path, ["checkout", "-b", "slice-42"]);
    std::fs::write(target.path.join("delivered.txt"), "delivered\n").unwrap();
    run_git(&target.path, ["add", "delivered.txt"]);
    run_git(
        &target.path,
        ["commit", "-m", "delivered on the worker's own branch"],
    );
    assert_eq!(
        git_stdout(&target.path, ["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/slice-42",
        "test setup: the worker left its worktree on its own branch"
    );

    assert_eq!(
        git_worktree_registration(&target).unwrap(),
        GitWorktreeRegistration::Present,
        "the worker's worktree is still the lease's registration"
    );
    assert!(remove_workspace_worktree(&target).unwrap());

    assert!(!target.path.exists(), "the checkout is removed");
    assert_eq!(
        git_worktree_registration(&target).unwrap(),
        GitWorktreeRegistration::Absent,
        "the registration is removed"
    );
    assert!(
        !git_ref_exists(&repo, &format!("refs/heads/{}", target.branch)).unwrap(),
        "the slice branch is removed"
    );
    assert!(
        git_ref_exists(&repo, "refs/heads/slice-42").unwrap(),
        "the worker's delivery branch is not the kernel's to delete"
    );
}

/// Review 4 (B4-M3) — the repository's common dir path holds a byte that is
/// not UTF-8 (`wt-\xff`), and a sibling directory carrying the U+FFFD that a
/// lossy decode would turn it into exists at the same relative path. The
/// path reader keeps git's bytes, so `canonicalize` resolves the real common
/// dir and the UTF-8 rule then refuses it — the lease never records the
/// decoy directory.
#[test]
fn common_dir_bytes_are_not_decoded_before_canonicalize() {
    use std::os::unix::ffi::OsStringExt;
    let tmp = tempfile::tempdir().unwrap();
    let mut raw = tmp.path().as_os_str().to_os_string().into_vec();
    raw.extend_from_slice(b"/wt-\xff");
    let repo = PathBuf::from(std::ffi::OsString::from_vec(raw)).join("repo");
    init_git_repo(&repo);
    let decoy = tmp.path().join("wt-\u{FFFD}").join("repo").join(".git");
    std::fs::create_dir_all(&decoy).unwrap();
    assert_eq!(
        PathBuf::from(repo.join(".git").to_string_lossy().into_owned()),
        decoy,
        "test setup: a lossy decode of the common dir names the decoy"
    );
    assert!(decoy.is_dir(), "test setup: the decoy resolves");

    let err = base::lease_git_common_dir(&repo).unwrap_err();

    assert!(matches!(err, CalmError::Internal(_)), "{err}");
    assert!(
        err.to_string().contains("not UTF-8"),
        "the real common dir was resolved and refused for what it is: {err}"
    );
    assert_eq!(
        std::fs::read_dir(&decoy).unwrap().count(),
        0,
        "the decoy was never used"
    );
}

/// Review 4 (B4-M3) — names that end in `\r`. Git prints a path raw and
/// appends one `\n`; stripping `\r\n` would cut the name's last byte off.
/// Two readers, two shapes: the common dir itself ends in CR — a linked
/// worktree of a bare repository named `bare\r` is the track cwd, the one
/// layout whose `--git-common-dir` output ends in CR (a `gitdir:` file
/// cannot: git strips its trailing CR) — and the repository directory ends
/// in CR (`--show-toplevel`). The lease resolves and provisions like any
/// other in both.
#[test]
fn common_dir_ending_in_cr_resolves() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    init_git_repo(&source);
    let bare = tmp.path().join("bare\r");
    run_git(
        tmp.path(),
        [
            "clone",
            "--bare",
            source.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    let default_branch = git_stdout(&source, ["symbolic-ref", "--short", "HEAD"])
        .trim()
        .to_string();
    let linked = tmp.path().join("linked");
    run_git(
        &bare,
        ["worktree", "add", linked.to_str().unwrap(), &default_branch],
    );
    let target = WorkspaceLeaseTarget {
        repo_root: linked.clone(),
        path: linked.join(".claude/worktrees/track-cr/card-cr"),
        branch: workspace_slice_branch_for("track-cr", "card-cr").unwrap(),
    };

    let common_dir = base::lease_git_common_dir(&linked).unwrap();
    assert_eq!(
        common_dir,
        bare.canonicalize().unwrap(),
        "git_common_dir keeps the bare repository's trailing CR"
    );
    assert!(common_dir.to_str().unwrap().ends_with("bare\r"));
    let base = base::resolve_head_lease_base(&target).unwrap();
    assert_eq!(base.git_common_dir, common_dir);
    provision_workspace_worktree(&target, &pinned(&base)).unwrap();
    base::verify_worktree_base(&target, &pinned(&base)).unwrap();

    let repo = tmp.path().join("repo\r");
    init_git_repo(&repo);
    assert_eq!(
        git_repo_root_for_track_cwd("track-cr", repo.to_str().unwrap()).unwrap(),
        repo.canonicalize().unwrap(),
        "the --show-toplevel reader keeps the trailing CR"
    );
    assert_eq!(
        base::lease_git_common_dir(&repo).unwrap(),
        repo.join(".git").canonicalize().unwrap()
    );
}

/// Review 3 (B3-M2) — an attached repository whose path contains a newline
/// passes admission and `worktree add`, but `rev-parse --git-common-dir`
/// prints the newline literally, so its output spans two lines. The path
/// reader strips exactly the one terminator: the lease is prepared and
/// provisioned like any other and `git_common_dir` carries the newline.
#[test]
fn lease_base_resolves_in_repo_path_with_newline() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join(format!("repo{}newline", '\n'));
    init_git_repo(&repo);
    let target = WorkspaceLeaseTarget {
        repo_root: repo.clone(),
        path: repo.join(".claude/worktrees/track-nl/card-nl"),
        branch: workspace_slice_branch_for("track-nl", "card-nl").unwrap(),
    };

    let base = base::resolve_head_lease_base(&target).unwrap();

    assert_eq!(
        base.git_common_dir,
        repo.join(".git").canonicalize().unwrap(),
        "git_common_dir is the repository's .git, newline and all"
    );
    assert!(base.git_common_dir.to_str().unwrap().contains('\n'));
    assert_eq!(
        git_repo_root_for_track_cwd("track-nl", repo.to_str().unwrap()).unwrap(),
        repo.canonicalize().unwrap(),
        "the --show-toplevel reader keeps the newline too"
    );
    provision_workspace_worktree(&target, &pinned(&base)).unwrap();
    base::verify_worktree_base(&target, &pinned(&base)).unwrap();
    assert_eq!(
        git_stdout(&target.path, ["rev-parse", "HEAD"]).trim(),
        base.base_sha
    );
}

/// Review 3 (A3-m2) — the repository has a user worktree at a path that is
/// not UTF-8 (`wt-\xff`; git prints the byte raw in `-z` output). It is
/// someone else's record: the registration parser compares bytes and only a
/// record without a `worktree ` line is a parse failure, so the lease is
/// provisioned twice (absent, then present) and removed, all `Ok`, and the
/// foreign worktree is untouched.
#[test]
fn foreign_non_utf8_worktree_record_does_not_fail_the_lease() {
    use std::os::unix::ffi::OsStringExt;
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);
    let mut foreign = tmp.path().as_os_str().to_os_string().into_vec();
    foreign.extend_from_slice(b"/wt-\xff");
    let foreign = PathBuf::from(std::ffi::OsString::from_vec(foreign));
    assert!(
        foreign.to_str().is_none(),
        "test setup: the path is not UTF-8"
    );
    let output = Command::new("git")
        .args(["worktree", "add", "--detach"])
        .arg(&foreign)
        .arg("HEAD")
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        git_stdout_bytes(&repo, ["worktree", "list", "--porcelain", "-z"]).contains(&0xff),
        "test setup: git lists the raw byte"
    );
    let target = WorkspaceLeaseTarget {
        repo_root: repo.clone(),
        path: repo.join(".claude/worktrees/track-u8/card-u8"),
        branch: workspace_slice_branch_for("track-u8", "card-u8").unwrap(),
    };
    let base = head_base(&target);

    assert_eq!(
        git_worktree_registration(&target).unwrap(),
        GitWorktreeRegistration::Absent,
        "the foreign record does not match and is not a parse failure"
    );
    provision_workspace_worktree(&target, &pinned(&base)).unwrap();
    assert_eq!(
        git_worktree_registration(&target).unwrap(),
        GitWorktreeRegistration::Present
    );
    provision_workspace_worktree(&target, &pinned(&base)).unwrap();
    assert!(remove_workspace_worktree(&target).unwrap());

    assert!(!target.path.exists());
    assert!(
        foreign.join("README.md").is_file(),
        "the foreign worktree is untouched"
    );
    assert!(
        git_stdout_bytes(&repo, ["worktree", "list", "--porcelain", "-z"]).contains(&0xff),
        "the foreign registration is still listed"
    );
}

/// Review 3 (A3-m3) — the repository is UTF-8 but `.claude/worktrees` is a
/// symlink into a directory that is not, so the canonical parent is not
/// UTF-8. `resolve_head_lease_base` refuses with an `Err` naming the path:
/// the prepare tx fails the op instead of panicking in `json!` (which would
/// leave it `Pending`, panicking again on every drive).
#[test]
fn non_utf8_canonical_parent_is_refused_not_a_panic() {
    use std::os::unix::ffi::OsStringExt;
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);
    let mut real = tmp.path().as_os_str().to_os_string().into_vec();
    real.extend_from_slice(b"/wt-\xff-real");
    let real = PathBuf::from(std::ffi::OsString::from_vec(real));
    std::fs::create_dir_all(&real).unwrap();
    std::fs::create_dir_all(repo.join(".claude")).unwrap();
    std::os::unix::fs::symlink(&real, repo.join(".claude/worktrees")).unwrap();
    let target = WorkspaceLeaseTarget {
        repo_root: repo.clone(),
        path: repo.join(".claude/worktrees/track-u8/card-u8"),
        branch: workspace_slice_branch_for("track-u8", "card-u8").unwrap(),
    };

    let err = base::resolve_head_lease_base(&target).unwrap_err();

    assert!(matches!(err, CalmError::Internal(_)), "{err}");
    assert!(
        err.to_string().contains("not UTF-8"),
        "the refusal names the cause: {err}"
    );
    assert!(
        !target.path.exists(),
        "nothing was provisioned behind the refused base"
    );
}

/// Review 5 (B5-M2 = A5-m1) — the layout where every reader except the
/// toplevel one is happy: the physical repository root is under a directory
/// that is not UTF-8 (`\xff-disk/proj`), its git dir is separate and UTF-8,
/// `.claude/worktrees` is a symlink to a UTF-8 directory, and the track cwd
/// is a UTF-8 symlink to the root. `--show-toplevel` prints the physical
/// path, so `repo_root` would be non-UTF-8 while `base_sha`, `git_common_dir`
/// and `canonical_path` all resolve — and the claude adapter's `json!` on
/// `repo_root` would then panic in `prepare_tx` (codex would write a lossy
/// lease `path`). Refused where the toplevel is read: `prepare_workspace_
/// lease_target_tx` is an `Err` naming the cause, no lease row exists.
#[tokio::test]
async fn non_utf8_toplevel_is_refused_at_target_preparation() {
    use std::os::unix::ffi::OsStringExt;
    let tmp = tempfile::tempdir().unwrap();
    let mut disk = tmp.path().as_os_str().to_os_string().into_vec();
    disk.extend_from_slice(b"/\xff-disk");
    let disk = PathBuf::from(std::ffi::OsString::from_vec(disk));
    let physical_root = disk.join("proj");
    std::fs::create_dir_all(&physical_root).unwrap();
    let gitdir = tmp.path().join("gitdir");
    let separate = format!("--separate-git-dir={}", gitdir.display());
    run_git(&physical_root, ["init", &separate]);
    run_git(
        &physical_root,
        ["config", "user.email", "lease@example.test"],
    );
    run_git(&physical_root, ["config", "user.name", "Lease Test"]);
    std::fs::write(physical_root.join("README.md"), "initial\n").unwrap();
    run_git(&physical_root, ["add", "README.md"]);
    run_git(&physical_root, ["commit", "-m", "initial"]);
    let worktrees = tmp.path().join("wt");
    std::fs::create_dir_all(&worktrees).unwrap();
    std::fs::create_dir_all(physical_root.join(".claude")).unwrap();
    std::os::unix::fs::symlink(&worktrees, physical_root.join(".claude/worktrees")).unwrap();
    let track_cwd = tmp.path().join("proj");
    std::os::unix::fs::symlink(&physical_root, &track_cwd).unwrap();
    assert!(
        git_stdout_bytes(&track_cwd, ["rev-parse", "--show-toplevel"]).contains(&0xff),
        "precondition: git prints the physical, non-UTF-8 toplevel for the UTF-8 cwd"
    );
    // The other two readers accept this layout: the refusal below can only
    // come from the toplevel reader.
    base::resolve_head_base(&physical_root).unwrap();
    assert_eq!(
        base::lease_git_common_dir(&physical_root).unwrap(),
        gitdir.canonicalize().unwrap()
    );
    let (repo, track_id, card_id) = lease_fixture(&track_cwd).await;

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let err = prepare_workspace_lease_target_tx(
        &mut tx,
        &track_id,
        &card_id,
        &std::env::temp_dir().join("neige-calm-test-unused-workspace-root"),
    )
    .await
    .unwrap_err();
    tx.commit().await.unwrap();

    assert!(matches!(err, CalmError::Internal(_)), "{err}");
    assert!(
        err.to_string().contains("not UTF-8"),
        "the refusal names the cause: {err}"
    );
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspace_leases")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(rows, 0, "no lease row behind a refused target");
    assert_eq!(
        std::fs::read_dir(&worktrees).unwrap().count(),
        0,
        "nothing was created under the worktree parent"
    );
}

/// A2b — the whole `{sha,NULL} × {head,commit,attempt,NULL,bogus} × {A,NULL}`
/// matrix against migration 0111's tuple CHECK (`canonical_path` and
/// `git_common_dir` follow `base_sha`'s nullness). Exactly four tuples land;
/// every other one is rejected by the CHECK itself, not by anything in Rust.
#[tokio::test]
async fn lease_check_rejects_every_invalid_tuple() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, track_id, card_id) = lease_fixture(tmp.path()).await;
    let mut accepted = Vec::new();
    let mut rejected = 0;
    for (index, (sha, source, attempt)) in [Some("sha"), None]
        .into_iter()
        .flat_map(|sha| {
            [
                Some("head"),
                Some("commit"),
                Some("attempt"),
                None,
                Some("bogus"),
            ]
            .into_iter()
            .flat_map(move |source| {
                [Some("A"), None]
                    .into_iter()
                    .map(move |attempt| (sha, source, attempt))
            })
        })
        .enumerate()
    {
        let (canonical_path, git_common_dir) = match sha {
            Some(_) => (Some("/cp"), Some("/gcd")),
            None => (None, None),
        };
        let result = sqlx::query(
            "INSERT INTO workspace_leases (lease_id, card_id, track_id, path, state, \
             lease_owner, lease_until_ms, boot_id, created_at_ms, updated_at_ms, \
             base_sha, base_source, base_attempt_id, canonical_path, git_common_dir) \
             VALUES (?1, ?2, ?3, ?4, 'released', 'op-test', NULL, NULL, 1, 1, \
             ?5, ?6, ?7, ?8, ?9)",
        )
        .bind(format!("lease-{index}"))
        .bind(&card_id)
        .bind(&track_id)
        .bind(format!("/tuple/{index}"))
        .bind(sha)
        .bind(source)
        .bind(attempt)
        .bind(canonical_path)
        .bind(git_common_dir)
        .execute(repo.pool())
        .await;
        match result {
            Ok(_) => accepted.push((sha, source, attempt)),
            Err(error) => {
                assert!(
                    error.to_string().contains("CHECK constraint failed"),
                    "({sha:?},{source:?},{attempt:?}) rejected by something other than the CHECK: {error}"
                );
                rejected += 1;
            }
        }
    }
    assert_eq!(
        accepted,
        vec![
            (Some("sha"), Some("head"), None),
            (Some("sha"), Some("commit"), None),
            (Some("sha"), Some("attempt"), Some("A")),
            (None, None, None),
        ],
        "exactly the four CHECK-accepted tuples land"
    );
    assert_eq!(rejected, 16);
    let landed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspace_leases")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(landed, 4);
}

async fn new_card(repo: &crate::db::sqlite::SqlxRepo, track_id: &str) -> String {
    crate::db::RepoSyncDomainRaw::card_create(
        repo,
        crate::model::NewCard {
            track_id: TrackId::from(track_id.to_string()),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: serde_json::Value::Null,
        },
    )
    .await
    .unwrap()
    .id
    .to_string()
}
