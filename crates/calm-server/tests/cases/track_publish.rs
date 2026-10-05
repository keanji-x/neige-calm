//! #1830 S3 — `neige_dev_publish` (`docs/architecture/1830-s3-push-pr-reclaim.md` §5): the
//! Planner of an attached track pushes `neige/track-<id>` to its own upstream URL (the checkout's
//! when the track worktree was made, #2112) and opens or reuses the PR, only when the branch tip is the commit of a `done` attempt of this track; no
//! kernel git script shows its repository's code a GitHub token.
//!
//! The world is S2's (`track_worker_cwd::world`): a clone of a local bare origin, the track
//! worktree from the production `ensure_track_worktree`, candidates from the real delivery with a
//! test-played worker. The `gh` shim is first on PATH and takes `--repo` = the bare origin's path;
//! nothing here reaches a real repository or the network.
use std::path::{Path, PathBuf};

use calm_server::model::TaskStatus;
use calm_server::plugin_host::mcp::RpcError;
use serde_json::{Value, json};

use crate::git_delivery::{Fx, git, git_output, ref_target, write_executable};
use crate::mcp_track_report::{call_tool, planner_identity};
use crate::support::forge_env::{EnvGuard, FORGE_ENV_LOCK};
use crate::support::gh_shim::{run_gh, write_gh_shim};
use crate::track_worker_cwd::{
    candidate_commit, declare_task, development_world, wait_running, wait_task,
};

const TOOL: &str = "neige_dev_publish";

/// The process environment of one test: the `gh` shim first on PATH, and optionally a
/// `GH_TOKEN` in the kernel's own environment (which the forge child inherits).
pub(super) struct PublishEnv {
    _token: Option<EnvGuard>,
    _path: EnvGuard,
    _shim_dir: tempfile::TempDir,
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

pub(super) async fn publish_env(token: Option<&str>) -> PublishEnv {
    let lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let shim_dir = tempfile::Builder::new().prefix("gh-").tempdir().unwrap();
    write_gh_shim(shim_dir.path());
    let mut path = std::ffi::OsString::from(shim_dir.path());
    if let Some(current) = std::env::var_os("PATH") {
        path.push(":");
        path.push(current);
    }
    PublishEnv {
        _path: EnvGuard::set("PATH", path),
        _token: token.map(|token| EnvGuard::set("GH_TOKEN", token)),
        _shim_dir: shim_dir,
        _lock: lock,
    }
}

pub(super) fn origin(fx: &Fx) -> PathBuf {
    fx.track_root.parent().unwrap().join("origin.git")
}

pub(super) fn gh_log(fx: &Fx) -> String {
    let mut state = origin(fx).into_os_string();
    state.push(".shimstate/gh.log");
    std::fs::read_to_string(state).unwrap_or_default()
}

pub(super) fn remote_branch(fx: &Fx) -> Option<String> {
    ref_target(&origin(fx), &format!("refs/heads/{}", fx.worker_branch()))
}

pub(super) async fn publish(fx: &Fx, key: &str) -> Result<Value, RpcError> {
    publish_titled(fx, key, "Publish the track", "Done work.").await
}

async fn publish_titled(fx: &Fx, key: &str, title: &str, body: &str) -> Result<Value, RpcError> {
    call_tool(
        &fx.boot,
        TOOL,
        planner_identity(&fx.boot),
        json!({"idempotency_key": key, "title": title, "body": body}),
    )
    .await
}

/// The shim's record of PR `number`'s title and body, as the last create or edit set them.
fn shim_pr_text(fx: &Fx, number: u64) -> (String, String) {
    let mut state = origin(fx).into_os_string();
    state.push(format!(".shimstate/prs/{number}"));
    let read = |field: &str| {
        let text = std::fs::read_to_string(Path::new(&state).join(field)).unwrap();
        text.strip_suffix('\n').unwrap_or(&text).to_string()
    };
    (read("title"), read("body"))
}

/// A `done` attempt of `key` that writes `<key>.txt`; returns its candidate commit.
pub(super) async fn done_candidate(fx: &Fx, key: &str) -> String {
    declare_task(fx, key, json!({})).await;
    let started = wait_running(fx, key).await;
    std::fs::write(started.cwd.join(format!("{key}.txt")), key).unwrap();
    started.complete(fx).await;
    let commit = candidate_commit(fx, &started.task.id).await;
    wait_task(fx, key, |task| task.status == TaskStatus::Done).await;
    commit
}

pub(super) async fn pr_opened_heads(fx: &Fx) -> Vec<Value> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT payload FROM events WHERE kind = 'forge.pr.opened' ORDER BY id")
            .fetch_all(&fx.pool())
            .await
            .unwrap();
    rows.into_iter()
        .map(|(payload,)| serde_json::from_str::<Value>(&payload).unwrap()["head_sha"].clone())
        .collect()
}

