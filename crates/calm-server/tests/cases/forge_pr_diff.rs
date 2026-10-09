//! #2459: `gh_pr_diff` stores the net diff of the requested `base_sha...head_sha`, not the diff
//! of whatever head the PR has when the read runs.
use super::*;
use crate::support::git_helpers::git_stdout;

/// `git diff base...head` in the origin, untrimmed: the diff GitHub's compare returns.
fn net_diff(origin: &Path, base: &str, head: &str) -> String {
    let range = format!("{base}...{head}");
    let output = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(origin)
        .args(["diff", range.as_str()])
        .output()
        .expect("run git diff");
    assert!(output.status.success(), "git diff {range} failed");
    String::from_utf8(output.stdout).expect("utf-8 diff")
}

/// Commit `name` on top of `parent` in `clone` and push it to `branch` on the origin.
fn push_commit(clone: &Path, parent: &str, branch: &str, name: &str) -> String {
    run_git(clone, ["fetch", "origin"]);
    run_git(clone, ["checkout", "--detach", parent]);
    stage_git_change(clone, name, &format!("{name}\n"));
    run_git(clone, ["commit", "-m", name]);
    let refspec = format!("HEAD:refs/heads/{branch}");
    run_git(clone, ["push", "origin", refspec.as_str()]);
    git_stdout(clone, ["rev-parse", "HEAD"])
}

#[tokio::test]
async fn gh_pr_diff_reads_the_requested_commits_after_the_pr_and_base_move() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let _env = setup_forge_env();
    let mut fx = boot_delivery_fixture().await;
    let (pr, cut_base) =
        drive_pr_to_published(&mut fx, 80, 2459, "reviewed.txt", "reviewed\n", "Pinned").await;
    let branch = git_stdout(
        &fx.origin_repo,
        [
            "for-each-ref",
            "--points-at",
            pr.head_sha.as_str(),
            "--format=%(refname:lstrip=2)",
            "refs/heads/neige/",
        ],
    );
    assert!(
        !branch.is_empty(),
        "the published head is on an origin branch"
    );

    // After the review head was published, main and the PR both move on.
    let mover = short_tempdir("m").expect("mover tempdir");
    let mover = mover.path().join("clone");
    clone_for_track(&fx.origin_repo, &mover);
    let main = push_commit(&mover, &cut_base, "main", "main-later.txt");
    let moved = push_commit(&mover, &pr.head_sha, &branch, "pushed-later.txt");

    let reviewed = read_pr_diff(&fx, 84, &pr, &main, &pr.head_sha).await;
    let artifact = reviewed.payload["artifact_path"]
        .as_str()
        .expect("artifact_path");
    let artifact = std::fs::read_to_string(artifact).expect("read diff artifact");
    assert_eq!(
        artifact,
        net_diff(&fx.origin_repo, &main, &pr.head_sha),
        "the artifact is the net diff of the requested commits"
    );
    assert!(
        artifact.contains("reviewed.txt")
            && !artifact.contains("pushed-later.txt")
            && !artifact.contains("main-later.txt"),
        "neither the PR's later head nor the base's own change is in the diff: {artifact}"
    );

    let later = read_pr_diff(&fx, 85, &pr, &main, &moved).await;
    let later = later.payload["artifact_path"]
        .as_str()
        .expect("artifact_path");
    let later = std::fs::read_to_string(later).expect("read later diff artifact");
    assert_eq!(later, net_diff(&fx.origin_repo, &main, &moved));
    assert!(later.contains("pushed-later.txt"), "{later}");

    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop git-forge plugin");
}
