//! Tests for the git candidate module (#1727 S4 slice 2 PR-B 1/2): the delivery scripts run
//! against real repositories, the two tables' CHECKs and triggers, and the pure derivations.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use calm_types::forge_git::{
    GIT_DELIVERY_OUTPUT_PROBE_SCRIPT, GIT_DELIVERY_PROBE_SCRIPT, GIT_DELIVERY_SCRIPT,
    GIT_LEASE_PROVENANCE_SCRIPT,
};
use calm_types::git_candidate::{DeliveryFailureCode, DeliveryWakeReason};
use calm_types::task_recovery::{TASK_CHILD_TRACK_ROUTE, TASK_IN_TRACK_ROUTE};
use serde_json::{Value, json};

use super::abandonment::{
    AbandonTaskOutcome, AbandonmentRow, abandonment_by_request_key_tx, abandonment_for_delivery_tx,
    insert_abandonment_tx,
};
use super::candidate::{CandidateRow, candidate_for_attempt_tx, from_operation_result};
use super::delivery::{
    DeliveryRow, DeliverySettled, FAILURE_EVIDENCE_MAX_LINE_BYTES, FAILURE_EVIDENCE_MAX_LINES,
    candidate_ref_name, classify_failure, delivery_argv, delivery_by_id_tx,
    delivery_by_request_key_tx, delivery_latest_for_attempt_tx, delivery_message,
    forge_payload_for, insert_initial_delivery_tx, insert_retry_delivery_tx, lease_for_delivery_tx,
    settle_candidate_tx, settle_failed_tx, unsettled_deliveries_for_track_tx,
    worktree_committed_delivery_fields,
};
use super::verification::{VerificationState, VerificationView};
use super::view::{
    AbandonmentFacts, BoundFacts, CandidateBinding, CandidateWorkspace, DeliveryFailure,
    DeliveryState, MISMATCH_ABANDONMENT_WITH_CANDIDATE, MISMATCH_CANDIDATE_ROW_MISSING,
    MISMATCH_CANDIDATE_WITH_FAILED_SETTLEMENT, MISMATCH_DELIVERY_ROW_MISSING, NoBindingReason,
    UnboundReason, candidate_binding, delivery_state,
};
use crate::db::sqlite::{SqlxRepo, begin_immediate_tx};
use crate::model::{Task, TaskKind, TaskStatus};
use crate::operation::forge_action_adapter::ForgeActionResultFile;
use crate::operation::workspace_lease::facts::WorkerWorktreeFacts;
use crate::operation::workspace_lease::{
    DeliveryPolicy, LeaseBase, WorkspaceLease, WorkspaceLeaseTarget, acquire_workspace_lease_tx,
    base, workspace_lease_path_for, workspace_slice_branch_for,
};
use crate::workspace_materialize::neige_git_command;

const TRACK: &str = "trk";
const CARD: &str = "crd";
const DELIVERY: &str = "dlv1";

// ---------------------------------------------------------------------------
// Script-level fixtures: a real repository, a lease worktree, the production argv.
// ---------------------------------------------------------------------------

fn git_output(dir: &Path, args: &[&str]) -> Output {
    neige_git_command()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git")
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = git_output(dir, args);
    assert!(
        output.status.success(),
        "git {args:?} failed in {}\nstdout:\n{}\nstderr:\n{}",
        dir.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn init_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "-q", "-b", "main"]);
    git(path, &["config", "user.email", "delivery@example.test"]);
    git(path, &["config", "user.name", "Delivery Test"]);
    std::fs::write(path.join("README.md"), "initial\n").unwrap();
    git(path, &["add", "README.md"]);
    git(path, &["commit", "-q", "-m", "initial"]);
}

/// The real `git` on PATH, for wrappers that forward to it.
fn real_git() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
        .expect("git on PATH")
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|e| panic!("canonicalize {}: {e}", path.display()))
}

struct ScriptRepo {
    _tmp: tempfile::TempDir,
    /// The directory the fixture treats as the Track cwd (the main repository, or a linked
    /// worktree of it for the linked-worktree fixture).
    track_root: PathBuf,
    /// The lease worktree, `<track_root>/.claude/worktrees/<TRACK>/<CARD>`.
    lease: PathBuf,
    canonical_path: PathBuf,
    common_dir: PathBuf,
    base_sha: String,
    branch: String,
    ref_name: String,
    /// A PATH prefix directory for `git` wrappers.
    bin: PathBuf,
}

impl ScriptRepo {
    /// A repository with two commits (`initial`, then `base` = C0), a lease worktree on the slice
    /// branch at C0, and the lease's `canonical_path` / `git_common_dir` as the lease row records
    /// them.
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        git(&repo, &["add", "base.txt"]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        Self::with_track_root(tmp, repo)
    }