async fn publish_op_count(fx: &Fx) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM operations WHERE idempotency_key LIKE '%:track.publish:%'",
    )
    .fetch_one(&fx.pool())
    .await
    .unwrap()
}

/// The shim's view of the PR of the track branch, read after every log assertion.
pub(super) fn shim_pr(fx: &Fx) -> Value {
    let gh = which_gh();
    let origin = origin(fx);
    let output = run_gh(
        &gh,
        &[
            "pr",
            "view",
            &fx.worker_branch(),
            "--repo",
            origin.to_str().unwrap(),
            "--json",
            "number,headRefOid",
        ],
    );
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

fn which_gh() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("gh"))
        .find(|gh| gh.is_file())
        .unwrap()
}

fn refused(result: Result<Value, RpcError>) -> String {
    let error = result.expect_err("the publish must be refused");
    assert_eq!(error.code, -32409, "{error:?}");
    error.message
}

fn hook(fx: &Fx, name: &str, body: &str) {
    write_executable(&fx.track_root.join(".git/hooks").join(name), body);
}

/// A hook that writes `${GH_TOKEN-unset}` to `out`.
fn token_probe_hook(out: &Path) -> String {
    format!(
        "#!/bin/sh\nprintf '%s' \"${{GH_TOKEN-unset}}\" > '{}'\n",
        out.display()
    )
}

/// P1 (D5, D6) — a done attempt's candidate C is pushed, the PR is opened on it, and the one
/// `forge.pr.opened` carries C. The live stdout completed the op: the shim saw no probe. The
/// user's clone keeps every ref it had.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn publish_pushes_the_candidate_and_opens_its_pr() {
    let _env = publish_env(None).await;
    let w = development_world().await;
    let fx = &w.fx;
    let c = done_candidate(fx, "a").await;
    let clone_refs = git(&fx.track_root, &["for-each-ref"]);
    let clone_head = git(&fx.track_root, &["rev-parse", "HEAD"]);

    let result = publish(fx, "p1").await.unwrap();

    assert_eq!(result["ok"], json!(true));
    assert_eq!(result["pr_number"], json!(1));
    assert_eq!(result["pr_action"], json!("created"));
    assert_eq!(result["head_sha"], json!(c));
    assert_eq!(result["base"], json!("main"));
    assert_eq!(remote_branch(fx).as_deref(), Some(c.as_str()));
    assert_eq!(pr_opened_heads(fx).await, vec![json!(c)]);
    let log = gh_log(fx);
    assert!(log.contains(" pr create "), "{log}");
    assert!(!log.contains("headRefOid,state"), "no probe ran: {log}");
    assert_eq!(shim_pr(fx)["headRefOid"], json!(c));
    assert_eq!(git(&fx.track_root, &["for-each-ref"]), clone_refs);
    assert_eq!(git(&fx.track_root, &["rev-parse", "HEAD"]), clone_head);
    assert_eq!(git(&fx.track_root, &["status", "--porcelain"]), "");
}

/// P2 (D3) — a commit D the Planner made after the last attempt is refused, naming D and the
/// latest candidate C; nothing is pushed and no operation exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn publish_refuses_a_commit_made_after_the_last_attempt() {
    let _env = publish_env(None).await;
    let w = development_world().await;
    let fx = &w.fx;
    let c = done_candidate(fx, "a").await;
    // The Planner's `git.commit` (`git add -A` + commit in its cwd, the track worktree).
    std::fs::write(fx.worktree.join("planner.txt"), "unverified\n").unwrap();
    git(&fx.worktree, &["add", "-A"]);
    git(&fx.worktree, &["commit", "-q", "-m", "planner edit"]);
    let d = git(&fx.worktree, &["rev-parse", "HEAD"]);

    let message = refused(publish(fx, "p2").await);

    assert!(
        message.starts_with("refused: publish-not-a-candidate: "),
        "{message}"
    );
    assert!(message.contains(&d) && message.contains(&c), "{message}");
    assert_eq!(remote_branch(fx), None);
    assert_eq!(publish_op_count(fx).await, 0);
}

