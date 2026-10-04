//! #2058 S1 — a long-running track catches up with main (`docs/architecture/2058-s1-track-catch-up.md`
//! §7 slice 2): a task declared `start: "upstream"` has the kernel fetch the upstream, start the
//! track checkout there and tell the worker to replay the track's last done commit; the delivery
//! is one commit on the upstream, which publish then puts on the PR in place of the old head.
//!
//! The world is S2's (`track_worker_cwd`): a clone of a local bare origin whose first commit `O0`
//! holds a 3-line `shared.txt`, the track worktree from the production `ensure_track_worktree`, the
//! real delivery, a test-played worker. The track's done `T` edits line 2; the upstream then moves
//! to `O1`, which edits line 1: a conflict plain `git apply --reject` rejects, which the worker
//! resolves by hand.
use std::path::{Path, PathBuf};

use calm_server::model::TaskStatus;
use serde_json::json;

use crate::git_delivery::{Fx, git, git_output};
use crate::track_publish::{origin, publish, publish_env, remote_branch};
use crate::track_worker_cwd::{
    Started, candidate_commit, declare_task, development_world_with, wait_running, wait_task,
};

const SHARED: &str = "one\ntwo\nthree\n";
const TRACK_EDIT: &str = "one\nTWO\nthree\n";
const UPSTREAM_EDIT: &str = "ONE\ntwo\nthree\n";
const RESOLVED: &str = "ONE\nTWO\nthree\n";

/// The world, and `O0`: the commit the track worktree started at.
async fn catch_up_world() -> (crate::track_worker_cwd::World, String) {
    let w = development_world_with(&[("shared.txt", SHARED)]).await;
    let o0 = git(&w.fx.worktree, &["rev-parse", "HEAD"]);
    assert_eq!(
        std::fs::read_to_string(w.fx.worktree.join("shared.txt")).unwrap(),
        SHARED
    );
    (w, o0)
}

/// A done attempt of `key` that writes `shared.txt`; returns its candidate commit.
async fn done_edit(fx: &Fx, key: &str, content: &str) -> String {
    declare_task(fx, key, json!({})).await;
    let started = wait_running(fx, key).await;
    std::fs::write(started.cwd.join("shared.txt"), content).unwrap();
    started.complete(fx).await;
    let commit = candidate_commit(fx, &started.task.id).await;
    wait_task(fx, key, |task| task.status == TaskStatus::Done).await;
    commit
}

