//! #1777 B — `calm.plan.list.candidate.upstream`: a bound candidate whose lease started from the
//! Track repository's upstream reads the upstream commit as last known now and how many commits
//! its base is behind it; a `head` lease reads no such field. Read-only: the read fetches nothing
//! and writes no ref.

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
        git(
            &repo,
            &["remote", "add", "origin", origin_path.to_str().unwrap()],
        );
        git(&repo, &["fetch", "-q", "origin"]);
        git(&repo, &["branch", "-q", "--set-upstream-to=origin/main"]);
        origin = Some(origin_path);
        repo
    })
    .await;
    let origin = origin.unwrap();
    let repo = fx.track_root.clone();
    let commit_upstream = |message: &str| {
        git(&origin, &["commit", "-q", "--allow-empty", "-m", message]);
        git(&origin, &["rev-parse", "HEAD"])
    };

    // The user fetched but never merged: HEAD lags; the lease starts from the upstream.
    let first = commit_upstream("landed before the lease");
    git(&repo, &["fetch", "-q", "origin"]);
    assert_ne!(git(&repo, &["rev-parse", "HEAD"]), first);
    let upstream_worker = fx.new_worker("up", AgentProvider::Codex).await;
    let upstream_lease = fx.kernel_lease(&upstream_worker.card_id).await;
    assert_eq!(upstream_lease.base_sha, first);
    assert_eq!(
        lease_base_source(&fx.pool(), &upstream_lease.lease_id).await,
        "upstream"
    );
    fx.running_task("up", "codex", &upstream_worker.card_id, json!({}))
        .await;

    // A `head` lease: the branch has no upstream while it is taken.
    git(&repo, &["branch", "-q", "--unset-upstream"]);
    let head_worker = fx.new_worker("head", AgentProvider::Codex).await;
    let head_lease = fx.kernel_lease(&head_worker.card_id).await;
    assert_eq!(
        lease_base_source(&fx.pool(), &head_lease.lease_id).await,
        "head"
    );
    fx.running_task("head", "codex", &head_worker.card_id, json!({}))
        .await;
    git(&repo, &["branch", "-q", "--set-upstream-to=origin/main"]);

    // Two more land upstream and the user fetches them.
    commit_upstream("landed after the lease");
    let now = commit_upstream("landed after the lease, again");
    git(&repo, &["fetch", "-q", "origin"]);
    let refs_before = all_refs(&repo);

    let entry = fx.plan_entry("up").await;
    assert_eq!(entry["candidate"]["binding"], "bound", "{entry}");
    assert_eq!(entry["candidate"]["base_sha"], first);
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

    let head_entry = fx.plan_entry("head").await;
    assert_eq!(head_entry["candidate"]["binding"], "bound", "{head_entry}");
    assert_eq!(head_entry["candidate"].get("upstream"), None::<&Value>);

    // The read fetched nothing and wrote no ref.
    assert_eq!(all_refs(&repo), refs_before);
    assert!(
        !refs_before.contains("refs/neige/upstream/"),
        "{refs_before}"
    );
}