/// P3 (D3) — the only attempt failed: its candidate is refused, naming the attempt and `failed`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn publish_refuses_a_failed_attempts_candidate() {
    let _env = publish_env(None).await;
    let w = development_world().await;
    let fx = &w.fx;
    declare_task(fx, "a", json!({})).await;
    let a = wait_running(fx, "a").await;
    std::fs::write(a.cwd.join("a.txt"), "a\n").unwrap();
    a.fail(fx, "could not finish").await;
    candidate_commit(fx, &a.task.id).await;

    let message = refused(publish(fx, "p3").await);

    assert!(
        message.starts_with("refused: publish-candidate-not-done: "),
        "{message}"
    );
    assert!(
        message.contains(&format!("attempt {}, which is failed", a.task.id)),
        "{message}"
    );
    assert_eq!(remote_branch(fx), None);
    assert_eq!(publish_op_count(fx).await, 0);
}

/// C1 (D4) — the kernel holds `GH_TOKEN`: the repository's `pre-push` hook runs without it, and
/// gh still gets it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn publish_git_never_sees_a_github_token() {
    let _env = publish_env(Some("sentinel")).await;
    let w = development_world().await;
    let fx = &w.fx;
    let c = done_candidate(fx, "a").await;
    let seen = fx.track_root.parent().unwrap().join("pre-push-token");
    hook(fx, "pre-push", &token_probe_hook(&seen));

    let result = publish(fx, "c1").await.unwrap();

    assert_eq!(result["head_sha"], json!(c));
    assert_eq!(std::fs::read_to_string(&seen).unwrap(), "unset");
    let log = gh_log(fx);
    assert!(log.contains(" pr create "), "{log}");
    assert!(
        log.lines().all(|line| line.starts_with("GH_TOKEN=set ")),
        "{log}"
    );
}

/// C2 (D4) — the same boundary for the delivery: a `pre-commit` hook in a real delivery runs
/// without the kernel's `GH_TOKEN`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delivery_commit_hook_never_sees_a_github_token() {
    let _env = publish_env(Some("sentinel")).await;
    let w = development_world().await;
    let fx = &w.fx;
    let seen = fx.track_root.parent().unwrap().join("pre-commit-token");
    hook(fx, "pre-commit", &token_probe_hook(&seen));

    done_candidate(fx, "a").await;

    assert_eq!(std::fs::read_to_string(&seen).unwrap(), "unset");
}

/// D7 and D2 — a track without its own worktree, then a track branch without an upstream, are
/// refused before any operation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn publish_refuses_without_a_worktree_or_an_upstream() {
    let _env = publish_env(None).await;
    let w = development_world().await;
    let fx = &w.fx;
    done_candidate(fx, "a").await;

    git(&fx.worktree, &["branch", "--unset-upstream"]);
    let message = refused(publish(fx, "no-upstream").await);
    assert_eq!(
        message,
        format!(
            "refused: publish-no-upstream: {} has no upstream remote to push to; set one with \
             git -C {} branch --set-upstream-to and retry",
            fx.worker_branch(),
            fx.worktree.display()
        )
    );

    sqlx::query("UPDATE tracks SET workspace_worktree_path = NULL WHERE id = ?1")
        .bind(fx.track())
        .execute(&fx.pool())
        .await
        .unwrap();
    let message = refused(publish(fx, "no-worktree").await);
    assert!(
        message.starts_with("refused: publish-needs-track-worktree: "),
        "{message}"
    );
    assert_eq!(remote_branch(fx), None);
    assert_eq!(publish_op_count(fx).await, 0);
}

