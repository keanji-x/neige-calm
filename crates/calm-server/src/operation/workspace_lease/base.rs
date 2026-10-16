//! The base a workspace lease records, and the check that the worker's checkout really is that
//! base at that path.
//!
//! Since #1830 S2 a worker runs in its track's checkout (`super::worker`): the lease row records
//! that directory's HEAD (`BaseSource::Commit`), its realpath and its common dir in the one
//! INSERT ([`super::acquire_workspace_lease_tx`]), and the spawn only verifies
//! ([`verify_worktree_base`]). `Head`, `Upstream` and `Attempt` are the values rows written before
//! S2 carry (the per-card worktree's upstream / HEAD start, the retired `task.replace` carry); the
//! column round-trip covers all four so the CHECK-accepted shapes and the Rust type never
//! disagree.

use std::path::PathBuf;

use sqlx::{Row, Sqlite, sqlite::SqliteRow};

use crate::error::{CalmError, Result};
use workspace::base::utf8_path;

/// `workspace_leases.base_source`: how `base_sha` was chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BaseSource {
    /// Before #1830 S2: the attached repository's HEAD at prepare time.
    Head,
    /// Before #1830 S2: the last known commit of the upstream of the branch HEAD was on (#1777).
    Upstream,
    /// The worker checkout's HEAD at prepare time: every lease since #1830 S2.
    Commit,
    /// Before #1830 S2: a kernel carry commit for a retired `task.replace` successor (#1785);
    /// `base_attempt_id` names the attempt that produced the candidate.
    Attempt,
}

impl BaseSource {
    pub(crate) fn as_column(self) -> &'static str {
        match self {
            BaseSource::Head => "head",
            BaseSource::Upstream => "upstream",
            BaseSource::Commit => "commit",
            BaseSource::Attempt => "attempt",
        }
    }

    pub(crate) fn from_column(value: &str) -> Result<Self> {
        match value {
            "head" => Ok(BaseSource::Head),
            "upstream" => Ok(BaseSource::Upstream),
            "commit" => Ok(BaseSource::Commit),
            "attempt" => Ok(BaseSource::Attempt),
            other => Err(CalmError::Internal(format!(
                "workspace lease base_source {other:?} is not head, upstream, commit or attempt"
            ))),
        }
    }
}

/// `workspace_leases.delivery_policy`: who commits and pins this lease's
/// worktree. `Kernel` (`'kernel'`) is the one criterion for candidate
/// binding (#1727 S4 D2); a NULL column — a lease claimed before migration
/// 0113, or the fixtures-only plain lease — is legacy and reads as `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeliveryPolicy {
    Kernel,
}

impl DeliveryPolicy {
    pub(crate) fn as_column(self) -> &'static str {
        match self {
            DeliveryPolicy::Kernel => "kernel",
        }
    }

    /// Decode the column of one row: NULL is legacy, `'kernel'` is `Kernel`
    /// (and the row has a base — the CHECK admits nothing else); any other
    /// shape is reported rather than guessed at.
    pub(crate) fn from_row(row: &SqliteRow) -> Result<Option<DeliveryPolicy>> {
        let value: Option<String> = row.try_get("delivery_policy")?;
        let base_sha: Option<String> = row.try_get("base_sha")?;
        match (value.as_deref(), base_sha) {
            (None, _) => Ok(None),
            (Some("kernel"), Some(_)) => Ok(Some(DeliveryPolicy::Kernel)),
            (Some("kernel"), None) => Err(CalmError::Internal(
                "workspace lease delivery_policy is kernel on a row without a base".into(),
            )),
            (Some(other), _) => Err(CalmError::Internal(format!(
                "workspace lease delivery_policy {other:?} is not kernel"
            ))),
        }
    }
}

/// The five base columns of a lease row, all present. A row written before
/// migration 0111 (or by the fixtures-only plain lease) has none of them and
/// reads as `None` at [`super::WorkspaceLease::base`]; the tuple CHECK keeps
/// a partially NULL row out of the table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LeaseBase {
    /// The commit the worktree is pinned to.
    pub base_sha: String,
    pub base_source: BaseSource,
    /// Set iff `base_source == Attempt`.
    pub base_attempt_id: Option<String>,
    /// The realpath of the worker's checkout, which the spawn check and the delivery's
    /// provenance check compare against. UTF-8 by construction (`super::worker` refuses a
    /// realpath that is not): the value is stored as TEXT and frozen into `tx_output.data`
    /// through `json!`, which panics on a path it cannot serialise.
    pub canonical_path: PathBuf,
    /// `canonicalize(git rev-parse --path-format=absolute --git-common-dir)`
    /// of the repository the lease anchors to. UTF-8 by construction ([`lease_git_common_dir`]).
    pub git_common_dir: PathBuf,
}

