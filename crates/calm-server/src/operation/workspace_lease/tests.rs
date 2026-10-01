use std::path::PathBuf;
use std::process::Command;

use super::base::BaseSource;
use super::*;
use crate::db::sqlite::begin_immediate_tx;
use crate::event::EventBus;
use crate::git_candidate::delivery::AttemptOutcome;

#[test]
fn remove_workspace_dir_if_exists_treats_missing_as_success() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("already-gone");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::remove_dir_all(&path).unwrap();

    remove_workspace_dir_if_exists(path.to_str().unwrap()).unwrap();
}

#[tokio::test]
async fn acquire_plain_workspace_lease_creates_leaf_for_non_git_track_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, track_id, card_id) = lease_fixture(tmp.path()).await;
    let path = tmp.path().join("plain-lease");

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let (lease, _event) =
        acquire_plain_workspace_lease_tx(&mut tx, &card_id, &track_id, "op-test", &path)
            .await
            .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(lease.path, path.to_string_lossy().to_string());
    assert_eq!(lease.delivery_policy, None, "a plain lease is legacy");
    assert!(path.is_dir(), "plain lease acquisition creates the leaf");

    let events = EventBus::new();
    assert!(
        release_workspace_lease_for_card_repo(
            &repo,
            &events,
            &card_id,
            ReleaseDelivery::Commit(AttemptOutcome::Completed),
        )
        .await
        .unwrap()
    );
    assert!(path.exists(), "plain lease release preserves the leaf");
    let deliveries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_git_deliveries")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(deliveries, 0, "a plain lease has no kernel delivery");
}

/// A track worktree (#1830 S1) as `ensure_track_worktree` makes it: `<repo>/.claude/worktrees/
/// track-<id>` on `neige/track-<id>`, with `.claude/worktrees/` excluded in the repository.
fn add_track_worktree(repo: &Path, track_id: &str) -> WorkspaceLeaseTarget {
    ensure_workspace_worktree_root_excluded(repo).unwrap();
    let target = WorkspaceLeaseTarget {
        repo_root: repo.to_path_buf(),
        path: crate::db::sqlite::track_worktree_path_for(repo, track_id),
        branch: track_worktree::track_branch_for(track_id).unwrap(),
    };
    run_git(
        repo,
        [
            "worktree",
            "add",
            "-b",
            &target.branch,
            target.path.to_str().unwrap(),
            "HEAD",
        ],
    );
    target
}

#[test]
fn workspace_worktree_remove_deletes_branch_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    let target = add_track_worktree(tmp.path(), "a");
    assert!(target.path.is_dir(), "the worktree exists");

    assert!(remove_workspace_worktree(&target).unwrap());
    assert!(!target.path.exists(), "worktree path removed");
    assert!(
        !git_ref_exists(&target.repo_root, &format!("refs/heads/{}", target.branch)).unwrap(),
        "branch removed"
    );

    assert!(!remove_workspace_worktree(&target).unwrap());
}

/// #1792: the branch delete runs the repository's `reference-transaction` hook, so it runs
/// with the allowlisted environment. nextest gives this test its own process.
#[test]
fn workspace_worktree_remove_hook_sees_only_the_allowlisted_environment() {
    let _sentinel = EnvVar::set(ENV_SENTINEL, "server-secret");
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);
    let target = add_track_worktree(&repo, "hook");
    let probes = install_ref_hook_env_probe(&repo, tmp.path());

    remove_workspace_worktree(&target).unwrap();

    assert_env_allowlisted(&probes.join("delete.env"));
}

/// The parent variable the hook-environment tests plant; it must never reach repository code.
pub(super) const ENV_SENTINEL: &str = "NEIGE_LEASE_ENV_SENTINEL";