/// #2112 — the push target and the PR base are the track branch's own upstream, recorded when the
/// track worktree was made: the primary checkout moving to another branch, without an upstream
/// and then with one of its own, changes neither, and neither does the track worktree's HEAD.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn publish_ignores_the_branch_the_primary_checkout_moved_to() {
    let _env = publish_env(None).await;
    let w = development_world().await;
    let fx = &w.fx;
    let a = done_candidate(fx, "a").await;

    git(&fx.track_root, &["checkout", "-q", "-b", "side"]);
    let first = publish(fx, "p1").await.unwrap();
    assert_eq!(first["base"], json!("main"), "{first}");
    assert_eq!(remote_branch(fx).as_deref(), Some(a.as_str()));

    git(&fx.track_root, &["push", "-q", "-u", "origin", "side"]);
    let b = done_candidate(fx, "b").await;
    let second = publish(fx, "p2").await.unwrap();
    assert_eq!(second["base"], json!("main"), "{second}");
    assert_eq!(remote_branch(fx).as_deref(), Some(b.as_str()));
    let log = gh_log(fx);
    assert!(log.contains(" --base main "), "{log}");
    assert!(!log.contains(" --base side "), "{log}");

    // The track worktree itself on another branch that tracks `side`: the upstream is still the
    // track branch's own, read by its name.
    git(
        &fx.worktree,
        &["switch", "-q", "-c", "elsewhere", "--track", "origin/side"],
    );
    let third = publish(fx, "p3").await.unwrap();
    assert_eq!(third["base"], json!("main"), "{third}");
}

/// A second done candidate is pushed fast-forward and reuses the open PR.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_candidate_is_pushed_and_reuses_the_pr() {
    let _env = publish_env(None).await;
    let w = development_world().await;
    let fx = &w.fx;
    let c1 = done_candidate(fx, "a").await;
    publish(fx, "first").await.unwrap();
    let c2 = done_candidate(fx, "b").await;

    let result = publish(fx, "second").await.unwrap();

    assert_eq!(result["pr_number"], json!(1));
    assert_eq!(result["pr_action"], json!("reused"));
    assert_eq!(result["head_sha"], json!(c2));
    assert_eq!(remote_branch(fx).as_deref(), Some(c2.as_str()));
    assert_eq!(pr_opened_heads(fx).await, vec![json!(c1), json!(c2)]);
    let log = gh_log(fx);
    assert_eq!(log.matches(" pr create ").count(), 1, "{log}");
    assert_eq!(shim_pr(fx), json!({"number": 1, "headRefOid": c2}));
}

/// #2139 (#2122 item 6) — a publish that reuses the open PR gives it this call's title and body
/// instead of silently keeping the first publish's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_publish_that_reuses_the_open_pr_sets_its_title_and_body() {
    let _env = publish_env(None).await;
    let w = development_world().await;
    let fx = &w.fx;
    done_candidate(fx, "a").await;
    let first = publish_titled(fx, "first", "First title", "First body.")
        .await
        .unwrap();
    assert_eq!(first["pr_action"], json!("created"));
    assert_eq!(
        shim_pr_text(fx, 1),
        ("First title".into(), "First body.".into())
    );
    done_candidate(fx, "b").await;

    let second = publish_titled(fx, "second", "Second title", "Second body,\nnow longer.")
        .await
        .unwrap();

    assert_eq!(second["pr_number"], json!(1));
    assert_eq!(second["pr_action"], json!("reused"));
    let log = gh_log(fx);
    assert_eq!(log.matches(" pr create ").count(), 1, "{log}");
    assert_eq!(log.matches(" pr edit ").count(), 1, "{log}");
    assert_eq!(
        shim_pr_text(fx, 1),
        ("Second title".into(), "Second body,\nnow longer.".into())
    );
}

/// D6 — the key names one publish: repeated, it replays (same op, nothing pushed again); over a
/// tip that moved since, it is refused and the remote keeps the first commit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_same_key_replays_and_refuses_a_moved_tip() {
    let _env = publish_env(None).await;
    let w = development_world().await;
    let fx = &w.fx;
    let c1 = done_candidate(fx, "a").await;
    let first = publish(fx, "k").await.unwrap();

    let replay = publish(fx, "k").await.unwrap();
    assert_eq!(replay["op_id"], first["op_id"]);
    assert_eq!(replay["head_sha"], json!(c1));
    assert_eq!(publish_op_count(fx).await, 1);

    done_candidate(fx, "b").await;
    let message = refused(publish(fx, "k").await);
    assert!(
        message.starts_with("refused: publish-key-reused: "),
        "{message}"
    );
    assert!(
        message.contains(":track.publish:k already used with different payload"),
        "{message}"
    );
    assert_eq!(remote_branch(fx).as_deref(), Some(c1.as_str()));
    assert_eq!(publish_op_count(fx).await, 1);
}