    /// The Track cwd is itself a linked worktree of the main repository (A27).
    fn linked_worktree_track() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        init_repo(&main);
        std::fs::write(main.join("base.txt"), "base\n").unwrap();
        git(&main, &["add", "base.txt"]);
        git(&main, &["commit", "-q", "-m", "base"]);
        let track = tmp.path().join("track-wt");
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "track",
                track.to_str().unwrap(),
                "HEAD",
            ],
        );
        Self::with_track_root(tmp, track)
    }

    fn with_track_root(tmp: tempfile::TempDir, track_root: PathBuf) -> Self {
        let base_sha = git(&track_root, &["rev-parse", "HEAD"]);
        let branch = workspace_slice_branch_for(TRACK, CARD).unwrap();
        let lease = workspace_lease_path_for(&canonical(&track_root), TRACK, CARD).unwrap();
        std::fs::create_dir_all(lease.parent().unwrap()).unwrap();
        git(
            &track_root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &branch,
                lease.to_str().unwrap(),
                "HEAD",
            ],
        );
        let canonical_path = canonical(lease.parent().unwrap()).join(CARD);
        let common_dir = canonical(&PathBuf::from(git(
            &lease,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )));
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        ScriptRepo {
            _tmp: tmp,
            track_root,
            lease,
            canonical_path,
            common_dir,
            base_sha,
            branch,
            ref_name: candidate_ref_name(TRACK, CARD, DELIVERY),
            bin,
        }
    }

    fn argv(&self) -> Vec<String> {
        delivery_argv(
            &delivery_message(TRACK, CARD, DELIVERY),
            &self.branch,
            &self.ref_name,
            &self.base_sha,
            self.canonical_path.to_str().unwrap(),
            self.common_dir.to_str().unwrap(),
        )
    }

    /// Run `argv` in `cwd` the way the forge wrapper does: a cleared environment with PATH,
    /// HOME, LANG, LC_ALL and TERM only; `path` replaces PATH when given.
    fn run_in(&self, cwd: &Path, argv: &[String], path: Option<OsString>) -> Output {
        let mut command = Command::new(&argv[0]);
        command.args(&argv[1..]).current_dir(cwd).env_clear();
        for key in ["PATH", "HOME", "LANG", "LC_ALL", "TERM"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        if let Some(path) = path {
            command.env("PATH", path);
        }
        command.output().expect("spawn script")
    }

    fn run_delivery(&self) -> Output {
        self.run_in(&self.lease, &self.argv(), None)
    }

    fn run_delivery_with_path(&self, path: OsString) -> Output {
        self.run_in(&self.lease, &self.argv(), Some(path))
    }

    fn run_output_probe(&self) -> Output {
        let argv = vec![
            "sh".to_string(),
            "-c".into(),
            GIT_DELIVERY_OUTPUT_PROBE_SCRIPT.into(),
            "sh".into(),
            self.branch.clone(),
            self.ref_name.clone(),
            self.base_sha.clone(),
        ];
        self.run_in(&self.lease, &argv, None)
    }

    fn run_probe(&self, ref_name: &str) -> Output {
        let argv = vec![
            "sh".to_string(),
            "-c".into(),
            GIT_DELIVERY_PROBE_SCRIPT.into(),
            "sh".into(),
            ref_name.to_string(),
        ];
        self.run_in(&self.lease, &argv, None)
    }

    /// PATH with a `git` wrapper script in front of the real one.
    fn path_with_git_wrapper(&self, body: &str) -> OsString {
        let script = format!("#!/bin/sh\nREAL='{}'\n{body}\n", real_git().display());
        let wrapper = self.bin.join("git");
        std::fs::write(&wrapper, script).unwrap();
        let mut permissions = std::fs::metadata(&wrapper).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        std::fs::set_permissions(&wrapper, permissions).unwrap();
        let mut dirs = vec![self.bin.clone()];
        dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        std::env::join_paths(dirs).unwrap()
    }

    fn worker_edit(&self, name: &str, content: &str) {
        std::fs::write(self.lease.join(name), content).unwrap();
    }

    fn head(&self) -> String {
        git(&self.lease, &["rev-parse", "HEAD"])
    }

    fn ref_target(&self) -> Option<String> {
        let output = git_output(
            &self.lease,
            &[
                "rev-parse",
                "-q",
                "--verify",
                &format!("{}^{{commit}}", self.ref_name),
            ],
        );
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn commit_count(&self) -> usize {
        git(&self.lease, &["rev-list", "--count", "HEAD"])
            .parse()
            .unwrap()
    }

    fn status(&self) -> String {
        git(&self.lease, &["status", "--porcelain"])
    }

    fn pseudo_ref_exists(&self, name: &str) -> bool {
        git_output(&self.lease, &["rev-parse", "-q", "--verify", name])
            .status
            .success()
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn json_line(output: &Output) -> Value {
    serde_json::from_str(stdout(output).trim()).unwrap_or_else(|e| {
        panic!(
            "stdout is not one JSON line: {e}\nstdout:\n{}\nstderr:\n{}",
            stdout(output),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn assert_exit(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout:\n{}\nstderr:\n{}",
        stdout(output),
        String::from_utf8_lossy(&output.stderr)
    );
}

// ---------------------------------------------------------------------------
// Script-level acceptance (design §6 A4c, A25, A25b, A27, A28, A30; probe half of A4).
// ---------------------------------------------------------------------------

/// A4c — the live stdout and the recovered stdout are byte-equal even when a competing writer
/// moves the branch inside the script's observation window: a `git` wrapper on PATH pulls the
/// slice branch back to C0 right after the script's `update-ref`. The ref, not the branch tip,
/// is the candidate. The second fixture rebases the lease so the base is not an ancestor.
#[test]
fn live_and_recovery_outputs_are_byte_equal() {
    let repo = ScriptRepo::new();
    let c0 = repo.base_sha.clone();
    repo.worker_edit("worker.txt", "worker output\n");
    let wrapper = format!(
        "\"$REAL\" \"$@\"\nrc=$?\nif [ \"$1\" = update-ref ] && [ \"$2\" = '{}' ]; then \
         \"$REAL\" update-ref refs/heads/{} {c0}; fi\nexit $rc",
        repo.ref_name, repo.branch
    );
    let path = repo.path_with_git_wrapper(&wrapper);
    let live = repo.run_delivery_with_path(path);
    assert_exit(&live, 0);
    let live_json = json_line(&live);
    let c1 = live_json["commit"].as_str().unwrap().to_string();
    assert_ne!(c1, c0, "the worker's edit was committed");
    assert_eq!(live_json["base_is_ancestor"], json!(true));
    assert_eq!(live_json["delivery_id"], json!(DELIVERY));
    assert_eq!(
        git(
            &repo.lease,
            &["rev-parse", &format!("refs/heads/{}", repo.branch)]
        ),
        c0,
        "the competing writer pulled the branch back to C0 inside the window"
    );
    assert_eq!(repo.ref_target().as_deref(), Some(c1.as_str()));

    let recovery = repo.run_output_probe();
    assert_exit(&recovery, 0);
    assert_eq!(
        stdout(&live),
        stdout(&recovery),
        "live == recovery, byte for byte"
    );

    // Second fixture: the lease was rebased so the base is no longer an ancestor of the tip.
    let repo = ScriptRepo::new();
    let c0 = repo.base_sha.clone();
    let initial = git(&repo.lease, &["rev-parse", "HEAD~1"]);
    git(
        &repo.lease,
        &["commit", "-q", "--allow-empty", "-m", "worker"],
    );
    let other = repo.track_root.join("other-line.txt");
    std::fs::write(&other, "elsewhere\n").unwrap();
    git(
        &repo.track_root,
        &["checkout", "-q", "-b", "other", &initial],
    );
    git(&repo.track_root, &["add", "other-line.txt"]);
    git(&repo.track_root, &["commit", "-q", "-m", "other line"]);
    let new_base = git(&repo.track_root, &["rev-parse", "HEAD"]);
    git(&repo.lease, &["rebase", "-q", "--onto", &new_base, &c0]);
    assert!(
        !git_output(&repo.lease, &["merge-base", "--is-ancestor", &c0, "HEAD"])
            .status
            .success(),
        "test setup: the base is not an ancestor after the rebase"
    );
    repo.worker_edit("worker.txt", "worker output\n");
    let wrapper = format!(
        "\"$REAL\" \"$@\"\nrc=$?\nif [ \"$1\" = update-ref ] && [ \"$2\" = '{}' ]; then \
         \"$REAL\" update-ref refs/heads/{} {c0}; fi\nexit $rc",
        repo.ref_name, repo.branch
    );
    let path = repo.path_with_git_wrapper(&wrapper);
    let live = repo.run_delivery_with_path(path);
    assert_exit(&live, 0);
    assert_eq!(json_line(&live)["base_is_ancestor"], json!(false));
    let recovery = repo.run_output_probe();
    assert_exit(&recovery, 0);
    assert_eq!(stdout(&live), stdout(&recovery));
    assert_eq!(
        repo.ref_target().as_deref(),
        Some(json_line(&live)["commit"].as_str().unwrap())
    );
}

/// A25 (script level) — HEAD off the slice branch (switched or detached) is exit 11 with an
/// empty stdout: nothing staged, no commit, no ref. On the branch the same tree delivers, and
/// so does a branch shadowed by a tag of the same name: the check compares the full symbolic
/// ref, never the DWIM `--short` form (which prints `heads/<branch>` once the tag exists).
#[test]
fn delivery_script_refuses_switched_branch_and_detached_head() {
    let repo = ScriptRepo::new();
    let commits = repo.commit_count();
    repo.worker_edit("worker.txt", "switched\n");

    git(&repo.lease, &["checkout", "-q", "-b", "other"]);
    let switched = repo.run_delivery();
    assert_exit(&switched, 11);
    assert_eq!(stdout(&switched), "");
    assert_eq!(
        repo.commit_count(),
        commits,
        "no commit on a switched branch"
    );
    assert_eq!(repo.ref_target(), None);
    assert_eq!(repo.status(), "?? worker.txt", "nothing was staged");

    git(&repo.lease, &["checkout", "-q", &repo.branch.clone()]);
    git(&repo.lease, &["branch", "-q", "-D", "other"]);
    git(&repo.lease, &["checkout", "-q", "--detach"]);
    let detached = repo.run_delivery();
    assert_exit(&detached, 11);
    assert_eq!(stdout(&detached), "");
    assert_eq!(repo.commit_count(), commits);
    assert_eq!(repo.ref_target(), None);

    git(&repo.lease, &["checkout", "-q", &repo.branch.clone()]);
    let delivered = repo.run_delivery();
    assert_exit(&delivered, 0);
    assert_eq!(repo.commit_count(), commits + 1);
    assert_eq!(
        repo.ref_target().as_deref(),
        Some(json_line(&delivered)["commit"].as_str().unwrap())
    );

    // Positive: a tag named like the slice branch. `symbolic-ref --short` disambiguates to
    // `heads/<branch>` and would read as "switched"; the full ref is still the slice branch.
    let repo = ScriptRepo::new();
    let commits = repo.commit_count();
    git(&repo.lease, &["tag", &repo.branch.clone(), "HEAD"]);
    assert_eq!(
        git(&repo.lease, &["symbolic-ref", "--short", "-q", "HEAD"]),
        format!("heads/{}", repo.branch),
        "test setup: the tag makes the DWIM form ambiguous"
    );
    assert_eq!(
        git(&repo.lease, &["symbolic-ref", "-q", "HEAD"]),
        format!("refs/heads/{}", repo.branch)
    );
    repo.worker_edit("worker.txt", "tagged\n");
    let tagged = repo.run_delivery();
    assert_exit(&tagged, 0);
    assert_eq!(repo.commit_count(), commits + 1, "delivered on the branch");
    assert_eq!(
        repo.ref_target().as_deref(),
        Some(json_line(&tagged)["commit"].as_str().unwrap())
    );
    assert_eq!(json_line(&tagged)["branch"], json!(repo.branch));
}

/// A25b (script level) — an operation in progress is exit 15 with its evidence on stdout, before
/// anything is staged: a conflicted merge (paths), a clean `--no-ff --no-commit` merge
/// (`MERGE_HEAD`), a conflicted cherry-pick (paths, not `CHERRY_PICK_HEAD` — the index check
/// runs first), a cherry-pick resolved with `git add` but not continued (`CHERRY_PICK_HEAD`),
/// a conflicted rebase (paths; HEAD is detached, and the in-progress check runs before the
/// branch check so this is 15, not 11), the same rebase resolved but not continued
/// (`REBASE_HEAD`), a clean `revert --no-commit` (`REVERT_HEAD`), an interactive rebase paused
/// at `break` (HEAD detached, no pseudo-ref at all — only the `rebase-merge` directory), and a
/// `git am` whose conflict was `git add`ed but not `--continue`d (HEAD on the branch, clean
/// index — only the `rebase-apply` directory). The pseudo-ref check tests the worktree-private
/// files, so an ordinary branch named `MERGE_HEAD` delivers. After `git merge --abort` the same
/// lease delivers.
#[test]
fn delivery_script_refuses_in_progress_operations() {
    // A branch off the base that changes `base.txt` in a conflicting way, and one that only adds.
    let conflicting = |repo: &ScriptRepo| {
        git(
            &repo.track_root,
            &["checkout", "-q", "-b", "conflict", &repo.base_sha],
        );
        std::fs::write(repo.track_root.join("base.txt"), "theirs\n").unwrap();
        git(&repo.track_root, &["commit", "-q", "-am", "theirs"]);
        let sha = git(&repo.track_root, &["rev-parse", "HEAD"]);
        git(&repo.track_root, &["checkout", "-q", "main"]);
        std::fs::write(repo.lease.join("base.txt"), "ours\n").unwrap();
        git(&repo.lease, &["commit", "-q", "-am", "ours"]);
        sha
    };

    // 1. Conflicted merge.
    let repo = ScriptRepo::new();
    let theirs = conflicting(&repo);
    let commits = repo.commit_count();
    assert!(
        !git_output(&repo.lease, &["merge", "--no-commit", &theirs])
            .status
            .success(),
        "test setup: the merge conflicts"
    );
    let merge = repo.run_delivery();
    assert_exit(&merge, 15);
    assert!(
        stdout(&merge).contains("base.txt"),
        "stdout carries the unmerged path: {}",
        stdout(&merge)
    );
    assert!(
        repo.pseudo_ref_exists("MERGE_HEAD"),
        "MERGE_HEAD is untouched"
    );
    assert_eq!(repo.commit_count(), commits, "no merge commit was minted");
    assert_eq!(repo.ref_target(), None);
    let base_txt = std::fs::read_to_string(repo.lease.join("base.txt")).unwrap();
    assert!(
        base_txt.contains("<<<<<<<"),
        "conflict markers stay in the tree"
    );
    // Positive: abort the merge and deliver.
    git(&repo.lease, &["merge", "--abort"]);
    let delivered = repo.run_delivery();
    assert_exit(&delivered, 0);
    assert_eq!(
        repo.ref_target().as_deref(),
        Some(json_line(&delivered)["commit"].as_str().unwrap())
    );

    // 2. Clean `--no-ff --no-commit` merge: only MERGE_HEAD.
    let repo = ScriptRepo::new();
    git(
        &repo.track_root,
        &["checkout", "-q", "-b", "clean", &repo.base_sha],
    );
    std::fs::write(repo.track_root.join("added.txt"), "added\n").unwrap();
    git(&repo.track_root, &["add", "added.txt"]);
    git(&repo.track_root, &["commit", "-q", "-m", "added"]);
    let clean = git(&repo.track_root, &["rev-parse", "HEAD"]);
    git(&repo.track_root, &["checkout", "-q", "main"]);
    git(&repo.lease, &["merge", "--no-ff", "--no-commit", &clean]);
    let commits = repo.commit_count();
    let merge = repo.run_delivery();
    assert_exit(&merge, 15);
    assert_eq!(stdout(&merge), "MERGE_HEAD\n");
    assert!(repo.pseudo_ref_exists("MERGE_HEAD"));
    // The lease is a linked worktree: the file the check found lives in its private gitdir.
    let merge_head = PathBuf::from(git(
        &repo.lease,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "MERGE_HEAD",
        ],
    ));
    assert!(
        merge_head.starts_with(repo.common_dir.join("worktrees")) && merge_head.is_file(),
        "{}",
        merge_head.display()
    );
    assert_eq!(repo.commit_count(), commits);
    assert_eq!(repo.ref_target(), None);

    // 3. Conflicted cherry-pick: the unmerged entries, not CHERRY_PICK_HEAD.
    let repo = ScriptRepo::new();
    let theirs = conflicting(&repo);
    let commits = repo.commit_count();
    assert!(
        !git_output(&repo.lease, &["cherry-pick", &theirs])
            .status
            .success(),
        "test setup: the cherry-pick conflicts"
    );
    assert!(repo.pseudo_ref_exists("CHERRY_PICK_HEAD"));
    let pick = repo.run_delivery();
    assert_exit(&pick, 15);
    assert!(stdout(&pick).contains("base.txt"), "{}", stdout(&pick));
    assert!(
        !stdout(&pick).contains("CHERRY_PICK_HEAD"),
        "the index check exits first: {}",
        stdout(&pick)
    );
    assert_eq!(repo.commit_count(), commits);
    assert_eq!(repo.ref_target(), None);

    // 4. The same cherry-pick resolved with `git add` but not `--continue`.
    std::fs::write(repo.lease.join("base.txt"), "resolved\n").unwrap();
    git(&repo.lease, &["add", "base.txt"]);
    assert_eq!(git(&repo.lease, &["ls-files", "-u"]), "");
    assert!(repo.pseudo_ref_exists("CHERRY_PICK_HEAD"));
    let resolved = repo.run_delivery();
    assert_exit(&resolved, 15);
    assert_eq!(stdout(&resolved), "CHERRY_PICK_HEAD\n");
    assert_eq!(repo.commit_count(), commits);
    assert_eq!(repo.ref_target(), None);
    assert!(repo.pseudo_ref_exists("CHERRY_PICK_HEAD"));

    // 5. Positive: an ordinary branch named `MERGE_HEAD`. `rev-parse --verify MERGE_HEAD`
    // resolves it, the worktree-private file does not exist, nothing is in progress.
    let repo = ScriptRepo::new();
    let commits = repo.commit_count();
    git(&repo.lease, &["branch", "MERGE_HEAD", "HEAD"]);
    assert!(
        repo.pseudo_ref_exists("MERGE_HEAD"),
        "test setup: the branch resolves under rev-parse --verify"
    );
    repo.worker_edit("worker.txt", "branch named MERGE_HEAD\n");
    let delivered = repo.run_delivery();
    assert_exit(&delivered, 0);
    assert_eq!(repo.commit_count(), commits + 1);
    assert_eq!(
        repo.ref_target().as_deref(),
        Some(json_line(&delivered)["commit"].as_str().unwrap())
    );

    // 6. Conflicted rebase: HEAD is detached, REBASE_HEAD exists, the index has unmerged
    // entries. The in-progress check runs before the branch check: 15 with the paths, not 11.
    let repo = ScriptRepo::new();
    let theirs = conflicting(&repo);
    let commits = repo.commit_count();
    assert!(
        !git_output(&repo.lease, &["rebase", "-q", &theirs])
            .status
            .success(),
        "test setup: the rebase conflicts"
    );
    assert!(
        !git_output(&repo.lease, &["symbolic-ref", "-q", "HEAD"])
            .status
            .success(),
        "test setup: a conflicted rebase detaches HEAD"
    );
    assert!(repo.pseudo_ref_exists("REBASE_HEAD"));
    let detached_head = repo.head();
    let rebase = repo.run_delivery();
    assert_exit(&rebase, 15);
    assert!(stdout(&rebase).contains("base.txt"), "{}", stdout(&rebase));
    assert!(
        !stdout(&rebase).contains("REBASE_HEAD"),
        "the index check exits first: {}",
        stdout(&rebase)
    );
    assert_eq!(repo.head(), detached_head, "no commit");
    assert_eq!(repo.commit_count(), commits);
    assert_eq!(repo.ref_target(), None);
    assert!(repo.pseudo_ref_exists("REBASE_HEAD"));

    // 7. The same rebase resolved with `git add` but not `--continue`: only REBASE_HEAD.
    std::fs::write(repo.lease.join("base.txt"), "resolved\n").unwrap();
    git(&repo.lease, &["add", "base.txt"]);
    assert_eq!(git(&repo.lease, &["ls-files", "-u"]), "");
    let resolved = repo.run_delivery();
    assert_exit(&resolved, 15);
    assert_eq!(stdout(&resolved), "REBASE_HEAD\n");
    assert_eq!(repo.head(), detached_head, "no commit");
    assert_eq!(repo.ref_target(), None);
    assert!(repo.pseudo_ref_exists("REBASE_HEAD"));

    // 8. A clean `revert --no-commit`: only REVERT_HEAD.
    let repo = ScriptRepo::new();
    let commits = repo.commit_count();
    git(&repo.lease, &["revert", "--no-commit", "HEAD"]);
    assert_eq!(git(&repo.lease, &["ls-files", "-u"]), "");
    assert!(repo.pseudo_ref_exists("REVERT_HEAD"));
    let revert = repo.run_delivery();
    assert_exit(&revert, 15);
    assert_eq!(stdout(&revert), "REVERT_HEAD\n");
    assert_eq!(repo.commit_count(), commits);
    assert_eq!(repo.ref_target(), None);
    assert!(repo.pseudo_ref_exists("REVERT_HEAD"));

    // 9. An interactive rebase paused at a `break` todo line: HEAD is detached, the index is
    // clean and there is no `REBASE_HEAD` (nor any other pseudo-ref) — only the `rebase-merge`
    // directory in the worktree's private gitdir. Without the directory check the branch check
    // would read this as a branch switch (11).
    let repo = ScriptRepo::new();
    let theirs = conflicting(&repo);
    let commits = repo.commit_count();
    let head_before = repo.head();
    let rebase = neige_git_command()
        .args(["rebase", "-i", "-q", &theirs])
        .env("GIT_SEQUENCE_EDITOR", "sed -i '1i break'")
        .current_dir(&repo.lease)
        .output()
        .expect("spawn git");
    assert!(
        rebase.status.success(),
        "test setup: rebase -i stops at break\nstderr:\n{}",
        String::from_utf8_lossy(&rebase.stderr)
    );
    let rebase_merge = PathBuf::from(git(
        &repo.lease,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "rebase-merge",
        ],
    ));
    assert!(
        rebase_merge.starts_with(repo.common_dir.join("worktrees")) && rebase_merge.is_dir(),
        "test setup: the state directory lives in the linked worktree's private gitdir: {}",
        rebase_merge.display()
    );
    assert!(
        !git_output(&repo.lease, &["symbolic-ref", "-q", "HEAD"])
            .status
            .success(),
        "test setup: the paused rebase detaches HEAD"
    );
    assert_eq!(git(&repo.lease, &["ls-files", "-u"]), "");
    for pseudo_ref in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "REBASE_HEAD",
    ] {
        assert!(
            !repo.pseudo_ref_exists(pseudo_ref),
            "test setup: {pseudo_ref} does not exist at a break"
        );
    }
    let paused = repo.run_delivery();
    assert_exit(&paused, 15);
    assert_eq!(stdout(&paused), "rebase-merge\n");
    assert_ne!(
        repo.head(),
        head_before,
        "test setup: HEAD sits on the onto commit"
    );
    assert_eq!(
        git(&repo.lease, &["rev-list", "--count", &repo.branch.clone()]),
        commits.to_string(),
        "no commit on the branch"
    );
    assert_eq!(repo.ref_target(), None);
    assert!(rebase_merge.is_dir(), "the paused rebase is untouched");

    // 10. `git am -3` of a conflicting patch, resolved with `git add` but not `--continue`:
    // HEAD stays on the branch, the index is clean and no pseudo-ref exists — only the
    // `rebase-apply` directory. Without the directory check the script commits the resolution,
    // pins it as the candidate and leaves the worktree mid-`am`.
    let repo = ScriptRepo::new();
    let theirs = conflicting(&repo);
    let commits = repo.commit_count();
    let head_before = repo.head();
    let patches = repo.lease.parent().unwrap().join("patches");
    git(
        &repo.track_root,
        &[
            "format-patch",
            "-1",
            &theirs,
            "-q",
            "-o",
            patches.to_str().unwrap(),
        ],
    );
    let patch = std::fs::read_dir(&patches)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "patch"))
        .expect("one patch file");
    assert!(
        !git_output(&repo.lease, &["am", "-3", patch.to_str().unwrap()])
            .status
            .success(),
        "test setup: the patch conflicts"
    );
    assert_ne!(
        git(&repo.lease, &["ls-files", "-u"]),
        "",
        "test setup: the three-way fallback leaves unmerged entries"
    );
    std::fs::write(repo.lease.join("base.txt"), "resolved\n").unwrap();
    git(&repo.lease, &["add", "base.txt"]);
    assert_eq!(git(&repo.lease, &["ls-files", "-u"]), "");
    assert_eq!(
        git(&repo.lease, &["symbolic-ref", "-q", "HEAD"]),
        format!("refs/heads/{}", repo.branch),
        "test setup: `am` keeps HEAD on the branch"
    );
    for pseudo_ref in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "REBASE_HEAD",
    ] {
        assert!(
            !repo.pseudo_ref_exists(pseudo_ref),
            "test setup: {pseudo_ref} does not exist mid-am"
        );
    }
    let rebase_apply = PathBuf::from(git(
        &repo.lease,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "rebase-apply",
        ],
    ));
    assert!(
        rebase_apply.starts_with(repo.common_dir.join("worktrees")) && rebase_apply.is_dir(),
        "test setup: {}",
        rebase_apply.display()
    );
    let mid_am = repo.run_delivery();
    assert_exit(&mid_am, 15);
    assert_eq!(stdout(&mid_am), "rebase-apply\n");
    assert_eq!(repo.head(), head_before, "no commit");
    assert_eq!(repo.commit_count(), commits);
    assert_eq!(repo.ref_target(), None);
    assert!(rebase_apply.is_dir(), "the `am` is untouched");
    assert_eq!(
        git(&repo.lease, &["diff", "--cached", "--name-only"]),
        "base.txt",
        "the resolution stays staged, uncommitted"
    );
}