/// Someone lands `shared.txt = content` on the origin's main; returns the new upstream commit.
fn move_upstream(fx: &Fx, content: &str) -> String {
    let parent = fx.track_root.parent().unwrap();
    let clone = parent.join("upstream-clone");
    if !clone.exists() {
        git(
            parent,
            &[
                "clone",
                "-q",
                origin(fx).to_str().unwrap(),
                "upstream-clone",
            ],
        );
        git(&clone, &["config", "user.email", "upstream@example.test"]);
        git(&clone, &["config", "user.name", "Upstream"]);
    }
    std::fs::write(clone.join("shared.txt"), content).unwrap();
    git(&clone, &["commit", "-q", "-am", "upstream edit"]);
    git(&clone, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
    git(&clone, &["rev-parse", "HEAD"])
}

async fn declare_catch_up(fx: &Fx, key: &str) {
    declare_task(fx, key, json!({"start": "upstream"})).await;
}

async fn card_prompt(fx: &Fx, started: &Started) -> String {
    sqlx::query_scalar("SELECT json_extract(payload, '$.prompt') FROM cards WHERE id = ?1")
        .bind(&started.identity.card_id)
        .fetch_one(&fx.pool())
        .await
        .unwrap()
}

fn starts_from(o1: &str) -> String {
    format!(
        "This task starts from the upstream `origin refs/heads/main` at `{o1}`, fetched by the \
         kernel."
    )
}

fn replays(m: &str, t: &str) -> String {
    format!("replay `git diff {m} {t}` here")
}

fn head(dir: &Path) -> String {
    git(dir, &["rev-parse", "HEAD"])
}

fn branch_tip(fx: &Fx, branch: &str) -> String {
    git(
        &fx.track_root,
        &["rev-parse", &format!("refs/heads/{branch}")],
    )
}

async fn failed_detail(fx: &Fx, key: &str) -> String {
    wait_task(fx, key, |task| task.status == TaskStatus::Failed)
        .await
        .status_detail
        .unwrap_or_default()
}

async fn lease_count(fx: &Fx) -> i64 {
    fx.table_count("workspace_leases").await
}

/// What the prompt tells the worker to do, as a worker with a read-only gitdir can: `git apply
/// --reject` of `git diff <m> <t>`, which rejects the conflicting hunk into `shared.txt.rej`; the
/// worker then writes the resolution and removes the `.rej`.
fn replay_and_resolve(cwd: &Path, m: &str, t: &str) {
    let diff = git_output(cwd, &["diff", m, t]);
    assert!(diff.status.success(), "{diff:?}");
    let patch: PathBuf = cwd.parent().unwrap().join("replay.patch");
    std::fs::write(&patch, &diff.stdout).unwrap();
    let applied = git_output(cwd, &["apply", "--reject", patch.to_str().unwrap()]);
    assert!(!applied.status.success(), "the hunk conflicts: {applied:?}");
    assert!(cwd.join("shared.txt.rej").exists());
    std::fs::write(cwd.join("shared.txt"), RESOLVED).unwrap();
    std::fs::remove_file(cwd.join("shared.txt.rej")).unwrap();
}

/// C1 (D5–D8, D1) — the catch-up starts at the fetched upstream O1 and its prompt names O1,
/// M = O0 and T; the worker's replay is delivered as one commit whose only parent is O1, on a
/// lease based on the upstream; publish then replaces the track's own published head with it,
/// and the PR range has no merge commit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_catch_up_starts_at_the_fetched_upstream_and_delivers_one_linear_commit() {
    let _env = publish_env(None).await;
    let (w, o0) = catch_up_world().await;
    let fx = &w.fx;
    let t = done_edit(fx, "work", TRACK_EDIT).await;
    publish(fx, "first").await.unwrap();
    let o1 = move_upstream(fx, UPSTREAM_EDIT);

    declare_catch_up(fx, "catch-up").await;
    let started = wait_running(fx, "catch-up").await;

    assert_eq!(head(&started.cwd), o1);
    assert_eq!(started.base_sha, o1);
    let source: String =
        sqlx::query_scalar("SELECT base_source FROM workspace_leases WHERE lease_id = ?1")
            .bind(&started.lease_id)
            .fetch_one(&fx.pool())
            .await
            .unwrap();
    assert_eq!(source, "upstream");
    let prompt = card_prompt(fx, &started).await;
    assert!(prompt.contains(&starts_from(&o1)), "{prompt}");
    assert!(prompt.contains(&replays(&o0, &t)), "{prompt}");
    assert!(!prompt.contains("The checkout was at"), "{prompt}");

    replay_and_resolve(&started.cwd, &o0, &t);
    started.complete(fx).await;
    let c = candidate_commit(fx, &started.task.id).await;
    wait_task(fx, "catch-up", |task| task.status == TaskStatus::Done).await;

    assert_eq!(
        git(&fx.track_root, &["rev-list", "--parents", "-n", "1", &c]),
        format!("{c} {o1}")
    );
    assert_eq!(
        git(&fx.track_root, &["show", &format!("{c}:shared.txt")]),
        RESOLVED.trim()
    );
    let result = publish(fx, "second").await.unwrap();
    assert_eq!(result["head_sha"], json!(c));
    assert_eq!(remote_branch(fx).as_deref(), Some(c.as_str()));
    assert_eq!(
        git(
            &fx.track_root,
            &["log", "--merges", "--format=%H", &format!("{o1}..{c}")]
        ),
        ""
    );
}