/// A clone of the origin beside the user's checkout, on `branch` of the origin (else on main),
/// with one commit no attempt made: someone else's work. Returns the clone and that commit.
fn foreign_clone(fx: &Fx, branch: Option<&str>) -> (PathBuf, String) {
    let other = fx.track_root.parent().unwrap().join("other-clone");
    git(
        fx.track_root.parent().unwrap(),
        &["clone", "-q", origin(fx).to_str().unwrap(), "other-clone"],
    );
    if let Some(branch) = branch {
        git(
            &other,
            &["checkout", "-q", "-b", "work", &format!("origin/{branch}")],
        );
    }
    git(&other, &["config", "user.email", "other@example.test"]);
    git(&other, &["config", "user.name", "Other"]);
    std::fs::write(other.join("other.txt"), "someone else's work\n").unwrap();
    git(&other, &["add", "other.txt"]);
    git(&other, &["commit", "-q", "-m", "someone else's work"]);
    let commit = git(&other, &["rev-parse", "HEAD"]);
    (other, commit)
}

/// R2 (#2058 D1, D2) — a remote branch at a commit no attempt of this track made is never
/// overwritten: the script exits 22 before any push or gh, the publish fails naming the code, and
/// the remote keeps the foreign commit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_publish_over_a_foreign_commit_fails_and_leaves_the_remote() {
    let _env = publish_env(None).await;
    let w = development_world().await;
    let fx = &w.fx;
    done_candidate(fx, "a").await;
    let (other, foreign) = foreign_clone(fx, None);
    let refspec = format!("HEAD:refs/heads/{}", fx.worker_branch());
    git(&other, &["push", "-q", "origin", &refspec]);

    let error = publish(fx, "nff").await.expect_err("a foreign head fails");

    assert_eq!(error.code, -32409, "{error:?}");
    assert!(error.message.starts_with("publish-failed: "), "{error:?}");
    assert!(
        error
            .message
            .contains("forge action exited with code 22; probe reports not landed"),
        "{error:?}"
    );
    assert_eq!(remote_branch(fx).as_deref(), Some(foreign.as_str()));
    assert!(pr_opened_heads(fx).await.is_empty());
    assert_eq!(gh_log(fx), "", "no gh invocation");
}

/// Copy `attempt`'s task, delivery and candidate rows as an attempt of a second track (a copy of
/// this track's row), with the candidate at `commit`. Returns the second track's id.
async fn candidate_of_another_track(fx: &Fx, attempt: &str, commit: &str) -> String {
    let other = format!("{}-other", fx.track());
    let other_attempt = format!("{other}:copied");
    let mut conn = fx.pool().acquire().await.unwrap();
    for (sql, binds) in [
        (
            "CREATE TEMP TABLE r5_track AS SELECT * FROM tracks WHERE id = ?1",
            vec![fx.track().to_string()],
        ),
        ("UPDATE r5_track SET id = ?1", vec![other.clone()]),
        ("INSERT INTO tracks SELECT * FROM r5_track", vec![]),
        (
            "CREATE TEMP TABLE r5_task AS SELECT * FROM tasks WHERE id = ?1",
            vec![attempt.to_string()],
        ),
        (
            "UPDATE r5_task SET id = ?1, track_id = ?2",
            vec![other_attempt.clone(), other.clone()],
        ),
        ("INSERT INTO tasks SELECT * FROM r5_task", vec![]),
        (
            "CREATE TEMP TABLE r5_delivery AS SELECT * FROM task_git_deliveries \
             WHERE producer_attempt_id = ?1",
            vec![attempt.to_string()],
        ),
        (
            "UPDATE r5_delivery SET delivery_id = 'r5-' || delivery_id, track_id = ?1, \
             producer_attempt_id = ?2, operation_key = 'r5-' || operation_key, \
             forge_idempotency_key = 'r5-' || forge_idempotency_key",
            vec![other.clone(), other_attempt.clone()],
        ),
        (
            "INSERT INTO task_git_deliveries SELECT * FROM r5_delivery",
            vec![],
        ),
        (
            "CREATE TEMP TABLE r5_candidate AS SELECT * FROM task_candidates \
             WHERE producer_attempt_id = ?1",
            vec![attempt.to_string()],
        ),
        (
            "UPDATE r5_candidate SET candidate_id = 'r5-' || candidate_id, track_id = ?1, \
             producer_attempt_id = ?2, commit_sha = ?3, ref_name = 'r5-' || ref_name",
            vec![other.clone(), other_attempt.clone(), commit.to_string()],
        ),
        (
            "INSERT INTO task_candidates SELECT * FROM r5_candidate",
            vec![],
        ),
    ] {
        let mut query = sqlx::query(sql);
        for bind in binds {
            query = query.bind(bind);
        }
        query
            .execute(&mut *conn)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
    other
}

/// R5 (#2058 D1) — the remote branch is at X, the done candidate of ANOTHER track: the lease list
/// is this track's candidates only, so the script exits 22 before any push or gh and the remote
/// keeps X.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn publish_refuses_a_remote_head_that_is_another_tracks_candidate() {
    let _env = publish_env(None).await;
    let w = development_world().await;
    let fx = &w.fx;
    declare_task(fx, "a", json!({})).await;
    let a = wait_running(fx, "a").await;
    std::fs::write(a.cwd.join("a.txt"), "a\n").unwrap();
    a.complete(fx).await;
    candidate_commit(fx, &a.task.id).await;
    wait_task(fx, "a", |task| task.status == TaskStatus::Done).await;
    let (other, x) = foreign_clone(fx, None);
    git(
        &other,
        &[
            "push",
            "-q",
            "origin",
            &format!("HEAD:refs/heads/{}", fx.worker_branch()),
        ],
    );
    let other_track = candidate_of_another_track(fx, &a.task.id, &x).await;
    let owners: Vec<(String,)> =
        sqlx::query_as("SELECT track_id FROM task_candidates WHERE commit_sha = ?1")
            .bind(&x)
            .fetch_all(&fx.pool())
            .await
            .unwrap();
    assert_eq!(
        owners,
        vec![(other_track,)],
        "X is another track's candidate"
    );

    let error = publish(fx, "r5").await.expect_err("X is not this track's");

    assert!(
        error
            .message
            .contains("forge action exited with code 22; probe reports not landed"),
        "{error:?}"
    );
    assert_eq!(remote_branch(fx).as_deref(), Some(x.as_str()));
    assert!(pr_opened_heads(fx).await.is_empty());
    assert_eq!(gh_log(fx), "", "no gh invocation");
}