/// A27 (script level) — a Track whose cwd is itself a linked worktree delivers (the provenance
/// check compares the common dir, not `<repo_root>/.git`); a lease worktree moved away and
/// replaced by a symlink is exit 10 with the observation line on stdout.
#[test]
fn delivery_script_linked_worktree_track_and_moved_worktree() {
    let repo = ScriptRepo::linked_worktree_track();
    assert!(
        repo.track_root.join(".git").is_file(),
        "test setup: the Track cwd is a linked worktree"
    );
    repo.worker_edit("worker.txt", "linked\n");
    let delivered = repo.run_delivery();
    assert_exit(&delivered, 0);
    assert_eq!(
        repo.ref_target().as_deref(),
        Some(json_line(&delivered)["commit"].as_str().unwrap())
    );

    let repo = ScriptRepo::new();
    let moved = repo.track_root.join("moved-away");
    git(
        &repo.track_root,
        &[
            "worktree",
            "move",
            repo.lease.to_str().unwrap(),
            moved.to_str().unwrap(),
        ],
    );
    assert!(!repo.lease.exists());
    std::os::unix::fs::symlink(&moved, &repo.lease).unwrap();
    repo.worker_edit("worker.txt", "moved\n");
    let refused = repo.run_delivery();
    assert_exit(&refused, 10);
    let observation = stdout(&refused);
    assert!(
        observation.starts_with("provenance realpath=") && observation.contains(" registered="),
        "stdout carries the observation line: {observation}"
    );
    let realpath = observation
        .split_whitespace()
        .find_map(|word| word.strip_prefix("realpath="))
        .unwrap();
    assert_ne!(realpath, repo.canonical_path.to_str().unwrap());
    assert_eq!(realpath, canonical(&moved).to_str().unwrap());
    assert_eq!(repo.ref_target(), None);
    assert_eq!(
        git(&moved, &["status", "--porcelain"]),
        "?? worker.txt",
        "nothing was staged"
    );
}

/// A28 (script level) — a rebased lease (the base is no longer an ancestor of the tip) still
/// delivers; the observation is recorded as `base_is_ancestor:false`, never a refusal.
#[test]
fn delivery_script_records_base_not_ancestor_after_rebase() {
    let repo = ScriptRepo::new();
    repo.worker_edit("worker.txt", "plain\n");
    let plain = repo.run_delivery();
    assert_exit(&plain, 0);
    assert_eq!(json_line(&plain)["base_is_ancestor"], json!(true));

    let repo = ScriptRepo::new();
    let c0 = repo.base_sha.clone();
    let initial = git(&repo.lease, &["rev-parse", "HEAD~1"]);
    git(
        &repo.lease,
        &["commit", "-q", "--allow-empty", "-m", "worker"],
    );
    git(
        &repo.track_root,
        &["checkout", "-q", "-b", "other", &initial],
    );
    git(
        &repo.track_root,
        &["commit", "-q", "--allow-empty", "-m", "other line"],
    );
    let new_base = git(&repo.track_root, &["rev-parse", "HEAD"]);
    git(&repo.lease, &["rebase", "-q", "--onto", &new_base, &c0]);
    repo.worker_edit("worker.txt", "rebased\n");
    let rebased = repo.run_delivery();
    assert_exit(&rebased, 0);
    let line = json_line(&rebased);
    assert_eq!(line["base_is_ancestor"], json!(false));
    assert_eq!(
        repo.ref_target().as_deref(),
        Some(line["commit"].as_str().unwrap())
    );
}

/// A3b's second fixture (script level) — the worker changed a file, and a competing writer
/// pulls the slice branch back to C0 inside the `git commit` invocation (a PATH wrapper resets
/// the branch after forwarding the commit). The script captures `new` once, after the commit
/// returns: it reads C0, pins the ref at C0 and prints `commit == base`; the commit it made is
/// an orphan, not the candidate. Fed through the production event table,
/// `candidate::from_operation_result` and `view::delivery_state`, the delivery reads
/// `no_change` — OID equality, never "did the script commit".
#[test]
fn delivery_script_no_change_after_commit_reset() {
    let repo = ScriptRepo::new();
    let c0 = repo.base_sha.clone();
    let lease = WorkspaceLease {
        lease_id: "lease-1".into(),
        card_id: CARD.into(),
        track_id: TRACK.into(),
        path: repo.lease.to_str().unwrap().to_string(),
        state: "held".into(),
        boot_id: None,
        base: Some(LeaseBase {
            base_sha: c0.clone(),
            base_source: base::BaseSource::Head,
            base_attempt_id: None,
            canonical_path: repo.canonical_path.clone(),
            git_common_dir: repo.common_dir.clone(),
        }),
        delivery_policy: Some(DeliveryPolicy::Kernel),
    };
    let delivery = delivery_row(None);
    let payload = forge_payload_for(&delivery, &lease).unwrap();
    assert_eq!(payload.argv, repo.argv(), "the production argv");

    repo.worker_edit("worker.txt", "worker output\n");
    let wrapper = format!(
        "\"$REAL\" \"$@\"\nrc=$?\nif [ \"$1\" = commit ]; then \
         \"$REAL\" update-ref refs/heads/{} {c0}; fi\nexit $rc",
        repo.branch
    );
    let path = repo.path_with_git_wrapper(&wrapper);
    let live = repo.run_in(&repo.lease, &payload.argv, Some(path));
    assert_exit(&live, 0);
    let line = json_line(&live);
    assert_eq!(line["commit"], json!(c0), "the captured OID is the base");
    assert_eq!(line["base_is_ancestor"], json!(true));
    assert_eq!(repo.ref_target().as_deref(), Some(c0.as_str()), "ref → C0");
    assert_eq!(repo.head(), c0);
    // The orphan commit exists (the worker's file is in its tree) but nothing points at it.
    let orphan = git(
        &repo.lease,
        &["rev-parse", &format!("{}@{{1}}", repo.branch)],
    );
    assert_ne!(orphan, c0);
    assert_eq!(
        git(
            &repo.lease,
            &["ls-tree", "--name-only", &orphan, "worker.txt"]
        ),
        "worker.txt"
    );
    assert_eq!(
        git(&repo.lease, &["for-each-ref", "--points-at", &orphan]),
        "",
        "no ref points at the orphan"
    );

    // The production chain: the payload's four-field extraction table (its embedding in the
    // payload is pinned by `delivery_payload_semantic_hash_is_stable`) → candidate row →
    // derived state.
    let event = worktree_committed_delivery_fields()
        .extract_payload(0, Some(&line))
        .unwrap();
    let candidate = from_operation_result(&delivery, &lease, &Value::Object(event), 9).unwrap();
    assert_eq!(candidate.commit_sha, c0);
    assert_eq!(candidate.base_sha, c0);
    assert_eq!(candidate.ref_name, repo.ref_name);
    let state = delivery_state(
        TaskStatus::Done,
        None,
        Some(&delivery_row(Some(candidate_settled()))),
        Some(&candidate),
        None,
    );
    assert!(matches!(state, DeliveryState::NoChange { .. }), "{state:?}");
    assert_eq!(
        serde_json::to_value(&state).unwrap()["state"],
        json!("no_change")
    );
}

/// A30 fixtures 1–3 — a provenance observation that fails (a git command exits non-zero even
/// after printing a matching line; the common-dir query fails; no git on PATH) is exit 14: not
/// a verdict, no ref, nothing staged.
#[test]
fn provenance_observation_failure_is_not_a_verdict() {
    let fixtures: [(&str, &str); 2] = [
        (
            "worktree list prints the matching line, then exits 128",
            "if [ \"$1\" = worktree ] && [ \"$2\" = list ]; then \"$REAL\" \"$@\"; exit 128; fi\n\
             exec \"$REAL\" \"$@\"",
        ),
        (
            "rev-parse --git-common-dir exits 128",
            "if [ \"$1\" = rev-parse ] && [ \"$2\" = --path-format=absolute ]; then exit 128; fi\n\
             exec \"$REAL\" \"$@\"",
        ),
    ];
    for (label, wrapper) in fixtures {
        let repo = ScriptRepo::new();
        repo.worker_edit("worker.txt", "observed\n");
        let path = repo.path_with_git_wrapper(wrapper);
        let output = repo.run_delivery_with_path(path);
        assert_eq!(
            output.status.code(),
            Some(14),
            "{label}: {}",
            stdout(&output)
        );
        assert_eq!(repo.ref_target(), None, "{label}: no ref");
        assert_eq!(repo.status(), "?? worker.txt", "{label}: nothing staged");
    }

    // 3. No git on PATH at all: a directory holding only `sh`.
    let repo = ScriptRepo::new();
    repo.worker_edit("worker.txt", "observed\n");
    std::os::unix::fs::symlink("/bin/sh", repo.bin.join("sh")).unwrap();
    let output = repo.run_delivery_with_path(repo.bin.clone().into());
    assert_eq!(
        output.status.code(),
        Some(14),
        "no git: {}",
        stdout(&output)
    );
    assert_eq!(repo.ref_target(), None);
    assert_eq!(repo.status(), "?? worker.txt");
}