/// Sets a variable of this test process and restores the previous value on drop. Correct under
/// `cargo nextest`'s process-per-test model, the documented runner (CI and the local gate). Under a
/// shared-process runner it is not: two concurrent tests setting the same variable can see each
/// other's value and restore the wrong one.
pub(super) struct EnvVar(&'static str, Option<std::ffi::OsString>);

impl EnvVar {
    pub(super) fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        // SAFETY: one test, one process under nextest; no other thread of the test reads the
        // environment while it is written.
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

/// A `reference-transaction` hook in `repo` that appends its environment to `<dir>/delete.env`
/// for each ref deletion and `<dir>/update.env` for each other ref update; returns `dir`.
pub(super) fn install_ref_hook_env_probe(repo: &Path, dir: &Path) -> std::path::PathBuf {
    let hook = repo.join(".git/hooks/reference-transaction");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    let zero = "0".repeat(40);
    let body = format!(
        "#!/bin/sh\nwhile read -r old new ref; do\n  kind=update\n  [ \"$new\" = {zero} ] && kind=delete\n  env >> '{}'/\"$kind.env\"\ndone\n",
        dir.display()
    );
    std::fs::write(&hook, body).unwrap();
    let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&hook, permissions).unwrap();
    dir.to_path_buf()
}

/// The probe at `path` was written, and by a process that saw `PATH` but not [`ENV_SENTINEL`].
pub(super) fn assert_env_allowlisted(path: &Path) {
    let seen = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("the hook wrote {}: {error}", path.display()));
    assert!(seen.contains("PATH="), "{seen}");
    assert!(!seen.contains(ENV_SENTINEL), "leaked: {seen}");
}

/// The `.claude/worktrees/` exclude lands in the common dir's `info/exclude` for a linked
/// checkout too, exactly once.
#[test]
fn worktree_root_exclude_resolves_for_a_linked_checkout() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("primary");
    init_git_repo(&primary);
    let linked = tmp.path().join("linked-track");
    run_git(
        &primary,
        [
            "worktree",
            "add",
            "-b",
            "linked-track",
            linked.to_str().unwrap(),
        ],
    );
    assert!(
        linked.join(".git").is_file(),
        "linked worktree .git is a gitdir file"
    );

    ensure_workspace_worktree_root_excluded(&linked).unwrap();
    ensure_workspace_worktree_root_excluded(&linked).unwrap();

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
        "the exclude entry is written once"
    );
}

/// An attached track with a track worktree, the fixture every prepare test below starts from.
async fn worktree_track_fixture(
    tmp: &Path,
) -> (
    crate::db::sqlite::SqlxRepo,
    String,
    String,
    WorkspaceLeaseTarget,
) {
    let checkout = tmp.join("checkout");
    init_git_repo(&checkout);
    let (repo, track_id, card_id) = lease_fixture(&checkout).await;
    let path = crate::test_seams::attach_track_worktree_for_test(repo.pool(), &track_id, &checkout)
        .await
        .unwrap();
    let target = track_worktree::track_worktree_target(&track_id, path.to_str().unwrap()).unwrap();
    (repo, track_id, card_id, target)
}

fn unused_workspace_root() -> PathBuf {
    std::env::temp_dir().join("neige-calm-test-unused-workspace-root")
}

/// D1/D2: an attached track's worker lease is its track worktree, on its branch, with the
/// worktree's HEAD, realpath and common dir as the base; `workspace.leased` names the path.
#[tokio::test]
async fn prepare_takes_the_track_worktree_at_its_head() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, track_id, card_id, target) = worktree_track_fixture(tmp.path()).await;
    let head = git_stdout(&target.path, ["rev-parse", "HEAD"])
        .trim()
        .to_string();

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let plan = prepare_worker_lease_tx(&mut tx, &track_id, &unused_workspace_root())
        .await
        .unwrap();
    let (lease, event) = acquire_workspace_lease_tx(&mut tx, &card_id, &track_id, "op-test", &plan)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(plan.path, target.path);
    assert_eq!(plan.branch, format!("neige/track-{track_id}"));
    let base = lease.base.expect("a worker lease records its base");
    assert_eq!(base.base_sha, head);
    assert_eq!(base.base_source, BaseSource::Commit);
    assert_eq!(base.canonical_path, target.path.canonicalize().unwrap());
    assert_eq!(
        base.git_common_dir,
        target.repo_root.join(".git").canonicalize().unwrap()
    );
    assert_eq!(lease.delivery_policy, Some(DeliveryPolicy::Kernel));
    assert_eq!(lease.path, target.path.to_str().unwrap());
    assert!(matches!(event.event, Event::WorkspaceLeased { ref path, .. } if path == &lease.path));
    base::verify_worktree_base(
        &target.path,
        &plan.branch,
        &base.base_sha,
        &base.canonical_path,
    )
    .unwrap();
}