/// #2112 — a catch-up fetches and starts from the track branch's own upstream: the primary
/// checkout on another branch whose upstream has moved elsewhere changes neither.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_catch_up_ignores_the_branch_the_primary_checkout_moved_to() {
    let (w, _) = catch_up_world().await;
    let fx = &w.fx;
    done_edit(fx, "work", TRACK_EDIT).await;
    let o1 = move_upstream(fx, UPSTREAM_EDIT);
    git(&fx.track_root, &["checkout", "-q", "-b", "side"]);
    git(
        &fx.track_root,
        &["commit", "-q", "--allow-empty", "-m", "side work"],
    );
    git(&fx.track_root, &["push", "-q", "-u", "origin", "side"]);
    let side = head(&fx.track_root);

    declare_catch_up(fx, "catch-up").await;
    let started = wait_running(fx, "catch-up").await;

    assert_eq!(
        head(&started.cwd),
        o1,
        "not the primary's side branch {side}"
    );
    assert_eq!(started.base_sha, o1);
    let prompt = card_prompt(fx, &started).await;
    assert!(prompt.contains(&starts_from(&o1)), "{prompt}");
}

/// C2 (D6 step 1) — the first catch-up fails with partial work F on O1; the second replays T, the
/// last done commit, not F, starts at O1 again, and names where the checkout was.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_catch_up_replays_the_last_done_commit() {
    let (w, o0) = catch_up_world().await;
    let fx = &w.fx;
    let t = done_edit(fx, "work", TRACK_EDIT).await;
    let o1 = move_upstream(fx, UPSTREAM_EDIT);

    declare_catch_up(fx, "catch-up-1").await;
    let first = wait_running(fx, "catch-up-1").await;
    assert_eq!(head(&first.cwd), o1);
    std::fs::write(first.cwd.join("partial.txt"), "half a replay\n").unwrap();
    first.fail(fx, "conflicts left").await;
    let f = candidate_commit(fx, &first.task.id).await;
    wait_task(fx, "catch-up-1", |task| task.status == TaskStatus::Failed).await;
    assert_eq!(head(&fx.worktree), f);

    declare_catch_up(fx, "catch-up-2").await;
    let second = wait_running(fx, "catch-up-2").await;

    assert_eq!(head(&second.cwd), o1);
    assert_eq!(second.base_sha, o1);
    let prompt = card_prompt(fx, &second).await;
    assert!(prompt.contains(&replays(&o0, &t)), "{prompt}");
    assert!(!prompt.contains(&format!("git diff {o0} {f}")), "{prompt}");
    assert!(
        prompt.contains(&format!(
            "The checkout was at `{f}`; commits after `{t}` are not replayed."
        )),
        "{prompt}"
    );
}

/// C3 (D5, D6 step 2) — the origin is unreachable after the worktree is made, so the kernel's
/// fetch fails: the catch-up is refused with `track-upstream-unavailable`, no lease is taken, and
/// HEAD stays at T.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_catch_up_whose_fetch_fails_is_refused() {
    let (w, _) = catch_up_world().await;
    let fx = &w.fx;
    let t = done_edit(fx, "work", TRACK_EDIT).await;
    let leases = lease_count(fx).await;
    std::fs::rename(origin(fx), origin(fx).with_extension("gone")).unwrap();

    declare_catch_up(fx, "catch-up").await;
    let detail = failed_detail(fx, "catch-up").await;

    assert!(
        detail.starts_with("spawn-failed: refused: track-upstream-unavailable: "),
        "{detail}"
    );
    assert!(
        detail.ends_with(&format!(
            "HEAD is at {t}; the track's work is in {t}; declare another start:\"upstream\" task"
        )),
        "{detail}"
    );
    assert_eq!(lease_count(fx).await, leases, "no lease");
    assert_eq!(head(&fx.worktree), t);
}