/// R1 (#2058 D1) — the published head P is this track's own candidate, so a done candidate C'
/// that does not contain P (the checkout was reset to the upstream, as a catch-up does) replaces
/// it: the remote branch and the PR head are C', and the PR is the same one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn publish_replaces_its_own_branch_with_a_non_descendant_candidate() {
    let _env = publish_env(None).await;
    let w = development_world().await;
    let fx = &w.fx;
    let p = done_candidate(fx, "a").await;
    publish(fx, "first").await.unwrap();
    git(&fx.worktree, &["reset", "-q", "--hard", "origin/main"]);
    let c = done_candidate(fx, "b").await;
    let is_ancestor = git_output(&fx.worktree, &["merge-base", "--is-ancestor", &p, &c]);
    assert!(!is_ancestor.status.success(), "C' must not contain P");

    let result = publish(fx, "second").await.unwrap();

    assert_eq!(result["pr_number"], json!(1));
    assert_eq!(result["head_sha"], json!(c));
    assert_eq!(remote_branch(fx).as_deref(), Some(c.as_str()));
    assert_eq!(pr_opened_heads(fx).await, vec![json!(p), json!(c)]);
    let log = gh_log(fx);
    assert_eq!(log.matches(" pr create ").count(), 1, "{log}");
    assert_eq!(shim_pr(fx), json!({"number": 1, "headRefOid": c}));
}

/// A `git` first on PATH that runs the real one. Only the publish script's lease read
/// (`ls-remote <url> refs/heads/<b>`, not the `ls-remote --get-url` the destination read runs)
/// while `trigger` exists is different: it lets the real read answer, then pushes `other`'s HEAD
/// to that branch (a writer landing between the lease read and the push), removes `trigger`, and
/// prints the answer.
fn write_racing_git_shim(dir: &Path, trigger: &Path, other: &Path) {
    let real = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("git"))
        .find(|git| git.is_file())
        .unwrap();
    write_executable(
        &dir.join("git"),
        &format!(
            "#!/bin/sh\n\
             if [ \"$1\" = ls-remote ] && [ \"$#\" -eq 3 ] && [ -e '{trigger}' ]; then\n\
             case \"$3\" in refs/heads/*)\n\
             out=$('{real}' \"$@\") || exit $?\n\
             '{real}' -C '{other}' push -q origin \"HEAD:$3\" >/dev/null 2>&1 || exit 97\n\
             rm -f '{trigger}'\n\
             printf '%s\\n' \"$out\"\n\
             exit 0;;\n\
             esac\n\
             fi\n\
             exec '{real}' \"$@\"\n",
            trigger = trigger.display(),
            real = real.display(),
            other = other.display(),
        ),
    );
}

