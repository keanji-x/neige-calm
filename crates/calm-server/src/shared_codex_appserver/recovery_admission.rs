//! Non-macOS owners support only the historical, untracked daemon protocol.
use super::*;

/// The serial and this invocation's memory-completion responsibility travel together.
/// Only replacement entry may mint the consumed-source form; neither a later row nor
/// a generation/state observation grants that responsibility or any process authority.
pub(super) struct StartTransition {
    serial: tokio::sync::OwnedMutexGuard<()>,
    source_consumed: bool,
}

impl StartTransition {
    pub(super) fn unconsumed(serial: tokio::sync::OwnedMutexGuard<()>) -> Self {
        Self {
            serial,
            source_consumed: false,
        }
    }

    pub(super) fn after_source_consumed(serial: tokio::sync::OwnedMutexGuard<()>) -> Self {
        Self {
            serial,
            source_consumed: true,
        }
    }

    /// Finish an otherwise-unhandled error before releasing this transition's serial.
    /// Lower owners may already have installed an authoritative Unreconciled failure;
    /// the Failed check preserves that report, rather than authorizing this write.
    pub(super) async fn terminalize_error<T>(
        &self,
        owner: &Arc<SharedCodexAppServer>,
        result: Result<T>,
    ) -> Result<T> {
        match result {
            Err(error) if self.source_consumed => {
                let already_failed = matches!(
                    owner.core.lock().await.state,
                    SupervisorState::Failed { .. }
                );
                if already_failed {
                    return Err(error);
                }
                let report = FailureReport::from_error(error);
                owner
                    .fail_in_memory_and_arm_heal(report.message, report.class)
                    .await;
                Err(report.error)
            }
            result => result,
        }
    }

    pub(super) fn release(self) {
        drop(self.serial);
    }
}

impl SharedCodexAppServer {
    /// Prepare under the serial, then move the same memory owner into the detached spawn.
    pub(super) async fn start_body_locked(
        self: &Arc<Self>,
        transition: StartTransition,
        paths: VerifiedRuntimePaths,
        increment_restart_count: bool,
        last_error: Option<String>,
    ) -> Result<StartedVia> {
        let prepared = self.prepare_legacy_start(&paths).await;
        if transition.terminalize_error(self, prepared).await? {
            return Ok(StartedVia::Takeover);
        }
        self.spawn_process_transition(transition, paths, increment_restart_count, last_error)
            .await?;
        Ok(StartedVia::Spawn)
    }

    async fn prepare_legacy_start(self: &Arc<Self>, paths: &VerifiedRuntimePaths) -> Result<bool> {
        let record = self.read_admitted_daemon_record(paths).await?;
        // Guard and reconciliation consume only this admitted snapshot.
        if let Err(guard_err) = SharedCodexHome::new(paths.paths().home.clone())
            .verify_expected_mcp_servers(EXPECTED_MCP_SERVERS)
        {
            let msg = format!("refusing to launch shared codex app-server: {guard_err}");
            self.record_guard_refusal(paths, &record, &msg).await;
            return Err(CalmError::CodexAppServer(msg));
        }
        let takeover_eligible = !matches!(
            record.state,
            SharedDaemonState::Idle | SharedDaemonState::Failed
        ) && record.pid.is_some()
            && record.pgid == record.pid
            && record.process_start_time.is_some()
            && record.boot_id.is_some()
            && record.started_at.is_some()
            && record.sock_path.is_some();
        if persisted_identity_present(&record) && !takeover_eligible {
            match self.resolve_persisted_shape(paths, &record).await {
                ShapeResolution::ProvenAbsent => {
                    // Durable proof of absence makes a retry safe even if spawn never completes.
                    let proven_error = record
                        .last_error
                        .as_deref()
                        .map(strip_unreconciled_prefix)
                        .unwrap_or("unreconciled survivor proven absent")
                        .to_string();
                    self.persist_failed_deduped(
                        &proven_error,
                        FailureClass::Persistent,
                        FailedIdentity::proven_absent(&paths.paths().socket, &paths.paths().home),
                    )
                    .await;
                }
                ShapeResolution::Unreconciled { needs_operator } => {
                    let msg =
                        unreconciled_message(self.backend_id.as_str(), &record, needs_operator);
                    self.persist_failed_deduped(
                        &msg,
                        FailureClass::Unreconciled,
                        FailedIdentity::retained(&record),
                    )
                    .await;
                    self.fail_in_memory_and_arm_heal(msg.clone(), FailureClass::Unreconciled)
                        .await;
                    return Err(CalmError::CodexAppServer(msg));
                }
            }
        }
        // Failed survivors are never adopted; takeover errors share this completion owner.
        self.try_takeover_live(paths, &record).await
    }

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
