//! The local half of an upstream lease base (#1777), round-3 contracts: the
//! atomic fetch receipt, the remote URL in the kernel ref and key, a
//! force-push over a checkout without commits of its own, and git's picks of
//! empty config values.

use std::path::Path;

use super::upstream::*;
use super::upstream_fetch::{UpstreamRefresh, refresh_upstream};
use super::upstream_tests::{attach_origin, attached_repo, git, kernel_ref};

fn fetch_key(repo: &Path, origin_upstream: &Upstream) -> super::upstream_fetch::FetchKey {
    (
        super::base::lease_git_common_dir(repo).unwrap(),
        origin_upstream.kernel_ref(),
    )
}

/// The receipt is atomic: the resolver takes the commit recorded with the
/// success outcome, never a separate read of the kernel ref. The old
/// incoherent pair — the kernel ref still at an older commit while the
/// provenance already says a newer fetch succeeded — is constructed by hand:
/// the receipt's commit is the one returned.
#[tokio::test]
async fn the_success_receipt_is_read_not_the_kernel_ref() {
    let attached = attached_repo();
    let origin = attach_origin(attached.path());
    let older = origin.commit("older");
    assert!(matches!(
        refresh_upstream(attached.path()).await,
        UpstreamRefresh::Fetched { .. }
    ));
    let newer = origin.commit("newer");
    git(attached.path(), &["fetch", "-q", "origin"]);
    let key = fetch_key(attached.path(), &origin.upstream());
    super::upstream_fetch::FetchProvenance::global().record_success_for_test(&key, &newer);
    assert_eq!(
        git(attached.path(), &["rev-parse", &kernel_ref(&origin)]),
        older,
        "the kernel ref still holds the older commit"
    );

    let known = last_known_upstream(attached.path()).unwrap().unwrap();
    assert_eq!(known.sha, newer);
    assert_eq!(known.source, UpstreamSource::KernelFetch);
    assert_eq!(
        choose_lease_start(attached.path()).unwrap(),
        LeaseStart::Upstream { sha: newer }
    );
}

/// The effective remote URL is part of the kernel ref and the provenance
/// key: two linked worktrees of one repository whose `origin` points at
/// different URLs (per-worktree config) get distinct kernel refs and keys,
/// and re-pointing a remote leaves the new key without a receipt, so the
/// tracking ref applies.
#[tokio::test]
async fn the_remote_url_is_part_of_the_kernel_ref_and_key() {
    let attached = attached_repo();
    let origin = attach_origin(attached.path());
    let mirror = tempfile::tempdir().unwrap();
    let mirror_path = mirror.path().join("mirror");
    git(
        origin.path(),
        &["clone", "-q", ".", mirror_path.to_str().unwrap()],
    );
    let worktree = attached.path().join("linked");
    git(
        attached.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            worktree.to_str().unwrap(),
        ],
    );
    git(
        &worktree,
        &[
            "branch",
            "-q",
            &format!("--set-upstream-to={}", origin.tracking_ref()),
        ],
    );
    // `remote.<r>.url` is multi-valued and a fetch uses the first value, so
    // per-worktree URLs replace — not add to — the shared one.
    git(
        attached.path(),
        &["config", "extensions.worktreeConfig", "true"],
    );
    git(
        attached.path(),
        &["config", "--unset-all", "remote.origin.url"],
    );
    git(
        attached.path(),
        &[
            "config",
            "--worktree",
            "remote.origin.url",
            origin.path().to_str().unwrap(),
        ],
    );
    git(
        &worktree,
        &[
            "config",
            "--worktree",
            "remote.origin.url",
            mirror_path.to_str().unwrap(),
        ],
    );
    let main_upstream = head_upstream(attached.path()).unwrap().unwrap();
    let linked_upstream = head_upstream(&worktree).unwrap().unwrap();
    assert_eq!(main_upstream.url, origin.path().to_str().unwrap());
    assert_eq!(linked_upstream.url, mirror_path.to_str().unwrap());
    assert_eq!(
        (main_upstream.remote.as_str(), main_upstream.merge.as_str()),
        (
            linked_upstream.remote.as_str(),
            linked_upstream.merge.as_str()
        )
    );
    assert_ne!(main_upstream.kernel_ref(), linked_upstream.kernel_ref());
    assert_ne!(
        fetch_key(attached.path(), &main_upstream),
        fetch_key(&worktree, &linked_upstream)
    );

    // A receipt, then the human re-points the remote: no receipt for the new key.
    origin.commit("fetched by the kernel");
    assert!(matches!(
        refresh_upstream(attached.path()).await,
        UpstreamRefresh::Fetched { .. }
    ));
    assert_eq!(
        last_known_upstream(attached.path())
            .unwrap()
            .unwrap()
            .source,
        UpstreamSource::KernelFetch
    );
    git(
        attached.path(),
        &[
            "config",
            "--worktree",
            "remote.origin.url",
            mirror_path.to_str().unwrap(),
        ],
    );
    let known = last_known_upstream(attached.path()).unwrap().unwrap();
    assert_eq!(known.source, UpstreamSource::TrackingRef);
    assert_eq!(known.ref_name, origin.tracking_ref());
}

/// An upstream force-push while the human has no commits of their own (HEAD
/// is their tracking ref): nothing to lose, so the lease bases on the
/// rewritten upstream the kernel fetched — no refusal, no advice to push.
#[tokio::test]
async fn a_force_push_over_a_checkout_without_own_commits_bases_on_the_upstream() {
    let attached = attached_repo();
    let origin = attach_origin(attached.path());
    let old_tip = origin.commit("the old tip");
    git(attached.path(), &["fetch", "-q", "origin"]);
    git(attached.path(), &["merge", "-q", "--ff-only", &old_tip]);
    git(origin.path(), &["reset", "-q", "--hard", "HEAD~1"]);
    let rewritten = origin.commit("rewritten history");
    assert!(matches!(
        refresh_upstream(attached.path()).await,
        UpstreamRefresh::Fetched { .. }
    ));
    assert_eq!(git(attached.path(), &["rev-parse", "HEAD"]), old_tip);
    assert!(!is_ancestor(attached.path(), &old_tip, &rewritten).unwrap());

    assert_eq!(
        choose_lease_start(attached.path()).unwrap(),
        LeaseStart::Upstream { sha: rewritten }
    );
}

/// Empty config values are kept while picking the value git uses, and a
/// picked empty value means no upstream — as git has none either.
#[test]
fn an_empty_picked_config_value_is_no_upstream() {
    let attached = attached_repo();
    let origin = attach_origin(attached.path());
    let merge_key = format!("branch.{}.merge", origin.branch);
    let remote_key = format!("branch.{}.remote", origin.branch);
    let real_merge = format!("refs/heads/{}", origin.branch);

    // The FIRST merge value is empty: no upstream (a filter would pick the second).
    git(attached.path(), &["config", "--unset-all", &merge_key]);
    git(attached.path(), &["config", "--add", &merge_key, ""]);
    git(
        attached.path(),
        &["config", "--add", &merge_key, &real_merge],
    );
    assert_eq!(head_upstream(attached.path()).unwrap(), None);

    // The LAST remote value is empty: no upstream.
    git(attached.path(), &["config", "--unset-all", &merge_key]);
    git(
        attached.path(),
        &["config", "--add", &merge_key, &real_merge],
    );
    assert!(head_upstream(attached.path()).unwrap().is_some());
    git(attached.path(), &["config", "--add", &remote_key, ""]);
    assert_eq!(head_upstream(attached.path()).unwrap(), None);
}