/// D1: an attached track without a worktree (the pre-#1830 shape) is refused and writes nothing.
#[tokio::test]
async fn prepare_refuses_an_attached_track_without_a_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    let (repo, track_id, _card_id) = lease_fixture(tmp.path()).await;

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let err = prepare_worker_lease_tx(&mut tx, &track_id, &unused_workspace_root())
        .await
        .unwrap_err();
    tx.rollback().await.unwrap();

    let CalmError::Conflict(message) = err else {
        panic!("a refusal is a Conflict: {err}");
    };
    assert!(
        message.starts_with(
            "refused: track-without-worktree: this track predates per-track worktrees"
        ),
        "{message}"
    );
}

/// D6: a tracked edit and an untracked file refuse the worker and are listed, an ignored file is
/// not; `status.showUntrackedFiles=no` does not hide the untracked one (`git add -A` would commit
/// it). A clean tree passes.
#[tokio::test]
async fn clean_check_lists_untracked_files_whatever_show_untracked_files_says() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);
    worker::ensure_clean_tree(&repo).await.unwrap();

    run_git(&repo, ["config", "status.showUntrackedFiles", "no"]);
    std::fs::write(repo.join(".gitignore"), "ignored.log\n").unwrap();
    run_git(&repo, ["add", ".gitignore"]);
    run_git(&repo, ["commit", "-m", "ignore"]);
    std::fs::write(repo.join("README.md"), "edited\n").unwrap();
    std::fs::write(repo.join("untracked.txt"), "new\n").unwrap();
    std::fs::write(repo.join("ignored.log"), "noise\n").unwrap();
    assert_eq!(
        git_stdout(&repo, ["status", "--porcelain"]),
        " M README.md\n",
        "test setup: plain `git status` hides the untracked file"
    );

    let err = worker::ensure_clean_tree(&repo).await.unwrap_err();

    let CalmError::Conflict(message) = err else {
        panic!("a refusal is a Conflict: {err}");
    };
    assert!(
        message.starts_with("refused: track-worktree-dirty: 2 uncommitted path(s)."),
        "{message}"
    );
    assert!(message.ends_with(": README.md, untracked.txt"), "{message}");
    assert!(!message.contains("ignored.log"), "{message}");
}

/// A git that cannot answer (the checkout is gone) is `track-worktree-unavailable`.
#[tokio::test]
async fn clean_check_of_a_missing_checkout_is_unavailable() {
    let tmp = tempfile::tempdir().unwrap();
    let err = worker::ensure_clean_tree(&tmp.path().join("gone"))
        .await
        .unwrap_err();
    let CalmError::Conflict(message) = err else {
        panic!("a refusal is a Conflict: {err}");
    };
    assert!(
        message.starts_with("refused: track-worktree-unavailable: "),
        "{message}"
    );
}

/// D7 supersede: a `held` lease at the checkout whose owner op is `stuck` is released by the next
/// prepare with no delivery row, so the next INSERT fits the one-active-lease-per-path index.
#[tokio::test]
async fn prepare_supersedes_a_stuck_owners_lease() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, track_id, card_id, _target) = worktree_track_fixture(tmp.path()).await;
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let plan = prepare_worker_lease_tx(&mut tx, &track_id, &unused_workspace_root())
        .await
        .unwrap();
    let (stuck, _) = acquire_workspace_lease_tx(&mut tx, &card_id, &track_id, "op-stuck", &plan)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::query(
        "INSERT INTO operations (id, operation_key, kind, idempotency_key, payload_hash, \
         target_type, target_id, target_json, payload_json, phase, created_at_ms, updated_at_ms) \
         VALUES ('op-stuck', 'k-stuck', 'codex-worker', NULL, 'h', 'card', NULL, '{}', '{}', \
         'stuck', 1, 1)",
    )
    .execute(repo.pool())
    .await
    .unwrap();

    let next_card = new_card(&repo, &track_id).await;
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let plan = prepare_worker_lease_tx(&mut tx, &track_id, &unused_workspace_root())
        .await
        .unwrap();
    assert_eq!(
        plan.superseded.len(),
        1,
        "the stuck owner's lease is released"
    );
    acquire_workspace_lease_tx(&mut tx, &next_card, &track_id, "op-next", &plan)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let state: String =
        sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
            .bind(&stuck.lease_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(state, "released");
    let deliveries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_git_deliveries")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(deliveries, 0, "a superseded lease writes no delivery row");
}

