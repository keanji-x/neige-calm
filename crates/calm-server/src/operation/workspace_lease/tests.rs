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
    let (repo, wave_id, card_id) = lease_fixture(tmp.path()).await;

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let target = prepare_workspace_lease_target_tx(&mut tx, &wave_id, &card_id)
        .await
        .unwrap();
    assert!(target.repo_root.is_absolute());
    assert_eq!(
        target.repo_root.canonicalize().unwrap(),
        tmp.path().canonicalize().unwrap()
    );
    assert!(target.path.is_absolute());
    assert!(target.path.starts_with(&target.repo_root));

    let (lease, _event) =
        acquire_workspace_lease_tx(&mut tx, &card_id, &wave_id, "op-test", &target)
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
    let (repo, wave_id, card_id) = lease_fixture(tmp.path()).await;

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let target = prepare_workspace_lease_target_tx(&mut tx, &wave_id, &card_id)
        .await
        .unwrap();
    let (lease, leased) =
        acquire_workspace_lease_tx(&mut tx, &card_id, &wave_id, "op-test", &target)
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

    provision_workspace_worktree(&target).unwrap();
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let scope = workspace_scope_tx(&mut tx, &card_id, &wave_id)
        .await
        .unwrap();
    append_workspace_events_tx(
        &mut tx,
        vec![(
            ActorId::KernelDispatcher,
            scope,
            Event::WorktreeProvisioned {
                wave_id: WaveId::from(wave_id.clone()),
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
async fn workspace_lease_target_rejects_non_git_wave_cwd_without_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, wave_id, card_id) = lease_fixture(tmp.path()).await;

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let err = prepare_workspace_lease_target_tx(&mut tx, &wave_id, &card_id)
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
async fn acquire_plain_workspace_lease_creates_leaf_for_non_git_wave_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, wave_id, card_id) = lease_fixture(tmp.path()).await;
    let path = plain_workspace_lease_path_for(&wave_id, &card_id).unwrap();
    assert!(
        !path.is_absolute(),
        "plain workspace lease path is legacy-relative"
    );

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let (lease, _event) =
        acquire_plain_workspace_lease_tx(&mut tx, &card_id, &wave_id, "op-test", &path)
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
        path: tmp.path().join(".claude/worktrees/wave-a/card-a"),
        branch: workspace_slice_branch_for("wave-a", "card-a").unwrap(),
    };

    provision_workspace_worktree(&target).unwrap();
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
        path: tmp.path().join(".claude/worktrees/wave-clean/card-clean"),
        branch: workspace_slice_branch_for("wave-clean", "card-clean").unwrap(),
    };

    provision_workspace_worktree(&target).unwrap();

    let status = git_stdout(tmp.path(), ["status", "--short", "--untracked-files=all"]);
    assert_eq!(status, "", "base repo must stay clean after provisioning");

    provision_workspace_worktree(&target).unwrap();
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
        path: tmp.path().join(".claude/worktrees/wave-stale/card-stale"),
        branch: workspace_slice_branch_for("wave-stale", "card-stale").unwrap(),
    };

    provision_workspace_worktree(&target).unwrap();
    assert!(target.path.is_dir(), "initial worktree exists");
    std::fs::remove_dir_all(&target.path).unwrap();
    assert!(
        !target.path.exists(),
        "test setup leaves a registered but missing worktree path"
    );
    assert_ne!(
        git_worktree_registration(&target.repo_root, &target.path).unwrap(),
        GitWorktreeRegistration::Absent,
        "git still has a stale worktree registration"
    );

    provision_workspace_worktree(&target).unwrap();

    assert!(
        target.path.is_dir(),
        "stale registration is re-provisioned as a real worktree"
    );
    assert_eq!(
        git_worktree_registration(&target.repo_root, &target.path).unwrap(),
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
            .join(".claude/worktrees/wave-unregistered/card-unregistered"),
        branch: workspace_slice_branch_for("wave-unregistered", "card-unregistered").unwrap(),
    };
    std::fs::create_dir_all(&target.path).unwrap();
    std::fs::write(target.path.join("stale.txt"), "partial worktree add\n").unwrap();
    assert_eq!(
        git_worktree_registration(&target.repo_root, &target.path).unwrap(),
        GitWorktreeRegistration::Absent,
        "test setup leaves a non-empty directory without worktree registration"
    );

    provision_workspace_worktree(&target).unwrap();

    assert!(
        target.path.is_dir(),
        "stale unregistered directory is re-provisioned as a real worktree"
    );
    assert!(
        !target.path.join("stale.txt").exists(),
        "stale unregistered contents are cleared before git worktree add"
    );
    assert_eq!(
        git_worktree_registration(&target.repo_root, &target.path).unwrap(),
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
async fn workspace_worktree_provision_resolves_exclude_for_linked_wave_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("primary");
    init_git_repo(&primary);
    let linked = tmp.path().join("linked-wave");
    let linked_str = linked.to_str().unwrap();
    run_git(
        &primary,
        ["worktree", "add", "-b", "linked-wave", linked_str],
    );
    assert!(
        linked.join(".git").is_file(),
        "linked worktree .git is a gitdir file"
    );

    let (repo, wave_id, card_id) = lease_fixture(&linked).await;
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let target = prepare_workspace_lease_target_tx(&mut tx, &wave_id, &card_id)
        .await
        .unwrap();
    assert_eq!(
        target.repo_root.canonicalize().unwrap(),
        linked.canonicalize().unwrap()
    );
    let (_lease, _event) =
        acquire_workspace_lease_tx(&mut tx, &card_id, &wave_id, "op-test", &target)
            .await
            .unwrap();
    tx.commit().await.unwrap();

    provision_workspace_worktree(&target).unwrap();

    assert!(target.path.is_dir(), "provisioned worktree exists");
    assert_eq!(
        git_stdout(&linked, ["status", "--short", "--untracked-files=all"]),
        "",
        "linked wave worktree must stay clean after provisioning"
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

    provision_workspace_worktree(&target).unwrap();
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
    let (repo, wave_id, card_id) = lease_fixture(tmp.path()).await;

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let target = prepare_workspace_lease_target_tx(&mut tx, &wave_id, &card_id)
        .await
        .unwrap();
    let (lease, _event) =
        acquire_workspace_lease_tx(&mut tx, &card_id, &wave_id, "op-test", &target)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    provision_workspace_worktree(&target).unwrap();
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
    let (repo, wave_id, card_id) = lease_fixture(tmp.path()).await;

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let target = prepare_workspace_lease_target_tx(&mut tx, &wave_id, &card_id)
        .await
        .unwrap();
    let (lease, _event) =
        acquire_workspace_lease_tx(&mut tx, &card_id, &wave_id, "op-test", &target)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    provision_workspace_worktree(&target).unwrap();

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
    let (repo, wave_id, card_id) = lease_fixture(tmp.path()).await;

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let target = prepare_workspace_lease_target_tx(&mut tx, &wave_id, &card_id)
        .await
        .unwrap();
    let (lease, _event) =
        acquire_workspace_lease_tx(&mut tx, &card_id, &wave_id, "op-test", &target)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    provision_workspace_worktree(&target).unwrap();

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
async fn wave_release_sweeps_worktrees_plain_dirs_and_branches_post_commit() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    let (repo, wave_id, card_id) = lease_fixture(tmp.path()).await;

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let target = prepare_workspace_lease_target_tx(&mut tx, &wave_id, &card_id)
        .await
        .unwrap();
    let (_lease, _event) =
        acquire_workspace_lease_tx(&mut tx, &card_id, &wave_id, "op-test", &target)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    provision_workspace_worktree(&target).unwrap();

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
        .join(&wave_id)
        .join(plain_card_id);
    std::fs::create_dir_all(&plain_path).unwrap();
    std::fs::write(plain_path.join("leftover.txt"), "plain leftover\n").unwrap();
    let plain_branch = workspace_slice_branch_for(&wave_id, plain_card_id).unwrap();
    run_git(tmp.path(), ["branch", &plain_branch]);
    let branch_only = workspace_slice_branch_for(&wave_id, "branch-only").unwrap();
    run_git(tmp.path(), ["branch", &branch_only]);

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let release = release_workspace_leases_for_wave_tx(&mut tx, &wave_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert!(
        release.events.is_empty(),
        "released lease rows do not emit another workspace release"
    );
    let sweep = release.sweep.expect("wave sweep plan");
    assert_eq!(
        sweep_workspace_worktrees_for_wave_repo(&repo, &events, sweep.clone())
            .await
            .unwrap(),
        2
    );
    assert!(
        !target.path.exists(),
        "wave teardown sweeps preserved worktree paths"
    );
    assert!(
        !plain_path.exists(),
        "wave teardown sweeps leftover plain workspace dirs"
    );
    assert!(
        !git_ref_exists(&target.repo_root, &format!("refs/heads/{}", target.branch)).unwrap(),
        "wave teardown sweeps preserved slice branches"
    );
    assert!(
        !git_ref_exists(&target.repo_root, &format!("refs/heads/{plain_branch}")).unwrap(),
        "wave teardown sweeps plain-dir slice branches"
    );
    assert!(
        !git_ref_exists(&target.repo_root, &format!("refs/heads/{branch_only}")).unwrap(),
        "wave teardown sweeps branch-only slice branches"
    );
    assert_eq!(event_kind_count(&repo, "worktree.removed").await, 2);
    assert_eq!(
        sweep_workspace_worktrees_for_wave_repo(&repo, &events, sweep)
            .await
            .unwrap(),
        0,
        "wave sweep is idempotent after paths are gone"
    );
    assert_eq!(
        event_kind_count(&repo, "worktree.removed").await,
        2,
        "idempotent sweep emits no duplicate removal events"
    );
}

#[tokio::test]
async fn wave_sweep_uses_persisted_lease_paths_when_wave_cwd_is_deleted() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    let wave_cwd = tmp.path().join("deleted-wave-cwd");
    std::fs::create_dir_all(&wave_cwd).unwrap();
    let (repo, wave_id, card_id) = lease_fixture(&wave_cwd).await;

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let target = prepare_workspace_lease_target_tx(&mut tx, &wave_id, &card_id)
        .await
        .unwrap();
    let (_lease, _event) =
        acquire_workspace_lease_tx(&mut tx, &card_id, &wave_id, "op-test", &target)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    provision_workspace_worktree(&target).unwrap();
    assert!(target.path.is_dir(), "test setup provisioned worktree");

    let events = EventBus::new();
    release_workspace_lease_for_card_repo(&repo, &events, &card_id)
        .await
        .unwrap();
    std::fs::remove_dir_all(&wave_cwd).unwrap();
    assert!(
        git_repo_root_for_wave_cwd(&wave_id, wave_cwd.to_str().unwrap()).is_err(),
        "test setup leaves wave.cwd unusable for git -C"
    );

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let release = release_workspace_leases_for_wave_tx(&mut tx, &wave_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert!(
        release.events.is_empty(),
        "released lease rows do not emit another workspace release"
    );
    let sweep = release.sweep.expect("wave sweep plan");
    assert_eq!(
        sweep_workspace_worktrees_for_wave_repo(&repo, &events, sweep)
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

async fn lease_fixture(wave_cwd: &Path) -> (crate::db::sqlite::SqlxRepo, String, String) {
    let repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let cove = crate::db::RepoSyncDomainRaw::cove_create(
        &repo,
        crate::model::NewCove {
            name: "lease fixture".into(),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let wave = crate::db::RepoSyncDomainRaw::wave_create(
        &repo,
        crate::model::NewWave {
            workflow_input: None,
            cove_id: cove.id,
            title: "lease fixture".into(),
            sort: None,
            cwd: wave_cwd.display().to_string(),
            workflow_id: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        },
    )
    .await
    .unwrap();
    let card = crate::db::RepoSyncDomainRaw::card_create(
        &repo,
        crate::model::NewCard {
            wave_id: wave.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: serde_json::Value::Null,
        },
    )
    .await
    .unwrap();
    (repo, wave.id.to_string(), card.id.to_string())
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
    String::from_utf8_lossy(&output.stdout).to_string()
}