impl LeaseBase {
    /// Decode the five columns of one `workspace_leases` row. `Ok(None)` for
    /// the all-NULL legacy shape; `Err` for any shape the CHECK does not
    /// accept (unreachable through the migration, reported rather than
    /// guessed at).
    pub(crate) fn from_row(row: &SqliteRow) -> Result<Option<LeaseBase>> {
        let base_sha: Option<String> = row.try_get("base_sha")?;
        let base_source: Option<String> = row.try_get("base_source")?;
        let base_attempt_id: Option<String> = row.try_get("base_attempt_id")?;
        let canonical_path: Option<String> = row.try_get("canonical_path")?;
        let git_common_dir: Option<String> = row.try_get("git_common_dir")?;
        let lease_id: String = row.try_get("lease_id")?;
        match (base_sha, base_source, canonical_path, git_common_dir) {
            (None, None, None, None) if base_attempt_id.is_none() => Ok(None),
            (Some(base_sha), Some(base_source), Some(canonical_path), Some(git_common_dir)) => {
                let base_source = BaseSource::from_column(&base_source)?;
                if (base_source == BaseSource::Attempt) != base_attempt_id.is_some() {
                    return Err(CalmError::Internal(format!(
                        "workspace lease {lease_id} base_source {} disagrees with base_attempt_id {base_attempt_id:?}",
                        base_source.as_column()
                    )));
                }
                Ok(Some(LeaseBase {
                    base_sha,
                    base_source,
                    base_attempt_id,
                    canonical_path: PathBuf::from(canonical_path),
                    git_common_dir: PathBuf::from(git_common_dir),
                }))
            }
            _ => Err(CalmError::Internal(format!(
                "workspace lease {lease_id} base columns are partially NULL"
            ))),
        }
    }

    /// Append the five base columns to the lease INSERT, in the INSERT's own
    /// order (`base_sha, base_source, base_attempt_id, canonical_path,
    /// git_common_dir`); `None` binds the legacy all-NULL tuple (the
    /// fixtures-only plain lease). Both paths are bound as the `&str` they
    /// are by construction; a `LeaseBase` holding a non-UTF-8 path (none of
    /// the constructors produces one) is an `Err` here, never a lossily
    /// rewritten row.
    pub(crate) fn bind_columns<'q>(
        query: sqlx::query::Query<'q, Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
        base: Option<&'q LeaseBase>,
    ) -> Result<sqlx::query::Query<'q, Sqlite, sqlx::sqlite::SqliteArguments<'q>>> {
        let paths = base
            .map(|base| {
                Ok::<_, CalmError>((
                    utf8_path(&base.canonical_path, "workspace lease canonical_path")?,
                    utf8_path(&base.git_common_dir, "workspace lease git_common_dir")?,
                ))
            })
            .transpose()?;
        Ok(query
            .bind(base.map(|base| base.base_sha.as_str()))
            .bind(base.map(|base| base.base_source.as_column()))
            .bind(base.and_then(|base| base.base_attempt_id.as_deref()))
            .bind(paths.map(|(canonical_path, _)| canonical_path))
            .bind(paths.map(|(_, git_common_dir)| git_common_dir)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_source_round_trips_all_values() {
        for source in [
            BaseSource::Head,
            BaseSource::Upstream,
            BaseSource::Commit,
            BaseSource::Attempt,
        ] {
            assert_eq!(BaseSource::from_column(source.as_column()).unwrap(), source);
        }
        assert_eq!(BaseSource::Head.as_column(), "head");
        assert_eq!(BaseSource::Upstream.as_column(), "upstream");
        assert_eq!(BaseSource::Commit.as_column(), "commit");
        assert_eq!(BaseSource::Attempt.as_column(), "attempt");
        let err = BaseSource::from_column("bogus").unwrap_err();
        assert!(matches!(err, CalmError::Internal(_)), "{err}");
        assert!(BaseSource::from_column("").is_err());
        assert!(BaseSource::from_column("HEAD").is_err());
    }

    /// The lossy rewrite is gone: a `LeaseBase` holding a non-UTF-8 path
    /// (no constructor produces one) is refused at the bind, never stored as
    /// a path that does not exist.
    #[test]
    fn bind_columns_refuses_a_non_utf8_path() {
        use std::os::unix::ffi::OsStringExt;
        let base = LeaseBase {
            base_sha: "abc".into(),
            base_source: BaseSource::Head,
            base_attempt_id: None,
            canonical_path: PathBuf::from(std::ffi::OsString::from_vec(b"/wt-\xff/c".to_vec())),
            git_common_dir: PathBuf::from("/repo/.git"),
        };
        let Err(err) = LeaseBase::bind_columns(sqlx::query("SELECT 1"), Some(&base)) else {
            panic!("a non-UTF-8 canonical_path must be refused at the bind");
        };
        assert!(err.to_string().contains("not UTF-8"), "{err}");
        assert!(LeaseBase::bind_columns(sqlx::query("SELECT 1"), None).is_ok());
    }
}