/// Every reader of a lease row takes the base columns by name through `WORKSPACE_LEASE_COLUMNS`
/// and `row_to_workspace_lease`: a SELECT that fell back to a shorter list would fail here at run
/// time (`ColumnNotFound`). One worker lease at a time (one active lease per path).
#[tokio::test]
async fn every_lease_reader_returns_base_and_policy() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, track_id, first_card, target) = worktree_track_fixture(tmp.path()).await;
    let events = EventBus::new();
    let head = git_stdout(&target.path, ["rev-parse", "HEAD"])
        .trim()
        .to_string();
    let take = |card_id: String| {
        let repo = &repo;
        let track_id = &track_id;
        async move {
            let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
            let plan = prepare_worker_lease_tx(&mut tx, track_id, &unused_workspace_root())
                .await
                .unwrap();
            let (lease, _event) =
                acquire_workspace_lease_tx(&mut tx, &card_id, track_id, "op-test", &plan)
                    .await
                    .unwrap();
            tx.commit().await.unwrap();
            assert_eq!(lease.base.as_ref(), Some(&plan.base));
            assert_eq!(lease.delivery_policy, Some(DeliveryPolicy::Kernel));
            lease
        }
    };

    // The card-repo release.
    take(first_card.clone()).await;
    assert!(
        release_workspace_lease_for_card_repo(
            &repo,
            &events,
            &first_card,
            ReleaseDelivery::Commit(AttemptOutcome::Completed),
        )
        .await
        .unwrap()
    );
    // The card-tx release.
    let card_tx = new_card(&repo, &track_id).await;
    take(card_tx.clone()).await;
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let released = release_workspace_lease_for_card_tx(
        &mut tx,
        &card_tx,
        ReleaseDelivery::Commit(AttemptOutcome::Failed),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(released.len(), 1, "card-tx release decodes the based row");
    // By id (the compensation step), and by id in any state (the delivery hand-off's reader).
    let card_by_id = new_card(&repo, &track_id).await;
    let lease_by_id = take(card_by_id).await;
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let any_state = facts::workspace_lease_by_id_tx(&mut tx, &lease_by_id.lease_id)
        .await
        .unwrap()
        .expect("row by id in any state");
    tx.commit().await.unwrap();
    assert_eq!(any_state.delivery_policy, Some(DeliveryPolicy::Kernel));
    assert!(
        release::release_workspace_lease_by_id(
            repo.pool(),
            &events,
            &lease_by_id.lease_id,
            ReleaseDelivery::Commit(AttemptOutcome::SpawnFailed),
        )
        .await
        .unwrap()
    );
    // The facts reader.
    let card_facts = new_card(&repo, &track_id).await;
    take(card_facts.clone()).await;
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let facts = facts::worker_worktree_facts_tx(&mut tx, &card_facts)
        .await
        .unwrap()
        .expect("facts for a leased card");
    tx.commit().await.unwrap();
    assert_eq!(facts.base_sha.as_deref(), Some(head.as_str()));
    assert_eq!(
        facts.branch.as_deref(),
        Some(format!("neige/track-{track_id}").as_str())
    );
    // All active (boot reclaim's reader), then the track release.
    let all_active = active_workspace_leases(repo.pool()).await.unwrap();
    assert_eq!(all_active.len(), 1);
    assert!(all_active.iter().all(|lease| lease.base.is_some()));
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
    assert_eq!(
        sweep.git_common_dirs,
        vec![target.repo_root.join(".git").canonicalize().unwrap()]
    );
    assert_eq!(sweep.track_worktree.as_deref(), target.path.to_str());
}

