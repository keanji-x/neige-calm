//! Non-macOS owners support only the historical, untracked daemon protocol.
use super::*;

impl SharedCodexAppServer {
    /// Read, admit and path-check one snapshot before any process or endpoint effect.
    /// A refusal grants no cleanup or durable write; retain any installed live Child.
    /// A crash watcher may independently revoke a proven-dead in-memory incarnation.
    pub(super) async fn read_admitted_daemon_record(
        &self,
        paths: &VerifiedRuntimePaths,
    ) -> Result<crate::db::SharedCodexDaemonRecord> {
        let record = self
            .repo
            .shared_daemon_runtime_get(self.backend_id.as_str())
            .await
            .map_err(|error| {
                CalmError::CodexAppServer(format!(
                    "unreconciled: failed reading persisted daemon record: {error}"
                ))
            })?;
        match record.recovery {
            calm_truth::db::DaemonRecovery::LegacyUntracked => {}
            calm_truth::db::DaemonRecovery::Pending { .. }
            | calm_truth::db::DaemonRecovery::Clear
            | calm_truth::db::DaemonRecovery::Recorded { .. } => {
                return Err(CalmError::Conflict(format!(
                    "shared codex recovery phase is unsupported on this platform (backend={}, phase={:?})",
                    self.backend_id.as_str(),
                    record.recovery
                )));
            }
        }
        // Reuse the isolation contract on the very snapshot this owner consumes.
        if !persisted_process_proven_absent(&record) {
            paths.check_record(&record).map_err(|error| {
                CalmError::CodexAppServer(format!("backend isolation refused: {error}"))
            })?;
        }
        Ok(record)
    }
}
