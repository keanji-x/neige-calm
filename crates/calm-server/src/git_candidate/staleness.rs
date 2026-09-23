//! `calm.plan.list.candidate.upstream` (#1777): how far a bound candidate's
//! base is behind the Track repository's upstream as last known. Read-only
//! and computed at read time — never stored, never fetched: the upstream is
//! the one a new lease of this Track would start from now
//! ([`last_known_upstream`]: the kernel ref the submit path fetched, else the
//! repository's own remote-tracking ref). Local `git` only, and run after
//! `plan.list`'s transaction has committed, so no git process ever runs while
//! the kernel's write transaction is held.
//!
//! Given only for a candidate whose lease started from the upstream
//! ([`super::view::CandidateBinding::upstream_base`]); a `head` or legacy
//! lease has no upstream to be behind, and the field is absent.

use std::path::Path;

use serde::Serialize;

use crate::operation::workspace_lease::git_repo_root_for_track_cwd;
use crate::operation::workspace_lease::upstream::{commits_behind, last_known_upstream};

/// `candidate.upstream`: the upstream commit now, and how many commits it has
/// that the candidate's base does not (`git rev-list --count <base>..<sha>`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct UpstreamStaleness {
    pub sha: String,
    pub behind: u64,
}

/// One answer per entry of `bases`, in order: `None` for an entry without an
/// upstream base, and for every entry when the Track repository has no known
/// upstream now. A git failure is a `warn!` and a `None`, never a guess: the
/// field is advisory and must not fail the read.
pub(crate) fn upstream_staleness(
    track_id: &str,
    track_cwd: &str,
    bases: &[Option<String>],
) -> Vec<Option<UpstreamStaleness>> {
    if bases.iter().all(Option::is_none) {
        return vec![None; bases.len()];
    }
    let upstream = git_repo_root_for_track_cwd(track_id, track_cwd)
        .and_then(|repo_root| Ok(last_known_upstream(&repo_root)?.map(|sha| (repo_root, sha))));
    let (repo_root, upstream_sha) = match upstream {
        Ok(Some(found)) => found,
        Ok(None) => return vec![None; bases.len()],
        Err(error) => {
            tracing::warn!(track_id, %error, "candidate upstream staleness: no upstream read");
            return vec![None; bases.len()];
        }
    };
    bases
        .iter()
        .map(|base| {
            let base = base.as_deref()?;
            behind(track_id, &repo_root, base, &upstream_sha)
        })
        .collect()
}

fn behind(
    track_id: &str,
    repo_root: &Path,
    base: &str,
    upstream_sha: &str,
) -> Option<UpstreamStaleness> {
    match commits_behind(repo_root, base, upstream_sha) {
        Ok(behind) => Some(UpstreamStaleness {
            sha: upstream_sha.to_string(),
            behind,
        }),
        Err(error) => {
            tracing::warn!(track_id, base, %error, "candidate upstream staleness: rev-list failed");
            None
        }
    }
}