/// The base is read with the allowlisted git environment: a hostile `GIT_DIR` in the kernel's
/// own environment names neither the base nor the common dir, and the spawn check still refuses
/// a checkout whose HEAD moved. nextest gives this test its own process.
#[test]
fn hostile_git_env_does_not_leak_into_lease_git() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);
    let target = add_track_worktree(&repo, "env");
    let base = worker::directory_base(&target.path).unwrap();
    let c0 = base.base_sha.clone();
    run_git(
        &target.path,
        ["commit", "--allow-empty", "-m", "C1 inside the worktree"],
    );
    let c1 = git_stdout(&target.path, ["rev-parse", "HEAD"])
        .trim()
        .to_string();
    assert_ne!(c1, c0, "test setup moved the worktree HEAD");
    let foreign = tmp.path().join("foreign");
    init_git_repo(&foreign);
    run_git(&foreign, ["commit", "--allow-empty", "-m", "foreign tip"]);

    let _git_dir = EnvVar::set("GIT_DIR", foreign.join(".git"));
    let resolved = worker::directory_base(&target.path).unwrap();
    assert_eq!(resolved.base_sha, c1, "the base is the checkout's own HEAD");
    assert_eq!(
        resolved.git_common_dir,
        repo.join(".git").canonicalize().unwrap(),
        "the common dir is the checkout's, not the foreign GIT_DIR's"
    );
    let err = base::verify_worktree_base(&target.path, &target.branch, &c0, &base.canonical_path)
        .unwrap_err();
    assert!(
        err.to_string().contains(&c0) && err.to_string().contains(&c1),
        "the spawn check names expected C0 and found C1: {err}"
    );
}

/// Whitespace, a newline, and a trailing CR in a checkout's path: every path reader hands the
/// path through as git printed it (only the one line terminator removed).
#[test]
fn lease_base_resolves_in_paths_git_prints_raw() {
    for name in ["my repo", "repo\nnewline", "repo\r"] {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join(name);
        init_git_repo(&repo);
        let base = worker::directory_base(&repo).unwrap();
        assert_eq!(
            base.git_common_dir,
            repo.join(".git").canonicalize().unwrap(),
            "{name:?}: git_common_dir is the repository's .git"
        );
        assert_eq!(
            base.canonical_path,
            repo.canonicalize().unwrap(),
            "{name:?}"
        );
        assert_eq!(
            git_repo_root_for_track_cwd("track-raw", repo.to_str().unwrap()).unwrap(),
            repo.canonicalize().unwrap(),
            "{name:?}: the --show-toplevel reader keeps the name"
        );
    }
}

/// The checkout resolves to a directory whose name is not UTF-8: refused with an `Err`, never a
/// panic in `json!` (which would leave the op `Pending`, panicking again on every drive).
#[test]
fn non_utf8_checkout_realpath_is_refused_not_a_panic() {
    use std::os::unix::ffi::OsStringExt;
    let tmp = tempfile::tempdir().unwrap();
    let mut real = tmp.path().as_os_str().to_os_string().into_vec();
    real.extend_from_slice(b"/wt-\xff-real");
    let real = PathBuf::from(std::ffi::OsString::from_vec(real));
    init_git_repo(&real);
    let checkout = tmp.path().join("checkout");
    std::os::unix::fs::symlink(&real, &checkout).unwrap();

    let err = worker::directory_base(&checkout).unwrap_err();

    assert!(matches!(err, CalmError::Internal(_)), "{err}");
    assert!(err.to_string().contains("not UTF-8"), "{err}");
}