/// A30 fixture 4 — `symbolic-ref` printing the expected branch name and then exiting 128 is an
/// observation failure (exit 14, nothing staged), not a passed branch check.
#[test]
fn branch_observation_failure_is_not_a_verdict() {
    let repo = ScriptRepo::new();
    repo.worker_edit("worker.txt", "observed\n");
    let wrapper = format!(
        "if [ \"$1\" = symbolic-ref ]; then printf '%s\\n' '{}'; exit 128; fi\nexec \"$REAL\" \"$@\"",
        repo.branch
    );
    let path = repo.path_with_git_wrapper(&wrapper);
    let output = repo.run_delivery_with_path(path);
    assert_exit(&output, 14);
    assert_eq!(repo.ref_target(), None);
    assert_eq!(repo.status(), "?? worker.txt", "nothing was staged");
    assert_eq!(repo.head(), repo.base_sha, "no commit");
}

/// The probe half of A4: the probe answers "does the ref exist" (0 / 1) and the output probe
/// re-prints the ref's target — not HEAD, which has moved on to a clean C2 by then.
#[test]
fn delivery_probe_scripts_read_ref_not_head() {
    let repo = ScriptRepo::new();
    assert_exit(&repo.run_probe(&repo.ref_name), 1);
    repo.worker_edit("worker.txt", "c1\n");
    let delivered = repo.run_delivery();
    assert_exit(&delivered, 0);
    let c1 = json_line(&delivered)["commit"]
        .as_str()
        .unwrap()
        .to_string();
    assert_exit(&repo.run_probe(&repo.ref_name), 0);
    assert_exit(
        &repo.run_probe(&candidate_ref_name(TRACK, CARD, "absent")),
        1,
    );

    repo.worker_edit("later.txt", "c2\n");
    git(&repo.lease, &["add", "later.txt"]);
    git(&repo.lease, &["commit", "-q", "-m", "c2"]);
    let c2 = repo.head();
    assert_ne!(c2, c1);
    let probe = repo.run_output_probe();
    assert_exit(&probe, 0);
    assert_eq!(json_line(&probe)["commit"], json!(c1));
    assert_eq!(stdout(&probe), stdout(&delivered));
}

/// The argv the payload runs is the two texts joined by one newline, followed by the six
/// positional parameters in the script's order.
#[test]
fn delivery_argv_joins_provenance_and_delivery_scripts() {
    let argv = delivery_argv("m", "b", "r", "s", "cp", "gcd");
    assert_eq!(
        argv,
        vec![
            "sh".to_string(),
            "-c".into(),
            format!("{GIT_LEASE_PROVENANCE_SCRIPT}\n{GIT_DELIVERY_SCRIPT}"),
            "sh".into(),
            "m".into(),
            "b".into(),
            "r".into(),
            "s".into(),
            "cp".into(),
            "gcd".into(),
        ]
    );
    assert_eq!(
        delivery_message("t", "c", "d"),
        "neige: worker c @ track t (delivery d)"
    );
    assert_eq!(
        candidate_ref_name("t", "c", "d"),
        "refs/neige/candidates/t/c/d"
    );
}

// ---------------------------------------------------------------------------
// Table fixtures: an in-memory repository with one Track, one card and one kernel lease.
// ---------------------------------------------------------------------------

struct DbFixture {
    repo: SqlxRepo,
    _tmp: tempfile::TempDir,
    area_id: String,
    track_id: String,
    card_id: String,
    lease: WorkspaceLease,
}

