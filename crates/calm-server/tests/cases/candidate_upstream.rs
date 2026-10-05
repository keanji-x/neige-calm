//! #1777 B — `neige_task_ls.candidate.upstream`: every bound candidate reads the commit, as last
//! known now, of the upstream of the branch the Track's own checkout (the track worktree, #2112) is
//! on now, and how many commits its base (since #1830 S2 the track worktree's HEAD) is behind it.
//! Absent when that branch has no known upstream. Read-only: the read fetches nothing and writes no
//! ref.

use std::path::{Path, PathBuf};

use super::git_delivery::*;
use calm_server::session_projection_repo::AgentProvider;
use serde_json::{Value, json};

async fn lease_base_source(pool: &sqlx::SqlitePool, lease_id: &str) -> String {
    sqlx::query_scalar("SELECT base_source FROM workspace_leases WHERE lease_id = ?1")
        .bind(lease_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn all_refs(repo: &Path) -> String {
    git(repo, &["for-each-ref", "--format=%(objectname) %(refname)"])
}

#[tokio::test]
async fn plan_list_reads_how_far_an_upstream_candidate_is_behind() {
    let mut origin: Option<PathBuf> = None;
    let fx = fixture_with(|tmp| {
        let repo = tmp.join("repo");
        init_repo(&repo);
        let origin_path = tmp.join("origin");
        git(
            tmp,
            &[
                "clone",
                "-q",
                repo.to_str().unwrap(),
                origin_path.to_str().unwrap(),
            ],
        );
        git(
            &origin_path,
            &["config", "user.email", "origin@example.test"],
        );
        git(&origin_path, &["config", "user.name", "Origin"]);
        origin = Some(origin_path);
        repo
    })
    .await;
    let origin = origin.unwrap();
    let repo = fx.track_root.clone();
    // The upstream is set on the track branch after the track worktree was made, so no kernel
    // fetch receipt exists and the repository's own tracking ref is what is last known. The main
    // checkout's branch has none (#2112: it is not the track's).
    git(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&repo, &["fetch", "-q", "origin"]);
    git(
        &fx.worktree,
        &["branch", "-q", "--set-upstream-to=origin/main"],
    );
    let commit_upstream = |message: &str| {
        git(&origin, &["commit", "-q", "--allow-empty", "-m", message]);
        git(&origin, &["rev-parse", "HEAD"])
    };

    // The worker's lease starts from the track worktree's HEAD (#1830 S2), whatever the upstream.
    let worker = fx.new_worker("up", AgentProvider::Codex).await;
    let lease = fx.kernel_lease(&worker.card_id).await;
    assert_eq!(lease.base_sha, git(&fx.worktree, &["rev-parse", "HEAD"]));
    assert_eq!(
        lease_base_source(&fx.pool(), &lease.lease_id).await,
        "commit"
    );
    fx.running_task("up", "codex", &worker.card_id, json!({}))
        .await;

    // Two land upstream and the user fetches them.
    commit_upstream("landed after the lease");
    let now = commit_upstream("landed after the lease, again");
    git(&repo, &["fetch", "-q", "origin"]);
    let refs_before = all_refs(&repo);

    let entry = fx.plan_entry("up").await;
    assert_eq!(entry["candidate"]["binding"], "bound", "{entry}");
    assert_eq!(entry["candidate"]["base_sha"], lease.base_sha);
    assert_eq!(
        entry["candidate"]["upstream"],
        json!({"sha": now, "behind": 2}),
        "{entry}"
    );
    assert!(entry["candidate"].get("base_source").is_none(), "{entry}");
    let summary = fx.plan_summary_entry("up").await;
    assert_eq!(
        summary["candidate"]["upstream"],
        json!({"sha": now, "behind": 2}),
        "{summary}"
    );

    // The read fetched nothing and wrote no ref.
    assert_eq!(all_refs(&repo), refs_before);
    assert!(
        !refs_before.contains("refs/neige/upstream/"),
        "{refs_before}"
    );

    // No known upstream for the track branch now: nothing to measure against, though the main
    // checkout's branch has one.
    git(&repo, &["branch", "-q", "--set-upstream-to=origin/main"]);
    git(&fx.worktree, &["branch", "-q", "--unset-upstream"]);
    let entry = fx.plan_entry("up").await;
    assert_eq!(entry["candidate"]["binding"], "bound", "{entry}");
    assert_eq!(
        entry["candidate"].get("upstream"),
        None::<&Value>,
        "{entry}"
    );
}