/// R4 (#2058 D1) — a writer that lands after the script read the remote head (P, this track's
/// own) and before its push is never overwritten: the lease names P, so the push is rejected,
/// the publish fails, and the remote keeps the foreign commit X.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_writer_between_the_lease_read_and_the_push_is_never_overwritten() {
    let _env = publish_env(None).await;
    let w = development_world().await;
    let fx = &w.fx;
    done_candidate(fx, "a").await;
    publish(fx, "first").await.unwrap();
    let c = done_candidate(fx, "b").await;
    let (other, foreign) = foreign_clone(fx, Some(&fx.worker_branch()));
    let shim_dir = tempfile::Builder::new().prefix("git-").tempdir().unwrap();
    let trigger = fx.track_root.parent().unwrap().join("race-trigger");
    write_racing_git_shim(shim_dir.path(), &trigger, &other);
    let mut path = std::ffi::OsString::from(shim_dir.path());
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap());
    let _git_shim = EnvGuard::set("PATH", path);
    std::fs::write(&trigger, "").unwrap();

    let error = publish(fx, "raced")
        .await
        .expect_err("the lease rejects the push");

    assert!(!trigger.exists(), "the race ran");
    assert!(error.message.starts_with("publish-failed: "), "{error:?}");
    assert_eq!(remote_branch(fx).as_deref(), Some(foreign.as_str()));
    assert_ne!(foreign, c);
}

/// A failed publish spends its key (the same key answers the same failure); a new key runs again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_publish_is_retried_under_a_new_key() {
    let _env = publish_env(None).await;
    let w = development_world().await;
    let fx = &w.fx;
    let c = done_candidate(fx, "a").await;
    hook(fx, "pre-push", "#!/bin/sh\nexit 1\n");

    let first = publish(fx, "k1")
        .await
        .expect_err("the hook rejects the push");
    assert!(first.message.starts_with("publish-failed: "), "{first:?}");
    std::fs::remove_file(fx.track_root.join(".git/hooks/pre-push")).unwrap();
    let again = publish(fx, "k1").await.expect_err("the key is spent");
    assert_eq!(again.message, first.message);
    assert_eq!(remote_branch(fx), None);

    let result = publish(fx, "k2").await.unwrap();
    assert_eq!(result["head_sha"], json!(c));
    assert_eq!(remote_branch(fx).as_deref(), Some(c.as_str()));
    assert_eq!(publish_op_count(fx).await, 2);
}

/// #1873 item 3: `url` is the PR's web page as gh reports it, not the remote the push went to.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_publish_result_url_is_the_pr_url() {
    let _env = publish_env(None).await;
    let w = development_world().await;
    let fx = &w.fx;
    done_candidate(fx, "a").await;

    let result = publish(fx, "url").await.unwrap();

    let pr_number = shim_pr(fx)["number"].clone();
    assert_eq!(result["pr_number"], pr_number);
    assert_eq!(
        result["url"],
        json!(format!("https://github.invalid/shim/pull/{pr_number}"))
    );
}

/// D2 — the push goes to the upstream's fetch URL, not to a `pushurl` the remote also has.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_push_goes_to_the_upstream_url_not_the_pushurl() {
    let _env = publish_env(None).await;
    let w = development_world().await;
    let fx = &w.fx;
    let c = done_candidate(fx, "a").await;
    let elsewhere = fx.track_root.parent().unwrap().join("elsewhere.git");
    git(
        fx.track_root.parent().unwrap(),
        &["init", "-q", "--bare", elsewhere.to_str().unwrap()],
    );
    git(
        &fx.track_root,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            elsewhere.to_str().unwrap(),
        ],
    );

    publish(fx, "pushurl").await.unwrap();

    assert_eq!(remote_branch(fx).as_deref(), Some(c.as_str()));
    let refs = git_output(&elsewhere, &["for-each-ref"]);
    assert!(refs.status.success() && refs.stdout.is_empty(), "{refs:?}");
}