async fn db_fixture() -> DbFixture {
    let tmp = tempfile::tempdir().unwrap();
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = crate::db::RepoSyncDomainRaw::area_create(
        &repo,
        crate::model::NewArea {
            name: "delivery fixture".into(),
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
            area_id: area.id.clone(),
            title: "delivery fixture".into(),
            sort: None,
            cwd: tmp.path().display().to_string(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        },
    )
    .await
    .unwrap();
    let track_id = track.id.to_string();
    let card_id = new_card(&repo, &track_id).await;
    let lease = kernel_lease(&repo, tmp.path(), &track_id, &card_id).await;
    DbFixture {
        repo,
        _tmp: tmp,
        area_id: area.id.to_string(),
        track_id,
        card_id,
        lease,
    }
}

async fn new_card(repo: &SqlxRepo, track_id: &str) -> String {
    crate::db::RepoSyncDomainRaw::card_create(
        repo,
        crate::model::NewCard {
            track_id: crate::ids::TrackId::from(track_id.to_string()),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        },
    )
    .await
    .unwrap()
    .id
    .to_string()
}

/// A lease through the production `acquire_workspace_lease_tx`: the five base columns and
/// `delivery_policy = 'kernel'` in the one INSERT. The repository root need not be a git
/// repository; the columns only have to be the shape the CHECK accepts.
async fn kernel_lease(
    repo: &SqlxRepo,
    repo_root: &Path,
    track_id: &str,
    card_id: &str,
) -> WorkspaceLease {
    let target = WorkspaceLeaseTarget {
        repo_root: repo_root.to_path_buf(),
        path: workspace_lease_path_for(repo_root, track_id, card_id).unwrap(),
        branch: workspace_slice_branch_for(track_id, card_id).unwrap(),
    };
    let parent = target.path.parent().unwrap();
    std::fs::create_dir_all(parent).unwrap();
    let lease_base = LeaseBase {
        base_sha: "b".repeat(40),
        base_source: base::BaseSource::Head,
        base_attempt_id: None,
        canonical_path: base::lease_canonical_path(parent, card_id).unwrap(),
        git_common_dir: repo_root.join(".git"),
    };
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let (lease, _event) =
        acquire_workspace_lease_tx(&mut tx, card_id, track_id, "op-test", &target, &lease_base)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    lease
}

async fn count(repo: &SqlxRepo, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(repo.pool())
        .await
        .unwrap()
}

impl DbFixture {
    async fn insert_delivery(&self, attempt: &str) -> DeliveryRow {
        let mut tx = begin_immediate_tx(self.repo.pool()).await.unwrap();
        let row = insert_initial_delivery_tx(
            &mut tx,
            &self.track_id,
            &self.card_id,
            attempt,
            &self.lease,
            1_000,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        row
    }

    fn candidate_for(&self, delivery: &DeliveryRow, commit_sha: &str) -> CandidateRow {
        CandidateRow {
            candidate_id: delivery.delivery_id.clone(),
            track_id: delivery.track_id.clone(),
            producer_attempt_id: delivery.producer_attempt_id.clone(),
            card_id: delivery.card_id.clone(),
            lease_id: delivery.lease_id.clone(),
            repo_root: "/repo".into(),
            git_common_dir: "/repo/.git".into(),
            branch: workspace_slice_branch_for(&self.track_id, &self.card_id).unwrap(),
            base_sha: "b".repeat(40),
            commit_sha: commit_sha.into(),
            base_is_ancestor: true,
            ref_name: candidate_ref_name(&self.track_id, &self.card_id, &delivery.delivery_id),
            created_at_ms: 2_000,
        }
    }
}

// ---------------------------------------------------------------------------
// Tables and triggers (migration 0113).
// ---------------------------------------------------------------------------

/// The `delivery_policy` column CHECK over `{sha,NULL} × {kernel,NULL,bogus,''}`: exactly
/// `(sha,kernel)`, `(sha,NULL)`, `(NULL,NULL)` land.
#[tokio::test]
async fn delivery_policy_check_rejects_every_invalid_tuple() {
    let fx = db_fixture().await;
    let mut accepted = Vec::new();
    for (index, (sha, policy)) in [Some("sha"), None]
        .into_iter()
        .flat_map(|sha| {
            [Some("kernel"), None, Some("bogus"), Some("")]
                .into_iter()
                .map(move |policy| (sha, policy))
        })
        .enumerate()
    {
        let (source, canonical_path, git_common_dir) = match sha {
            Some(_) => (Some("head"), Some("/cp"), Some("/gcd")),
            None => (None, None, None),
        };
        let result = sqlx::query(
            "INSERT INTO workspace_leases (lease_id, card_id, track_id, path, state, \
             lease_owner, lease_until_ms, boot_id, created_at_ms, updated_at_ms, \
             base_sha, base_source, base_attempt_id, canonical_path, git_common_dir, \
             delivery_policy) \
             VALUES (?1, ?2, ?3, ?4, 'released', 'op-test', NULL, NULL, 1, 1, \
             ?5, ?6, NULL, ?7, ?8, ?9)",
        )
        .bind(format!("policy-{index}"))
        .bind(&fx.card_id)
        .bind(&fx.track_id)
        .bind(format!("/tuple/{index}"))
        .bind(sha)
        .bind(source)
        .bind(canonical_path)
        .bind(git_common_dir)
        .bind(policy)
        .execute(fx.repo.pool())
        .await;
        match result {
            Ok(_) => accepted.push((sha, policy)),
            Err(error) => assert!(
                error.to_string().contains("CHECK constraint failed"),
                "({sha:?},{policy:?}) rejected by something other than the CHECK: {error}"
            ),
        }
    }
    assert_eq!(
        accepted,
        vec![
            (Some("sha"), Some("kernel")),
            (Some("sha"), None),
            (None, None)
        ]
    );
}

/// The settlement-group CHECK over all 432 tuples of
/// `{NULL,candidate,failed} × {NULL,1} × {NULL,x} × {NULL,x} × {NULL,0,1} × {NULL, 5 reasons}`:
/// exactly six shapes land — unsettled, candidate × three wake reasons, failed × retry 0/1.
#[tokio::test]
async fn settlement_check_rejects_every_invalid_tuple() {
    let fx = db_fixture().await;
    let wake_reasons = [
        None,
        Some("failed"),
        Some("ungated_candidate"),
        Some("gate_already_terminal"),
        Some("deferred_to_gate"),
        Some("bogus"),
    ];
    let mut accepted = Vec::new();
    let mut total = 0;
    let mut index = 0;
    for settlement in [None, Some("candidate"), Some("failed")] {
        for event_id in [None, Some(1_i64)] {
            for code in [None, Some("x")] {
                for reason in [None, Some("x")] {
                    for retry in [None, Some(0_i64), Some(1_i64)] {
                        for wake in wake_reasons {
                            total += 1;
                            index += 1;
                            let result = sqlx::query(
                                "INSERT INTO task_git_deliveries (delivery_id, track_id, \
                                 producer_attempt_id, card_id, lease_id, ordinal, operation_key, \
                                 forge_idempotency_key, created_at_ms, settlement, \
                                 settled_event_id, failure_code, failure_reason, retry_allowed, \
                                 wake_reason) \
                                 VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, 1, ?8, ?9, ?10, ?11, \
                                 ?12, ?13)",
                            )
                            .bind(format!("d-{index}"))
                            .bind(&fx.track_id)
                            .bind(format!("attempt-{index}"))
                            .bind(&fx.card_id)
                            .bind(&fx.lease.lease_id)
                            .bind(format!("op-{index}"))
                            .bind(format!("idem-{index}"))
                            .bind(settlement)
                            .bind(event_id)
                            .bind(code)
                            .bind(reason)
                            .bind(retry)
                            .bind(wake)
                            .execute(fx.repo.pool())
                            .await;
                            match result {
                                Ok(_) => {
                                    accepted.push((settlement, event_id, code, reason, retry, wake))
                                }
                                Err(error) => assert!(
                                    error.to_string().contains("CHECK constraint failed"),
                                    "tuple {index} rejected by something other than the CHECK: {error}"
                                ),
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(total, 432);
    assert_eq!(
        accepted,
        vec![
            (None, None, None, None, None, None),
            (
                Some("candidate"),
                Some(1),
                None,
                None,
                None,
                Some("ungated_candidate")
            ),
            (
                Some("candidate"),
                Some(1),
                None,
                None,
                None,
                Some("gate_already_terminal")
            ),
            (
                Some("candidate"),
                Some(1),
                None,
                None,
                None,
                Some("deferred_to_gate")
            ),
            (
                Some("failed"),
                Some(1),
                Some("x"),
                Some("x"),
                Some(0),
                Some("failed")
            ),
            (
                Some("failed"),
                Some(1),
                Some("x"),
                Some("x"),
                Some(1),
                Some("failed")
            ),
        ],
        "exactly the six CHECK-accepted shapes land"
    );
    assert_eq!(
        count(&fx.repo, "SELECT COUNT(*) FROM task_git_deliveries").await,
        6
    );
}

/// A5's trigger — the six settlement columns are written once: a settled row refuses any
/// further UPDATE, an unsettled row refuses an UPDATE that does not settle it, and the
/// `settle_*_tx` guards make a second settlement a no-op (`0` rows, `Ok`).
#[tokio::test]
async fn settlement_written_once() {
    let fx = db_fixture().await;
    let failed = fx.insert_delivery("attempt-failed").await;
    let candidate = fx.insert_delivery("attempt-candidate").await;
    let pending = fx.insert_delivery("attempt-pending").await;

    // An unsettled row refuses a non-settling UPDATE: one that edits metadata, and one that
    // writes `settlement = NULL` and nothing else (only the `NEW.settlement IS NULL` clause
    // rejects the latter; the metadata clauses do not fire).
    for sql in [
        "UPDATE task_git_deliveries SET reason = 'edited' WHERE delivery_id = ?1",
        "UPDATE task_git_deliveries SET settlement = NULL WHERE delivery_id = ?1",
    ] {
        let refused = sqlx::query(sql)
            .bind(&pending.delivery_id)
            .execute(fx.repo.pool())
            .await;
        assert!(
            refused
                .as_ref()
                .is_err_and(|error| error.to_string().contains("written once")),
            "{sql}: {refused:?}"
        );
    }
    // A first settlement that also changes a metadata column is refused as a whole.
    let refused = sqlx::query(
        "UPDATE task_git_deliveries SET settlement = 'failed', settled_event_id = 5, \
         failure_code = 'commit_failed', failure_reason = 'x', retry_allowed = 1, \
         wake_reason = 'failed', created_at_ms = created_at_ms + 1 WHERE delivery_id = ?1",
    )
    .bind(&pending.delivery_id)
    .execute(fx.repo.pool())
    .await;
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.to_string().contains("written once")),
        "{refused:?}"
    );

    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    assert_eq!(
        settle_failed_tx(
            &mut tx,
            &failed.delivery_id,
            7,
            DeliveryFailureCode::CommitFailed,
            "hook exited 1",
            true,
        )
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        settle_candidate_tx(
            &mut tx,
            &candidate.delivery_id,
            8,
            DeliveryWakeReason::UngatedCandidate,
            &fx.candidate_for(&candidate, &"c".repeat(40)),
        )
        .await
        .unwrap(),
        1
    );
    tx.commit().await.unwrap();

    for (column, value) in [
        ("retry_allowed", "0"),
        ("wake_reason", "'deferred_to_gate'"),
    ] {
        let sql =
            format!("UPDATE task_git_deliveries SET {column} = {value} WHERE delivery_id = ?1");
        let refused = sqlx::query(&sql)
            .bind(&failed.delivery_id)
            .execute(fx.repo.pool())
            .await;
        assert!(
            refused
                .as_ref()
                .is_err_and(|error| error.to_string().contains("written once")),
            "{column}: {refused:?}"
        );
    }

    // The second settlement of either kind is a guarded no-op, not an error.
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    assert_eq!(
        settle_failed_tx(
            &mut tx,
            &failed.delivery_id,
            9,
            DeliveryFailureCode::Unresolved,
            "again",
            true,
        )
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        settle_candidate_tx(
            &mut tx,
            &candidate.delivery_id,
            10,
            DeliveryWakeReason::DeferredToGate,
            &fx.candidate_for(&candidate, &"d".repeat(40)),
        )
        .await
        .unwrap(),
        0
    );
    let failed_row = delivery_by_id_tx(&mut tx, &failed.delivery_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        failed_row.settlement,
        Some(DeliverySettled::Failed {
            settled_event_id: 7,
            code: DeliveryFailureCode::CommitFailed,
            reason: "hook exited 1".into(),
            retry_allowed: true,
            wake_reason: DeliveryWakeReason::Failed,
        })
    );
    let candidate_row = delivery_by_id_tx(&mut tx, &candidate.delivery_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        candidate_row.settlement,
        Some(DeliverySettled::Candidate {
            settled_event_id: 8,
            wake_reason: DeliveryWakeReason::UngatedCandidate,
        })
    );
    let minted = candidate_for_attempt_tx(&mut tx, "attempt-candidate")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        minted.commit_sha,
        "c".repeat(40),
        "the first settlement's candidate stays"
    );
    assert_eq!(
        delivery_by_id_tx(&mut tx, &pending.delivery_id)
            .await
            .unwrap()
            .unwrap()
            .settlement,
        None
    );
    tx.commit().await.unwrap();
    assert_eq!(
        count(&fx.repo, "SELECT COUNT(*) FROM task_candidates").await,
        1
    );
}

#[tokio::test]
async fn candidate_row_is_immutable() {
    let fx = db_fixture().await;
    let delivery = fx.insert_delivery("attempt-1").await;
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    settle_candidate_tx(
        &mut tx,
        &delivery.delivery_id,
        1,
        DeliveryWakeReason::UngatedCandidate,
        &fx.candidate_for(&delivery, &"c".repeat(40)),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    for sql in [
        "UPDATE task_candidates SET commit_sha = 'moved' WHERE candidate_id = ?1",
        "UPDATE task_candidates SET branch = 'other' WHERE candidate_id = ?1",
        "UPDATE task_candidates SET base_is_ancestor = 0 WHERE candidate_id = ?1",
    ] {
        let refused = sqlx::query(sql)
            .bind(&delivery.delivery_id)
            .execute(fx.repo.pool())
            .await;
        assert!(
            refused
                .as_ref()
                .is_err_and(|error| error.to_string().contains("immutable")),
            "{sql}: {refused:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The abandonment table (migration 0114).
// ---------------------------------------------------------------------------

impl DbFixture {
    /// A delivery settled `failed` (the only shape an abandonment can hang on).
    async fn insert_failed_delivery(&self, attempt: &str) -> DeliveryRow {
        let row = self.insert_delivery(attempt).await;
        let mut tx = begin_immediate_tx(self.repo.pool()).await.unwrap();
        settle_failed_tx(
            &mut tx,
            &row.delivery_id,
            1,
            DeliveryFailureCode::CommitFailed,
            "hook",
            true,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        delivery_by_id_tx(
            &mut begin_immediate_tx(self.repo.pool()).await.unwrap(),
            &row.delivery_id,
        )
        .await
        .unwrap()
        .unwrap()
    }

    fn abandonment_for(
        &self,
        delivery: &DeliveryRow,
        request_key: &str,
        task_outcome: AbandonTaskOutcome,
        task_status: TaskStatus,
    ) -> AbandonmentRow {
        AbandonmentRow {
            delivery_id: delivery.delivery_id.clone(),
            track_id: delivery.track_id.clone(),
            producer_attempt_id: delivery.producer_attempt_id.clone(),
            request_idempotency_key: request_key.into(),
            reason: Some("gave up".into()),
            task_outcome,
            task_status,
            created_at_ms: 3_000,
        }
    }
}

/// The implication CHECK over `task_outcome × task_status` (3 × 2 = 6 tuples): exactly four
/// land — `failed/failed`, `done_unchanged/done`, `already_terminal/done`,
/// `already_terminal/failed` — and every other spelling of either column is refused.
#[tokio::test]
async fn abandonment_check_rejects_every_invalid_tuple() {
    let fx = db_fixture().await;
    let delivery = fx.insert_failed_delivery("attempt-1").await;
    let mut accepted = Vec::new();
    let mut index = 0;
    for outcome in ["failed", "done_unchanged", "already_terminal"] {
        for status in ["done", "failed"] {
            index += 1;
            let inserted = sqlx::query(
                "INSERT INTO task_git_delivery_abandonments (delivery_id, track_id, \
                 producer_attempt_id, request_idempotency_key, reason, task_outcome, \
                 task_status, created_at_ms) VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, 1)",
            )
            .bind(&delivery.delivery_id)
            .bind(&delivery.track_id)
            .bind(&delivery.producer_attempt_id)
            .bind(format!("req-{index}"))
            .bind(outcome)
            .bind(status)
            .execute(fx.repo.pool())
            .await;
            if inserted.is_ok() {
                accepted.push((outcome, status));
                sqlx::query("DELETE FROM task_git_delivery_abandonments WHERE delivery_id = ?1")
                    .bind(&delivery.delivery_id)
                    .execute(fx.repo.pool())
                    .await
                    .unwrap();
            }
        }
    }
    assert_eq!(
        accepted,
        vec![
            ("failed", "failed"),
            ("done_unchanged", "done"),
            ("already_terminal", "done"),
            ("already_terminal", "failed"),
        ]
    );
    for (outcome, status) in [
        ("bogus", "failed"),
        ("failed", "verifying"),
        ("failed", "canceled"),
        ("done_unchanged", ""),
    ] {
        let refused = sqlx::query(
            "INSERT INTO task_git_delivery_abandonments (delivery_id, track_id, \
             producer_attempt_id, request_idempotency_key, reason, task_outcome, task_status, \
             created_at_ms) VALUES (?1, ?2, ?3, 'req-x', NULL, ?4, ?5, 1)",
        )
        .bind(&delivery.delivery_id)
        .bind(&delivery.track_id)
        .bind(&delivery.producer_attempt_id)
        .bind(outcome)
        .bind(status)
        .execute(fx.repo.pool())
        .await;
        assert!(refused.is_err(), "{outcome}/{status}: {refused:?}");
    }
    // Both FKs: a delivery id without a row and a track id without a row are refused.
    for (delivery_id, track_id) in [
        ("no-such-delivery", fx.track_id.as_str()),
        (delivery.delivery_id.as_str(), "no-such-track"),
    ] {
        let refused = sqlx::query(
            "INSERT INTO task_git_delivery_abandonments (delivery_id, track_id, \
             producer_attempt_id, request_idempotency_key, reason, task_outcome, task_status, \
             created_at_ms) VALUES (?1, ?2, ?3, 'req-fk', NULL, 'failed', 'failed', 1)",
        )
        .bind(delivery_id)
        .bind(track_id)
        .bind(&delivery.producer_attempt_id)
        .execute(fx.repo.pool())
        .await;
        assert!(
            refused
                .as_ref()
                .is_err_and(|error| error.to_string().contains("FOREIGN KEY")),
            "{delivery_id}/{track_id}: {refused:?}"
        );
    }
}

/// Every column of an abandonment row is immutable; the PK refuses a second abandonment of the
/// same delivery and the request key is unique per attempt. The readers find the row by
/// delivery and by request key, and `facts()` is what the derivation reads.
#[tokio::test]
async fn abandonment_row_is_immutable() {
    let fx = db_fixture().await;
    let delivery = fx.insert_failed_delivery("attempt-1").await;
    let row = fx.abandonment_for(
        &delivery,
        "req-1",
        AbandonTaskOutcome::AlreadyTerminal,
        TaskStatus::Done,
    );
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    insert_abandonment_tx(&mut tx, &row).await.unwrap();
    tx.commit().await.unwrap();
    for sql in [
        "UPDATE task_git_delivery_abandonments SET reason = 'edited' WHERE delivery_id = ?1",
        "UPDATE task_git_delivery_abandonments SET task_outcome = 'failed', task_status = 'failed' \
         WHERE delivery_id = ?1",
        "UPDATE task_git_delivery_abandonments SET request_idempotency_key = 'other' \
         WHERE delivery_id = ?1",
        "UPDATE task_git_delivery_abandonments SET created_at_ms = 9 WHERE delivery_id = ?1",
    ] {
        let refused = sqlx::query(sql)
            .bind(&delivery.delivery_id)
            .execute(fx.repo.pool())
            .await;
        assert!(
            refused
                .as_ref()
                .is_err_and(|error| error.to_string().contains("immutable")),
            "{sql}: {refused:?}"
        );
    }
    // A second abandonment of the same delivery (any key) and a reused key (any delivery).
    let other = fx.insert_failed_delivery("attempt-2").await;
    for (delivery_for_row, key, what) in [
        (&delivery, "req-2", "same delivery, new key"),
        (
            &other,
            "req-1",
            "same key on another delivery of the same attempt",
        ),
    ] {
        let mut duplicate = fx.abandonment_for(
            delivery_for_row,
            key,
            AbandonTaskOutcome::Failed,
            TaskStatus::Failed,
        );
        duplicate.producer_attempt_id = "attempt-1".into();
        let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
        let refused = insert_abandonment_tx(&mut tx, &duplicate).await;
        assert!(
            refused
                .as_ref()
                .is_err_and(|error| error.to_string().contains("UNIQUE")),
            "{what}: {refused:?}"
        );
    }
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    assert_eq!(
        abandonment_for_delivery_tx(&mut tx, &delivery.delivery_id)
            .await
            .unwrap()
            .as_ref(),
        Some(&row)
    );
    assert_eq!(
        abandonment_by_request_key_tx(&mut tx, &fx.track_id, "attempt-1", "req-1")
            .await
            .unwrap()
            .as_ref(),
        Some(&row)
    );
    assert_eq!(
        abandonment_by_request_key_tx(&mut tx, &fx.track_id, "attempt-1", "req-9")
            .await
            .unwrap(),
        None
    );
    // The replay key is Track-scoped: another Track quoting this attempt and key finds nothing.
    assert_eq!(
        abandonment_by_request_key_tx(&mut tx, "another-track", "attempt-1", "req-1")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        abandonment_for_delivery_tx(&mut tx, &other.delivery_id)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        row.facts(),
        AbandonmentFacts {
            reason: Some("gave up".into()),
            task_outcome: "already_terminal".into(),
            task_status: "done".into(),
        }
    );
}

/// The retry row: `ordinal + 1`, the predecessor, the request key and reason; found by request
/// key (an `ordinal = 1` row never is) and as the attempt's latest.
#[tokio::test]
async fn retry_delivery_row_and_request_key_reader() {
    let fx = db_fixture().await;
    let first = fx.insert_failed_delivery("attempt-1").await;
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    let retry = insert_retry_delivery_tx(&mut tx, &first, "req-1", Some("hook fixed"), 4_000)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(retry.ordinal, 2);
    assert_eq!(
        retry.predecessor_delivery_id.as_deref(),
        Some(first.delivery_id.as_str())
    );
    assert_eq!(retry.request_idempotency_key.as_deref(), Some("req-1"));
    assert_eq!(retry.reason.as_deref(), Some("hook fixed"));
    assert_eq!(retry.lease_id, first.lease_id);
    assert_eq!(retry.card_id, first.card_id);
    assert_ne!(retry.delivery_id, first.delivery_id);
    assert_ne!(retry.operation_key, first.operation_key);
    assert_ne!(retry.forge_idempotency_key, first.forge_idempotency_key);
    assert!(retry.settlement.is_none());
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    assert_eq!(
        delivery_by_request_key_tx(&mut tx, &fx.track_id, "attempt-1", "req-1")
            .await
            .unwrap()
            .as_ref(),
        Some(&retry)
    );
    assert_eq!(
        delivery_by_request_key_tx(&mut tx, &fx.track_id, "attempt-1", "req-2")
            .await
            .unwrap(),
        None
    );
    // The replay key is Track-scoped: another Track quoting this attempt and key finds nothing.
    assert_eq!(
        delivery_by_request_key_tx(&mut tx, "another-track", "attempt-1", "req-1")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        delivery_latest_for_attempt_tx(&mut tx, "attempt-1")
            .await
            .unwrap()
            .as_ref(),
        Some(&retry)
    );
    // The same request key twice on one attempt is refused by the table.
    let refused = insert_retry_delivery_tx(&mut tx, &retry, "req-1", None, 5_000).await;
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.to_string().contains("UNIQUE")),
        "{refused:?}"
    );
}

/// A9d (this slice's part) — deleting the Track, or its Area, cascades both tables away in the
/// one `DELETE FROM tracks` statement (the delivery's non-cascading lease FK and the candidate's
/// delivery FK are checked at statement end); events outlive the rows. This pins the PAIR of
/// abandonment cascades (`track_id` and `delivery_id`): dropping one alone is masked by the other
/// chain; `abandonment_cascades_with_its_delivery_row` pins the `delivery_id` cascade by itself.
#[tokio::test]
async fn delivery_tables_cascade_on_track_delete() {
    for delete_area in [false, true] {
        let fx = db_fixture().await;
        let settled = fx.insert_delivery("attempt-settled").await;
        let _pending = fx.insert_delivery("attempt-pending").await;
        let abandoned = fx.insert_failed_delivery("attempt-abandoned").await;
        let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
        settle_candidate_tx(
            &mut tx,
            &settled.delivery_id,
            1,
            DeliveryWakeReason::UngatedCandidate,
            &fx.candidate_for(&settled, &"c".repeat(40)),
        )
        .await
        .unwrap();
        insert_abandonment_tx(
            &mut tx,
            &fx.abandonment_for(
                &abandoned,
                "req-1",
                AbandonTaskOutcome::Failed,
                TaskStatus::Failed,
            ),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            count(&fx.repo, "SELECT COUNT(*) FROM task_git_deliveries").await,
            3
        );
        assert_eq!(
            count(
                &fx.repo,
                "SELECT COUNT(*) FROM task_git_delivery_abandonments"
            )
            .await,
            1
        );
        assert_eq!(
            count(&fx.repo, "SELECT COUNT(*) FROM task_candidates").await,
            1
        );
        let events_before = count(&fx.repo, "SELECT COUNT(*) FROM events").await;
        assert!(events_before > 0, "the lease acquisition left an event");

        let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
        if delete_area {
            crate::db::sqlite::area_delete_tx(&mut tx, &fx.area_id)
                .await
                .unwrap();
        } else {
            crate::db::sqlite::track_delete_tx(
                &mut tx,
                &fx.track_id,
                &crate::track_area_cache::TrackAreaCache::new(),
            )
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();
        assert_eq!(
            count(&fx.repo, "SELECT COUNT(*) FROM task_git_deliveries").await,
            0
        );
        assert_eq!(
            count(&fx.repo, "SELECT COUNT(*) FROM task_candidates").await,
            0
        );
        assert_eq!(
            count(
                &fx.repo,
                "SELECT COUNT(*) FROM task_git_delivery_abandonments"
            )
            .await,
            0
        );
        assert_eq!(
            count(&fx.repo, "SELECT COUNT(*) FROM workspace_leases").await,
            0
        );
        assert_eq!(
            count(&fx.repo, "SELECT COUNT(*) FROM events").await,
            events_before
        );
    }
}

/// The abandonment row's own `delivery_id … ON DELETE CASCADE` (0114), pinned on its own:
/// deleting the delivery row takes the abandonment with it. The Track-delete test above pins the
/// PAIR of cascades — there, either FK alone is masked by the other chain (the Track cascade
/// removes both rows whichever FK cascades first) — so a dropped `delivery_id` cascade only
/// reads as `FOREIGN KEY constraint failed` here.
#[tokio::test]
async fn abandonment_cascades_with_its_delivery_row() {
    let fx = db_fixture().await;
    let abandoned = fx.insert_failed_delivery("attempt-abandoned").await;
    let kept = fx.insert_failed_delivery("attempt-kept").await;
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    for (delivery, key) in [(&abandoned, "req-1"), (&kept, "req-2")] {
        insert_abandonment_tx(
            &mut tx,
            &fx.abandonment_for(
                delivery,
                key,
                AbandonTaskOutcome::Failed,
                TaskStatus::Failed,
            ),
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    assert_eq!(
        count(
            &fx.repo,
            "SELECT COUNT(*) FROM task_git_delivery_abandonments"
        )
        .await,
        2
    );

    sqlx::query("DELETE FROM task_git_deliveries WHERE delivery_id = ?1")
        .bind(&abandoned.delivery_id)
        .execute(fx.repo.pool())
        .await
        .expect("the abandonment cascades with its delivery row");
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    assert_eq!(
        abandonment_for_delivery_tx(&mut tx, &abandoned.delivery_id)
            .await
            .unwrap(),
        None,
        "the abandonment of the deleted delivery is gone"
    );
    assert!(
        abandonment_for_delivery_tx(&mut tx, &kept.delivery_id)
            .await
            .unwrap()
            .is_some(),
        "the other delivery's abandonment stays"
    );
    assert_eq!(
        count(&fx.repo, "SELECT COUNT(*) FROM task_git_deliveries").await,
        1
    );
}

// ---------------------------------------------------------------------------
// Delivery rows and the payload.
// ---------------------------------------------------------------------------

/// The initial row: `ordinal = 1`, fresh ids, the forge idempotency key spelled through the
/// plugin id constant; the readers find it by attempt, by id, and as unsettled with or without
/// a forge Operation under its key; the lease is read back in any state.
#[tokio::test]
async fn initial_delivery_row_and_readers() {
    let fx = db_fixture().await;
    let row = fx.insert_delivery("attempt-1").await;
    assert_eq!(row.ordinal, 1);
    assert_eq!(
        row.forge_idempotency_key,
        format!(
            "dev.neige.git-forge:{}:{}:git.commit:d:{}",
            fx.track_id, fx.card_id, row.delivery_id
        )
    );
    assert_ne!(row.operation_key, row.delivery_id);
    assert_eq!(row.settlement, None);

    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    assert_eq!(
        delivery_latest_for_attempt_tx(&mut tx, "attempt-1")
            .await
            .unwrap(),
        Some(row.clone())
    );
    assert_eq!(
        delivery_by_id_tx(&mut tx, &row.delivery_id).await.unwrap(),
        Some(row.clone())
    );
    assert_eq!(
        delivery_latest_for_attempt_tx(&mut tx, "attempt-none")
            .await
            .unwrap(),
        None
    );
    let unsettled = unsettled_deliveries_for_track_tx(&mut tx, &fx.track_id)
        .await
        .unwrap();
    assert_eq!(unsettled.len(), 1);
    assert_eq!(unsettled[0].row, row);
    assert_eq!(unsettled[0].operation_id, None, "no forge op yet");
    let lease = lease_for_delivery_tx(&mut tx, &row).await.unwrap();
    assert_eq!(lease.lease_id, fx.lease.lease_id);
    assert_eq!(lease.delivery_policy, Some(DeliveryPolicy::Kernel));
    tx.commit().await.unwrap();

    // A forge Operation under the row's key is found; the released lease still reads.
    let op_repo = crate::operation::SqlxOperationRepo::new(fx.repo.pool().clone());
    let op_id = crate::operation::OperationRepo::insert_operation(
        &op_repo,
        crate::operation::forge_action_adapter::FORGE_ACTION_KIND,
        crate::operation::OperationKey {
            operation_key: row.operation_key.clone(),
            idempotency_key: Some(row.forge_idempotency_key.clone()),
            payload_hash: "hash".into(),
        },
        json!({}),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE workspace_leases SET state = 'released' WHERE lease_id = ?1")
        .bind(&fx.lease.lease_id)
        .execute(fx.repo.pool())
        .await
        .unwrap();
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    let unsettled = unsettled_deliveries_for_track_tx(&mut tx, &fx.track_id)
        .await
        .unwrap();
    assert_eq!(unsettled[0].operation_id.as_deref(), Some(op_id.as_str()));
    let lease = lease_for_delivery_tx(&mut tx, &row).await.unwrap();
    assert_eq!(lease.state, "released");
    tx.commit().await.unwrap();

    // A legacy (plain) lease never gets a delivery row.
    let plain_card = new_card(&fx.repo, &fx.track_id).await;
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    let (plain, _event) = crate::operation::workspace_lease::acquire_plain_workspace_lease_tx(
        &mut tx,
        &plain_card,
        &fx.track_id,
        "op-plain",
        &fx._tmp.path().join("plain"),
    )
    .await
    .unwrap();
    assert_eq!(plain.delivery_policy, None);
    let refused = insert_initial_delivery_tx(
        &mut tx,
        &fx.track_id,
        &plain_card,
        "attempt-plain",
        &plain,
        1,
    )
    .await;
    assert!(refused.is_err(), "{refused:?}");
    tx.rollback().await.unwrap();
}

/// The payload of one delivery row hashes the same every time it is assembled (the sweep's
/// re-submission dedups on it), carries the four-field table, and the legacy two-field table
/// never carries `delivery_id`.
#[tokio::test]
async fn delivery_payload_semantic_hash_is_stable() {
    let fx = db_fixture().await;
    let row = fx.insert_delivery("attempt-1").await;
    let first = forge_payload_for(&row, &fx.lease).unwrap();
    let second = forge_payload_for(&row, &fx.lease).unwrap();
    let hash = crate::mcp_server::transport::semantic_payload_hash;
    assert_eq!(hash(&first).unwrap(), hash(&second).unwrap());
    assert_eq!(first.idem_key, format!("git.commit:d:{}", row.delivery_id));
    let base = fx.lease.base.as_ref().unwrap();
    let branch = workspace_slice_branch_for(&fx.track_id, &fx.card_id).unwrap();
    let ref_name = candidate_ref_name(&fx.track_id, &fx.card_id, &row.delivery_id);
    assert_eq!(
        first.argv,
        delivery_argv(
            &delivery_message(&fx.track_id, &fx.card_id, &row.delivery_id),
            &branch,
            &ref_name,
            &base.base_sha,
            base.canonical_path.to_str().unwrap(),
            base.git_common_dir.to_str().unwrap(),
        )
    );
    let probe = first.probe.as_ref().unwrap();
    assert_eq!(
        probe.probe_argv,
        vec![
            "sh",
            "-c",
            GIT_DELIVERY_PROBE_SCRIPT,
            "sh",
            ref_name.as_str()
        ]
    );
    assert_eq!(
        probe.output_probe_argv.as_deref().unwrap(),
        [
            "sh",
            "-c",
            GIT_DELIVERY_OUTPUT_PROBE_SCRIPT,
            "sh",
            branch.as_str(),
            ref_name.as_str(),
            base.base_sha.as_str(),
        ]
    );
    // The four-field extraction table, read off the payload's Debug shape (the legacy two-field
    // table is pinned by `worker_success_commit_payload_uses_shared_git_scripts_as_drift_lock`).
    let shape = format!("{first:?}");
    assert!(
        shape.contains("event_kind: \"worktree.committed\""),
        "{shape}"
    );
    for (field, path) in [
        ("branch", "/branch"),
        ("commit_sha", "/commit"),
        ("delivery_id", "/delivery_id"),
        ("base_is_ancestor", "/base_is_ancestor"),
    ] {
        assert!(
            shape.contains(&format!("{field:?}: JsonField {{ path: {path:?} }}")),
            "{field}: {shape}"
        );
    }
    assert!(!first.parked);

    // A lease without the kernel policy or without a base is refused, never a payload.
    let mut legacy_lease = fx.lease.clone();
    legacy_lease.delivery_policy = None;
    assert!(forge_payload_for(&row, &legacy_lease).is_err());
    let mut baseless = fx.lease.clone();
    baseless.base = None;
    assert!(forge_payload_for(&row, &baseless).is_err());
}

/// The candidate row is a copy of the result event and the lease; an event naming another
/// delivery is refused.
#[test]
fn candidate_from_operation_result_copies_event_and_lease() {
    let lease = WorkspaceLease {
        lease_id: "lease-1".into(),
        card_id: CARD.into(),
        track_id: TRACK.into(),
        path: "/repo/.claude/worktrees/trk/crd".into(),
        state: "released".into(),
        boot_id: None,
        base: Some(LeaseBase {
            base_sha: "b".repeat(40),
            base_source: base::BaseSource::Head,
            base_attempt_id: None,
            canonical_path: PathBuf::from("/real/repo/.claude/worktrees/trk/crd"),
            git_common_dir: PathBuf::from("/real/repo/.git"),
        }),
        delivery_policy: Some(DeliveryPolicy::Kernel),
    };
    let delivery = delivery_row(None);
    let event = json!({
        "track_id": TRACK, "card_id": CARD, "commit_sha": "c".repeat(40),
        "branch": "neige/trk/crd", "delivery_id": DELIVERY, "base_is_ancestor": false,
    });
    let candidate = from_operation_result(&delivery, &lease, &event, 5).unwrap();
    assert_eq!(
        candidate,
        CandidateRow {
            candidate_id: DELIVERY.into(),
            track_id: TRACK.into(),
            producer_attempt_id: "attempt-1".into(),
            card_id: CARD.into(),
            lease_id: "lease-1".into(),
            repo_root: "/repo".into(),
            git_common_dir: "/real/repo/.git".into(),
            branch: "neige/trk/crd".into(),
            base_sha: "b".repeat(40),
            commit_sha: "c".repeat(40),
            base_is_ancestor: false,
            ref_name: candidate_ref_name(TRACK, CARD, DELIVERY),
            created_at_ms: 5,
        }
    );
    let mut other = event.clone();
    other["delivery_id"] = json!("dlv2");
    assert!(from_operation_result(&delivery, &lease, &other, 5).is_err());
    let mut missing = event.clone();
    missing.as_object_mut().unwrap().remove("base_is_ancestor");
    assert!(from_operation_result(&delivery, &lease, &missing, 5).is_err());
}

#[test]
fn repo_root_is_the_inverse_of_lease_path() {
    use super::candidate::repo_root_from_lease_path;
    for root in ["/repo", "/mnt/data/x y/repo", "/"] {
        let path = workspace_lease_path_for(Path::new(root), TRACK, CARD).unwrap();
        assert_eq!(
            repo_root_from_lease_path(path.to_str().unwrap(), TRACK, CARD).unwrap(),
            root
        );
    }
    let path = workspace_lease_path_for(Path::new("/repo"), TRACK, CARD).unwrap();
    assert!(repo_root_from_lease_path(path.to_str().unwrap(), "other", CARD).is_err());
    assert!(repo_root_from_lease_path(path.to_str().unwrap(), TRACK, "other").is_err());
    assert!(repo_root_from_lease_path("/repo/elsewhere", TRACK, CARD).is_err());
    assert!(repo_root_from_lease_path("relative/.claude/worktrees/trk/crd", TRACK, CARD).is_err());
}

// ---------------------------------------------------------------------------
// Failure classification (D2 code table).
// ---------------------------------------------------------------------------

fn result_file(exit_code: i32, stdout: &str) -> ForgeActionResultFile {
    ForgeActionResultFile {
        exit_code,
        stdout: stdout.into(),
    }
}

#[test]
fn classify_failure_maps_every_code() {
    use super::delivery::failure_sentence;
    let observation = "provenance realpath=/a common_dir=/b registered=0";
    for code in [10, 12] {
        let (kind, reason, retry) = classify_failure(
            Some(&result_file(code, &format!("{observation}\n"))),
            None,
            true,
        );
        assert_eq!(kind, DeliveryFailureCode::ProvenanceMismatch, "{code}");
        assert_eq!(
            reason,
            format!(
                "{}\n{observation}",
                failure_sentence(&code.to_string(), None)
            )
        );
        assert!(retry);
    }
    let (kind, reason, retry) = classify_failure(Some(&result_file(11, "ignored\n")), None, true);
    assert_eq!(kind, DeliveryFailureCode::ProvenanceMismatch);
    assert_eq!(
        reason,
        failure_sentence("11", None),
        "11 carries no evidence"
    );
    assert!(retry);
    let entries = "100644 a1 1\tbase.txt\n100644 a2 2\tbase.txt\n100644 a3 3\tbase.txt\n";
    let (kind, reason, _) = classify_failure(Some(&result_file(15, entries)), None, true);
    assert_eq!(kind, DeliveryFailureCode::ProvenanceMismatch);
    assert_eq!(
        reason,
        format!("{}\n{}", failure_sentence("15", None), entries.trim_end())
    );
    let (kind, reason, _) = classify_failure(Some(&result_file(15, "MERGE_HEAD\n")), None, true);
    assert_eq!(kind, DeliveryFailureCode::ProvenanceMismatch);
    assert!(reason.ends_with("\nMERGE_HEAD"), "{reason}");

    for code in [13, 14] {
        let (kind, reason, retry) = classify_failure(Some(&result_file(code, "")), None, true);
        assert_eq!(kind, DeliveryFailureCode::CommitFailed, "{code}");
        assert_eq!(reason, failure_sentence(&code.to_string(), None));
        assert!(retry);
    }
    for code in [1, 2, 128, 255] {
        let (kind, reason, retry) =
            classify_failure(Some(&result_file(code, "")), Some("action-failed"), true);
        assert_eq!(kind, DeliveryFailureCode::CommitFailed, "{code}");
        assert!(reason.contains(&format!("status {code} ")), "{reason}");
        assert!(retry);
    }
    for class in ["action-failed", "action-not-landed"] {
        let (kind, reason, retry) = classify_failure(None, Some(class), true);
        assert_eq!(kind, DeliveryFailureCode::CommitFailed, "{class}");
        assert_eq!(reason, failure_sentence("no_result", None));
        assert!(retry);
    }
    for (result, class) in [
        (None, Some("gate-infra")),
        (None, Some("action-timeout")),
        (None, Some("parked_deadline")),
        (None, Some("something-new")),
        (None, None),
        (Some(result_file(0, "{}")), Some("action-failed")),
        (Some(result_file(0, "")), None),
    ] {
        let (kind, reason, retry) = classify_failure(result.as_ref(), class, true);
        assert_eq!(kind, DeliveryFailureCode::Unresolved, "{class:?}");
        assert_eq!(reason, failure_sentence("unresolved", None));
        assert!(retry);
    }
    // The missing workspace is judged before anything else, and is never retryable.
    let (kind, reason, retry) =
        classify_failure(Some(&result_file(10, "x\n")), Some("action-failed"), false);
    assert_eq!(kind, DeliveryFailureCode::WorkspaceMissing);
    assert_eq!(reason, failure_sentence("workspace_missing", None));
    assert!(!retry);

    // Every key the mapping uses has a sentence, and none carries the G4 clause: the wake text
    // states it once, keyed on `retry_allowed` (`observation.rs`, slice 3 review round 2).
    for key in [
        "10",
        "11",
        "12",
        "13",
        "14",
        "15",
        "git",
        "no_result",
        "workspace_missing",
        "unresolved",
    ] {
        let sentence = failure_sentence(key, Some(1));
        assert_ne!(sentence, key, "no sentence for {key}");
        assert!(!sentence.contains("Retry delivers"), "{key}: {sentence}");
        assert!(sentence.ends_with('.'), "{key}: {sentence}");
    }

    // Evidence is cut to the fixed limits: a 1100-byte line is truncated to the cap.
    let long = "x".repeat(1100);
    assert!(long.len() > FAILURE_EVIDENCE_MAX_LINE_BYTES);
    let many = (0..FAILURE_EVIDENCE_MAX_LINES * 2)
        .map(|_| long.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let (_, reason, _) = classify_failure(Some(&result_file(15, &many)), None, true);
    let evidence: Vec<&str> = reason.lines().skip(1).collect();
    assert_eq!(evidence.len(), FAILURE_EVIDENCE_MAX_LINES);
    assert!(
        evidence
            .iter()
            .all(|line| line.len() == FAILURE_EVIDENCE_MAX_LINE_BYTES)
    );

    // A production-shaped observation line (two absolute paths with real-length ids) survives
    // intact: the `common_dir=` and `registered=` facts the 10 / 12 sentences point at are the
    // tail of the line, which is exactly what a 200-byte cap cut off.
    let repo_root = "/mnt/data2/kenji/neige-calm/.claude/worktrees/wt-primary-checkout";
    let lease_path =
        workspace_lease_path_for(Path::new(repo_root), &"t".repeat(32), &"c".repeat(32)).unwrap();
    let production_line = format!(
        "provenance realpath={} common_dir={repo_root}/.git registered=0",
        lease_path.display()
    );
    assert!(
        (250..=320).contains(&production_line.len()),
        "{} bytes: the measured production range",
        production_line.len()
    );
    for code in [10, 12] {
        let (_, reason, _) = classify_failure(
            Some(&result_file(code, &format!("{production_line}\n"))),
            None,
            true,
        );
        assert_eq!(
            reason.lines().nth(1),
            Some(production_line.as_str()),
            "{code}: the observation line is carried whole"
        );
        assert!(reason.ends_with(" registered=0"), "{code}: {reason}");
    }
}

// ---------------------------------------------------------------------------
// Pure derivations (D2 derivation table; D8 binding table).
// ---------------------------------------------------------------------------

fn delivery_row(settlement: Option<DeliverySettled>) -> DeliveryRow {
    DeliveryRow {
        delivery_id: DELIVERY.into(),
        track_id: TRACK.into(),
        producer_attempt_id: "attempt-1".into(),
        card_id: CARD.into(),
        lease_id: "lease-1".into(),
        ordinal: 1,
        operation_key: "op-key".into(),
        forge_idempotency_key: "idem".into(),
        predecessor_delivery_id: None,
        request_idempotency_key: None,
        reason: None,
        created_at_ms: 1,
        settlement,
    }
}

fn candidate_settled() -> DeliverySettled {
    DeliverySettled::Candidate {
        settled_event_id: 1,
        wake_reason: DeliveryWakeReason::UngatedCandidate,
    }
}

fn failed_settled() -> DeliverySettled {
    DeliverySettled::Failed {
        settled_event_id: 1,
        code: DeliveryFailureCode::CommitFailed,
        reason: "hook".into(),
        retry_allowed: true,
        wake_reason: DeliveryWakeReason::Failed,
    }
}

fn candidate(commit_sha: &str, base_is_ancestor: bool) -> CandidateRow {
    CandidateRow {
        candidate_id: DELIVERY.into(),
        track_id: TRACK.into(),
        producer_attempt_id: "attempt-1".into(),
        card_id: CARD.into(),
        lease_id: "lease-1".into(),
        repo_root: "/repo".into(),
        git_common_dir: "/repo/.git".into(),
        branch: "neige/trk/crd".into(),
        base_sha: "b".repeat(40),
        commit_sha: commit_sha.into(),
        base_is_ancestor,
        ref_name: candidate_ref_name(TRACK, CARD, DELIVERY),
        created_at_ms: 2,
    }
}

/// A real abandonment row's facts (`AbandonmentRow::facts`), not a hand-built struct.
fn abandonment() -> AbandonmentFacts {
    AbandonmentRow {
        delivery_id: DELIVERY.into(),
        track_id: TRACK.into(),
        producer_attempt_id: "attempt-1".into(),
        request_idempotency_key: "req-1".into(),
        reason: Some("gave up".into()),
        task_outcome: AbandonTaskOutcome::Failed,
        task_status: TaskStatus::Failed,
        created_at_ms: 3,
    }
    .facts()
}

/// D2's derivation table, one assertion per input (15: the two `not_reported` statuses, the
/// two `pending`/`canceled` no-lease statuses, `failed`, the two `inconsistent` statuses, the
/// six row shapes, and the two `inconsistent` row shapes — a `candidate` settlement without its
/// row, a `failed` settlement with one).
#[test]
fn delivery_state_covers_every_row() {
    let pending = delivery_row(None);
    let with_candidate = delivery_row(Some(candidate_settled()));
    let failed = delivery_row(Some(failed_settled()));
    let committed = candidate(&"c".repeat(40), true);
    let unchanged = candidate(&"b".repeat(40), true);

    for status in [
        TaskStatus::Dispatched,
        TaskStatus::Running,
        TaskStatus::Pending,
        TaskStatus::Canceled,
    ] {
        let state = delivery_state(status, None, None, None, None);
        assert_eq!(state, DeliveryState::NotReported, "{status:?}");
        assert_eq!(
            serde_json::to_value(&state).unwrap(),
            json!({"state": "not_reported"})
        );
    }
    let ended = delivery_state(TaskStatus::Failed, Some("worker-timeout"), None, None, None);
    assert_eq!(
        ended,
        DeliveryState::EndedWithoutDelivery {
            attempt_status: TaskStatus::Failed,
            status_detail: Some("worker-timeout".into()),
        }
    );
    assert_eq!(
        serde_json::to_value(&ended).unwrap(),
        json!({"state": "ended_without_delivery", "attempt_status": "failed", "status_detail": "worker-timeout"})
    );
    for status in [TaskStatus::Verifying, TaskStatus::Done] {
        assert_eq!(
            delivery_state(status, None, None, None, None),
            DeliveryState::Inconsistent {
                mismatches: vec![MISMATCH_DELIVERY_ROW_MISSING]
            },
            "{status:?}"
        );
    }
    assert_eq!(
        delivery_state(TaskStatus::Done, None, Some(&pending), None, None),
        DeliveryState::Pending {
            delivery_id: DELIVERY.into(),
            ordinal: 1
        }
    );
    assert_eq!(
        delivery_state(
            TaskStatus::Done,
            None,
            Some(&with_candidate),
            Some(&committed),
            None
        ),
        DeliveryState::Committed {
            delivery_id: DELIVERY.into(),
            ordinal: 1,
            candidate_id: DELIVERY.into(),
            commit_sha: "c".repeat(40),
            r#ref: candidate_ref_name(TRACK, CARD, DELIVERY),
            base_is_ancestor: true,
        }
    );
    let no_change = delivery_state(
        TaskStatus::Verifying,
        None,
        Some(&with_candidate),
        Some(&unchanged),
        None,
    );
    assert_eq!(
        no_change,
        DeliveryState::NoChange {
            delivery_id: DELIVERY.into(),
            ordinal: 1,
            candidate_id: DELIVERY.into(),
            commit_sha: "b".repeat(40),
            r#ref: candidate_ref_name(TRACK, CARD, DELIVERY),
            base_is_ancestor: true,
        }
    );
    assert_eq!(
        serde_json::to_value(&no_change).unwrap()["state"],
        json!("no_change")
    );
    let failed_state = delivery_state(TaskStatus::Done, None, Some(&failed), None, None);
    assert_eq!(
        failed_state,
        DeliveryState::Failed {
            delivery_id: DELIVERY.into(),
            ordinal: 1,
            failure: DeliveryFailure {
                code: DeliveryFailureCode::CommitFailed,
                reason: "hook".into(),
                retry_allowed: true,
            },
        }
    );
    assert_eq!(
        serde_json::to_value(&failed_state).unwrap(),
        json!({
            "state": "failed", "delivery_id": DELIVERY, "ordinal": 1,
            "failure": {"code": "commit_failed", "reason": "hook", "retry_allowed": true}
        })
    );
    assert_eq!(
        delivery_state(
            TaskStatus::Failed,
            None,
            Some(&failed),
            None,
            Some(&abandonment())
        ),
        DeliveryState::Abandoned {
            delivery_id: DELIVERY.into(),
            ordinal: 1,
            reason: Some("gave up".into()),
            task_outcome: "failed".into(),
            task_status: "failed".into(),
        }
    );
    assert_eq!(
        serde_json::to_value(delivery_state(
            TaskStatus::Failed,
            None,
            Some(&failed),
            None,
            Some(&abandonment())
        ))
        .unwrap(),
        json!({
            "state": "abandoned", "delivery_id": DELIVERY, "ordinal": 1,
            "reason": "gave up", "task_outcome": "failed", "task_status": "failed"
        })
    );
    assert_eq!(
        delivery_state(
            TaskStatus::Done,
            None,
            Some(&with_candidate),
            Some(&committed),
            Some(&abandonment())
        ),
        DeliveryState::Inconsistent {
            mismatches: vec![MISMATCH_ABANDONMENT_WITH_CANDIDATE]
        }
    );
    // The two row shapes one transaction never leaves behind, each with its own tag.
    for abandoned in [None, Some(abandonment())] {
        assert_eq!(
            delivery_state(
                TaskStatus::Done,
                None,
                Some(&with_candidate),
                None,
                abandoned.as_ref()
            ),
            DeliveryState::Inconsistent {
                mismatches: vec![MISMATCH_CANDIDATE_ROW_MISSING]
            },
            "candidate settlement without a candidate row ({abandoned:?})"
        );
        assert_eq!(
            delivery_state(
                TaskStatus::Done,
                None,
                Some(&failed),
                Some(&committed),
                abandoned.as_ref()
            ),
            DeliveryState::Inconsistent {
                mismatches: vec![MISMATCH_CANDIDATE_WITH_FAILED_SETTLEMENT]
            },
            "failed settlement with a candidate row ({abandoned:?})"
        );
    }
    assert_eq!(MISMATCH_CANDIDATE_ROW_MISSING, "candidate_row_missing");
    assert_eq!(
        MISMATCH_CANDIDATE_WITH_FAILED_SETTLEMENT,
        "candidate_with_failed_settlement"
    );
}

/// A3b's second fixture as a pure function: `no_change` is candidate OID == base OID and
/// nothing else — not the ancestry observation, not whether a commit was made.
#[test]
fn no_change_is_captured_oid_equality() {
    let row = delivery_row(Some(candidate_settled()));
    for base_is_ancestor in [true, false] {
        let same = candidate(&"b".repeat(40), base_is_ancestor);
        assert!(
            matches!(
                delivery_state(TaskStatus::Done, None, Some(&row), Some(&same), None),
                DeliveryState::NoChange { .. }
            ),
            "OID == base is no_change whatever the ancestry says ({base_is_ancestor})"
        );
        let differs = candidate(&"c".repeat(40), base_is_ancestor);
        assert!(
            matches!(
                delivery_state(TaskStatus::Done, None, Some(&row), Some(&differs), None),
                DeliveryState::Committed { .. }
            ),
            "OID != base is committed whatever the ancestry says ({base_is_ancestor})"
        );
    }
}

/// A3b's first fixture as a pure function: a candidate whose OID is the base reads
/// `no_change`, never `committed` merely because a candidate exists.
#[test]
fn unchanged_workspace_is_no_change_not_committed() {
    let row = delivery_row(Some(candidate_settled()));
    let unchanged = candidate(&"b".repeat(40), true);
    let state = delivery_state(TaskStatus::Done, None, Some(&row), Some(&unchanged), None);
    assert!(matches!(state, DeliveryState::NoChange { .. }), "{state:?}");
    assert_ne!(
        serde_json::to_value(&state).unwrap()["state"],
        json!("committed")
    );
}

fn task(kind: TaskKind, spawn: &str, context: Value) -> Task {
    Task {
        id: "attempt-1".into(),
        track_id: TRACK.into(),
        key: "k".into(),
        kind,
        goal: "goal".into(),
        context_json: context.to_string(),
        acceptance_criteria: None,
        cwd: None,
        depends_on_json: "[]".into(),
        priority: 0,
        gate_json: None,
        status: TaskStatus::Running,
        status_detail: None,
        worker_card_id: Some(CARD.into()),
        gate_result_json: None,
        gate_attempt: 0,
        gate_pid: None,
        gate_pid_starttime: None,
        gate_pid_boot_id: None,
        running_deadline_ms: None,
        context_stale_at_ms: None,
        declared_by: "planner".into(),
        spawn: spawn.into(),
        created_at_ms: 1,
        updated_at_ms: 1,
        finished_at_ms: None,
    }
}

fn lease(delivery_policy: Option<DeliveryPolicy>) -> WorkspaceLease {
    WorkspaceLease {
        lease_id: "lease-1".into(),
        card_id: CARD.into(),
        track_id: TRACK.into(),
        path: "/repo/.claude/worktrees/trk/crd".into(),
        state: "held".into(),
        boot_id: None,
        base: Some(LeaseBase {
            base_sha: "b".repeat(40),
            base_source: base::BaseSource::Head,
            base_attempt_id: None,
            canonical_path: PathBuf::from("/repo/.claude/worktrees/trk/crd"),
            git_common_dir: PathBuf::from("/repo/.git"),
        }),
        delivery_policy,
    }
}

fn facts() -> WorkerWorktreeFacts {
    WorkerWorktreeFacts {
        path: Some("/repo/.claude/worktrees/trk/crd".into()),
        state: "held".into(),
        branch: Some("neige/trk/crd".into()),
        last_commit: None,
        base_sha: Some("b".repeat(40)),
        removed: false,
    }
}

fn not_reported() -> BoundFacts {
    BoundFacts {
        delivery: DeliveryState::NotReported,
        verification: VerificationView {
            state: VerificationState::NotStarted,
            gate_attempt: 0,
            target: None,
            log_path: None,
            gate_log: None,
        },
    }
}

/// D8's first-match table: isolated, terminal, child-track, no lease, legacy lease, bound —
/// and a kernel lease with no delivery row yet is bound with `not_reported`, never legacy.
#[test]
fn candidate_binding_covers_every_row() {
    let isolated =
        json!({"neige_execution": {"version": "isolated-codex-v1", "workspace": "empty"}});
    let workspace = CandidateWorkspace {
        path: Some("/repo/.claude/worktrees/trk/crd".into()),
        branch: Some("neige/trk/crd".into()),
        lease_state: "held".into(),
        removed: false,
    };
    let bound = candidate_binding(
        &task(TaskKind::Codex, TASK_IN_TRACK_ROUTE, isolated),
        Some(&lease(Some(DeliveryPolicy::Kernel))),
        Some(&facts()),
        Some(not_reported()),
    )
    .unwrap();
    assert_eq!(
        bound,
        CandidateBinding::None {
            reason: NoBindingReason::Isolated
        }
    );
    assert_eq!(
        serde_json::to_value(&bound).unwrap(),
        json!({"binding": "none", "reason": "isolated"})
    );
    assert_eq!(
        candidate_binding(
            &task(
                TaskKind::Codex,
                TASK_IN_TRACK_ROUTE,
                json!({"neige_execution": "garbage"})
            ),
            None,
            None,
            None,
        )
        .unwrap(),
        CandidateBinding::None {
            reason: NoBindingReason::Isolated
        },
        "a present invalid selection is never legacy"
    );
    assert_eq!(
        candidate_binding(
            &task(TaskKind::Terminal, TASK_IN_TRACK_ROUTE, json!({})),
            Some(&lease(Some(DeliveryPolicy::Kernel))),
            Some(&facts()),
            Some(not_reported()),
        )
        .unwrap(),
        CandidateBinding::None {
            reason: NoBindingReason::Terminal
        }
    );
    assert_eq!(
        candidate_binding(
            &task(TaskKind::Codex, TASK_CHILD_TRACK_ROUTE, json!({})),
            None,
            None,
            None,
        )
        .unwrap(),
        CandidateBinding::None {
            reason: NoBindingReason::ChildTrack
        }
    );
    let no_lease = candidate_binding(
        &task(TaskKind::Claude, TASK_IN_TRACK_ROUTE, json!({})),
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        no_lease,
        CandidateBinding::None {
            reason: NoBindingReason::NoLease
        }
    );
    assert_eq!(
        serde_json::to_value(&no_lease).unwrap(),
        json!({"binding": "none", "reason": "no_lease"})
    );
    let legacy = candidate_binding(
        &task(TaskKind::Codex, TASK_IN_TRACK_ROUTE, json!({})),
        Some(&lease(None)),
        Some(&facts()),
        None,
    )
    .unwrap();
    assert_eq!(
        legacy,
        CandidateBinding::Unbound {
            reason: UnboundReason::LegacyLease,
            workspace: workspace.clone(),
        }
    );
    assert_eq!(
        serde_json::to_value(&legacy).unwrap(),
        json!({
            "binding": "unbound", "reason": "legacy_lease",
            "workspace": {"path": "/repo/.claude/worktrees/trk/crd", "branch": "neige/trk/crd",
                          "lease_state": "held", "removed": false}
        })
    );
    let bound = candidate_binding(
        &task(TaskKind::Claude, TASK_IN_TRACK_ROUTE, json!({})),
        Some(&lease(Some(DeliveryPolicy::Kernel))),
        Some(&facts()),
        Some(not_reported()),
    )
    .unwrap();
    assert_eq!(
        bound,
        CandidateBinding::Bound {
            producer_attempt_id: "attempt-1".into(),
            base_sha: "b".repeat(40),
            workspace,
            delivery: DeliveryState::NotReported,
            verification: not_reported().verification,
        }
    );
    let wire = serde_json::to_value(&bound).unwrap();
    assert_eq!(wire["binding"], json!("bound"));
    assert_eq!(wire["delivery"], json!({"state": "not_reported"}));
    assert_eq!(
        wire["verification"],
        json!({"state": "not_started", "gate_attempt": 0})
    );
    assert_eq!(wire["base_sha"], json!("b".repeat(40)));

    // Contract violations are errors, never a guessed binding.
    assert!(
        candidate_binding(
            &task(TaskKind::Claude, TASK_IN_TRACK_ROUTE, json!({})),
            Some(&lease(Some(DeliveryPolicy::Kernel))),
            Some(&facts()),
            None,
        )
        .is_err(),
        "a kernel lease without a derived delivery state"
    );
    assert!(
        candidate_binding(
            &task(TaskKind::Claude, TASK_IN_TRACK_ROUTE, json!({})),
            Some(&lease(Some(DeliveryPolicy::Kernel))),
            None,
            Some(not_reported()),
        )
        .is_err(),
        "a lease without worktree facts"
    );
}