/// C4 (D6 steps 1 and 2) — with no done attempt there is nothing to replay; a managed track with
/// a done attempt passes the branch check (its worker branch is `main`) and has no upstream.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_catch_up_without_a_done_attempt_is_refused() {
    let (w, o0) = catch_up_world().await;
    let fx = &w.fx;

    declare_catch_up(fx, "catch-up").await;
    let detail = failed_detail(fx, "catch-up").await;

    assert_eq!(
        detail,
        "spawn-failed: refused: track-nothing-to-replay: no attempt of this track is done; \
         declare the task without start"
    );
    assert_eq!(head(&fx.worktree), o0);

    done_edit(fx, "work", TRACK_EDIT).await;
    let managed = fx.workspace_root.join("managed");
    sqlx::query(
        "UPDATE tracks SET workspace_kind = 'managed', workspace_path = ?1, \
         workspace_worktree_path = NULL WHERE id = ?2",
    )
    .bind(managed.to_str().unwrap())
    .bind(fx.track())
    .execute(&fx.pool())
    .await
    .unwrap();
    declare_catch_up(fx, "catch-up-managed").await;
    let detail = failed_detail(fx, "catch-up-managed").await;
    assert!(
        detail.starts_with("spawn-failed: refused: track-upstream-unavailable: "),
        "{detail}"
    );
    assert_eq!(
        head(&managed),
        git(&managed, &["rev-parse", "refs/heads/main"])
    );
}

/// C6 (D6 step 0) — the idle worktree was switched to a clean human branch with an unpublished
/// commit: the catch-up is refused naming that branch, and neither the human branch nor
/// `neige/track-<id>` moves.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_catch_up_on_a_foreign_branch_is_refused_and_moves_nothing() {
    let (w, _) = catch_up_world().await;
    let fx = &w.fx;
    let t = done_edit(fx, "work", TRACK_EDIT).await;
    move_upstream(fx, UPSTREAM_EDIT);
    git(&fx.worktree, &["switch", "-q", "-c", "human"]);
    std::fs::write(fx.worktree.join("human.txt"), "mine\n").unwrap();
    git(&fx.worktree, &["add", "human.txt"]);
    git(&fx.worktree, &["commit", "-q", "-m", "unpublished"]);
    let human = head(&fx.worktree);

    declare_catch_up(fx, "catch-up").await;
    let detail = failed_detail(fx, "catch-up").await;

    assert!(
        detail.starts_with(&format!(
            "spawn-failed: refused: track-worktree-unavailable: the checkout is on \
             refs/heads/human, not refs/heads/{}",
            fx.worker_branch()
        )),
        "{detail}"
    );
    assert_eq!(branch_tip(fx, "human"), human);
    assert_eq!(branch_tip(fx, &fx.worker_branch()), t);
    assert_eq!(head(&fx.worktree), human);
}

/// C7 (D6 2a) — the worker's spawn fails after prepare reset the checkout to O1: the task fails
/// naming the upstream it is at and the commit that holds the track's work, and
/// `neige/track-<id>` stays at O1.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_catch_up_whose_spawn_fails_names_the_upstream_and_the_work() {
    let (w, _) = catch_up_world().await;
    let fx = &w.fx;
    let t = done_edit(fx, "work", TRACK_EDIT).await;
    let o1 = move_upstream(fx, UPSTREAM_EDIT);
    w.shared.fail_next_thread_start_for_test();

    declare_catch_up(fx, "catch-up").await;
    let detail = failed_detail(fx, "catch-up").await;

    assert!(detail.starts_with("spawn-failed: "), "{detail}");
    assert!(
        detail.contains(&format!(
            "HEAD is at upstream {o1}; the track's work is in {t}; declare another \
             start:\"upstream\" task"
        )),
        "{detail}"
    );
    assert_eq!(branch_tip(fx, &fx.worker_branch()), o1);
}