/// G15 — a symlink leaf is unlinked, never followed: whatever it points at (an external
/// directory, registered or not) keeps every entry and registration it had.
#[test]
fn removal_of_symlink_leaf_unlinks_only() {
    for registered in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_git_repo(&repo);
        let external = tmp.path().join("external");
        let target = WorkspaceLeaseTarget {
            repo_root: repo.clone(),
            path: crate::db::sqlite::track_worktree_path_for(&repo, "rm"),
            branch: track_worktree::track_branch_for("rm").unwrap(),
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

        let result = remove_workspace_worktree(&target);

        assert!(
            std::fs::symlink_metadata(&target.path).is_err(),
            "registered={registered}: the link is gone"
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
            assert!(err.to_string().contains("git branch -D"), "{err}");
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

/// G15 — someone else's registration at the target's realpath (track A's worktree moved to track
/// B's path, a symlink left at A's) is refused rather than `worktree remove --force`d through the
/// alias; B's directory and its uncommitted file are exactly as they were.
#[test]
fn foreign_registration_at_the_target_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);
    let a = add_track_worktree(&repo, "al-a");
    let b_path = crate::db::sqlite::track_worktree_path_for(&repo, "al-b");
    std::fs::rename(&a.path, &b_path).unwrap();
    std::os::unix::fs::symlink(&b_path, &a.path).unwrap();
    std::fs::write(b_path.join("UNCOMMITTED.txt"), "A's uncommitted work\n").unwrap();
    let entries_before = std::fs::read_dir(&b_path).unwrap().count();
    let b = WorkspaceLeaseTarget {
        repo_root: repo.clone(),
        path: b_path.clone(),
        branch: track_worktree::track_branch_for("al-b").unwrap(),
    };
    assert!(matches!(
        git_worktree_registration(&b).unwrap(),
        GitWorktreeRegistration::Foreign { .. }
    ));

    let err = remove_workspace_worktree(&b).unwrap_err();

    assert!(
        err.to_string()
            .contains("resolves to a worktree registered as"),
        "{err}"
    );
    assert_eq!(std::fs::read_dir(&b_path).unwrap().count(), entries_before);
    assert!(b_path.join("UNCOMMITTED.txt").is_file());
}

/// A user worktree at a path that is not UTF-8 is someone else's record: the registration parser
/// compares bytes, so the target reads absent, then present once added, and is removed, all `Ok`,
/// with the foreign worktree untouched.
#[test]
fn foreign_non_utf8_worktree_record_does_not_fail_the_target() {
    use std::os::unix::ffi::OsStringExt;
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);
    let mut foreign = tmp.path().as_os_str().to_os_string().into_vec();
    foreign.extend_from_slice(b"/wt-\xff");
    let foreign = PathBuf::from(std::ffi::OsString::from_vec(foreign));
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
    let target = WorkspaceLeaseTarget {
        repo_root: repo.clone(),
        path: crate::db::sqlite::track_worktree_path_for(&repo, "u8"),
        branch: track_worktree::track_branch_for("u8").unwrap(),
    };

    assert_eq!(
        git_worktree_registration(&target).unwrap(),
        GitWorktreeRegistration::Absent
    );
    add_track_worktree(&repo, "u8");
    assert_eq!(
        git_worktree_registration(&target).unwrap(),
        GitWorktreeRegistration::Present
    );
    assert!(remove_workspace_worktree(&target).unwrap());
    assert!(!target.path.exists());
    assert!(
        foreign.join("README.md").is_file(),
        "the foreign worktree is untouched"
    );
}

/// The repository's common dir path holds a byte that is
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

/// The whole `{sha,NULL} × {head,upstream,commit,attempt,NULL,bogus} × {A,NULL}`
/// matrix against the tuple CHECK (0111, widened by 0115 to `upstream`;
/// `canonical_path` and `git_common_dir` follow `base_sha`'s nullness).
/// Exactly five tuples land; every other one is rejected by the CHECK
/// itself, not by anything in Rust.
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
                Some("upstream"),
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
            (Some("sha"), Some("upstream"), None),
            (Some("sha"), Some("commit"), None),
            (Some("sha"), Some("attempt"), Some("A")),
            (None, None, None),
        ],
        "exactly the five CHECK-accepted tuples land"
    );
    assert_eq!(rejected, 19);
    let landed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspace_leases")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(landed, 5);
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

#[path = "tests/native_reentry.rs"]
mod native_reentry;

#[path = "tests/disposal.rs"]
mod disposal;

#[path = "tests/read_configuration.rs"]
mod read_configuration;
