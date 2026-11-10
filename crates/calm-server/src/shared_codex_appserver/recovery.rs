//! macOS shared-owner authority carried by value through one serialized transition.
use super::heal_retry::classify_spawn_failure;
use super::*;
use crate::db::{DaemonRecovery, RecoveryId, RecoveryWrite, SharedCodexDaemonRecord};
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;

// This token is produced only by an acknowledged Begin, never decoded from a
// durable Pending row. Moving it into Entered consumes the no-process-entry fact.
pub(super) struct Reservation {
    pub(super) id: RecoveryId,
    pub(super) identity: FailedIdentity,
}
pub(super) enum Stage {
    Preparing,
    Existing,
    Reconciling(FailedIdentity),
    Reserved(Reservation),
    Entered {
        id: RecoveryId,
        identity: FailedIdentity,
    },
    Observed {
        id: RecoveryId,
        identity: FailedIdentity,
    },
    Settled(FailedIdentity),
}
pub(super) struct RecoveryAttempt {
    pub(super) stage: Stage,
}
impl RecoveryAttempt {
    pub(super) fn new() -> Self {
        Self {
            stage: Stage::Preparing,
        }
    }
    pub(super) fn enter(&mut self) -> Result<RecoveryId> {
        match std::mem::replace(&mut self.stage, Stage::Preparing) {
            Stage::Reserved(reservation) => {
                let id = reservation.id;
                self.stage = Stage::Entered {
                    id,
                    identity: reservation.identity,
                };
                Ok(id)
            }
            stage => {
                self.stage = stage;
                Err(CalmError::Conflict(
                    "process entry requires an unconsumed reservation".into(),
                ))
            }
        }
    }
    pub(super) fn entered_id(&self) -> Result<RecoveryId> {
        match &self.stage {
            Stage::Entered { id, .. } | Stage::Observed { id, .. } => Ok(*id),
            _ => Err(CalmError::Conflict("no entered process scope".into())),
        }
    }
}

#[cfg(test)]
pub(super) struct RecoveryHooks {
    pub(super) socket_cleanup_error: std::sync::Mutex<Option<std::io::ErrorKind>>,
    #[cfg(test)]
    pub(super) after_acquire: std::sync::Mutex<
        Option<(
            tokio::sync::oneshot::Sender<i32>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    >,
    #[cfg(test)]
    pub(super) after_listener_publish_failure: std::sync::Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    >,
    #[cfg(test)]
    pub(super) after_begin: std::sync::Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    >,
}
#[cfg(test)]
impl RecoveryHooks {
    pub(super) fn new() -> Self {
        Self {
            socket_cleanup_error: std::sync::Mutex::new(None),
            #[cfg(test)]
            after_listener_publish_failure: std::sync::Mutex::new(None),
            #[cfg(test)]
            after_begin: std::sync::Mutex::new(None),
            #[cfg(test)]
            after_acquire: std::sync::Mutex::new(None),
        }
    }
    #[cfg(test)]
    pub(super) async fn pause_after_acquire(&self, pid: i32) {
        let barrier = self.after_acquire.lock().unwrap().take();
        if let Some((entered, resume)) = barrier {
            let _ = entered.send(pid);
            resume.await.expect("release acquired process scope");
        }
    }
}
impl FailedIdentity {
    fn update(
        &self,
        state: SharedDaemonState,
        error: Option<String>,
        recovery: RecoveryWrite,
    ) -> SharedCodexDaemonUpdate {
        SharedCodexDaemonUpdate {
            recovery,
            state,
            pid: self.pid,
            pgid: self.pgid,
            paths_are_legacy_display: self.paths_are_legacy_display,
            sock_path: self.sock_path.clone(),
            codex_home_path: self.codex_home_path.clone(),
            process_start_time: self.process_start_time,
            boot_id: self.boot_id.clone(),
            started_at: self.started_at,
            last_error: error,
            increment_restart_count: false,
            daemon_env_signature: self.daemon_env_signature.clone(),
        }
    }
    fn empty_scope(&self) -> bool {
        self.pid.is_some_and(|pid| {
            macos_process::persisted_absent(
                pid,
                self.pgid,
                self.process_start_time,
                self.boot_id.as_deref(),
            )
        })
    }
}

/// Unlike CLI-query resolution, Command permits relative paths and relative or
/// empty PATH entries. This check uses the exact configured command and child
/// PATH. Success is only a preflight: a later opaque spawn Err remains Pending.
pub(super) fn preflight_command(command: &str, path: &OsStr) -> Result<()> {
    let executable = |candidate: &Path| {
        std::fs::metadata(candidate)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    let found = if command.contains('/') {
        executable(Path::new(command))
    } else {
        std::env::split_paths(path).any(|dir| executable(&dir.join(command)))
    };
    if !found {
        return Err(CalmError::CodexAppServer(format!(
            "spawn shared codex app-server: executable unavailable: {command}"
        )));
    }
    Ok(())
}

impl SharedCodexAppServer {
    pub(super) async fn begin_recovery(&self, paths: &VerifiedRuntimePaths) -> Result<Reservation> {
        let id = RecoveryId::fresh();
        let mut identity =
            FailedIdentity::proven_absent(&paths.paths().socket, &paths.paths().home);
        identity.recovery = DaemonRecovery::Pending { id };
        self.repo
            .shared_daemon_runtime_set(
                self.backend_id.as_str(),
                identity.update(
                    SharedDaemonState::Starting,
                    None,
                    RecoveryWrite::Begin { id },
                ),
            )
            .await?;
        self.failed_persist_dedup.clear();
        Ok(Reservation { id, identity })
    }

    pub(super) fn observe_starting(
        &self,
        attempt: &mut RecoveryAttempt,
        paths: &VerifiedRuntimePaths,
        observed: &StartingIdentity,
        signature: String,
    ) -> Result<()> {
        let id = attempt.entered_id()?;
        let mut identity =
            FailedIdentity::proven_absent(&paths.paths().socket, &paths.paths().home);
        identity.recovery = DaemonRecovery::Recorded { id };
        identity.pid = Some(observed.pid);
        identity.pgid = Some(observed.pgid);
        identity.process_start_time = observed.process_start_time;
        identity.boot_id = observed.boot_id.clone();
        identity.started_at = Some(observed.started_at);
        identity.daemon_env_signature = Some(signature);
        attempt.stage = Stage::Observed { id, identity };
        Ok(())
    }

    pub(super) async fn publish_observed(
        &self,
        id: RecoveryId,
        identity: &FailedIdentity,
        message: &str,
    ) -> Result<()> {
        self.repo
            .shared_daemon_runtime_set(
                self.backend_id.as_str(),
                identity.update(
                    SharedDaemonState::Failed,
                    Some(message.into()),
                    RecoveryWrite::Publish { id },
                ),
            )
            .await?;
        self.failed_persist_dedup.clear();
        Ok(())
    }

    pub(super) async fn settle_scope(
        &self,
        attempt: &mut RecoveryAttempt,
        id: RecoveryId,
        identity: &FailedIdentity,
        message: &str,
    ) -> Result<()> {
        let mut cleared = identity.clone();
        cleared.pid = None;
        cleared.pgid = None;
        cleared.process_start_time = None;
        cleared.boot_id = None;
        cleared.started_at = None;
        cleared.daemon_env_signature = None;
        self.repo
            .shared_daemon_runtime_set(
                self.backend_id.as_str(),
                cleared.update(
                    SharedDaemonState::Failed,
                    Some(message.into()),
                    RecoveryWrite::Settle { id },
                ),
            )
            .await?;
        cleared.recovery = DaemonRecovery::Clear;
        attempt.stage = Stage::Settled(cleared);
        self.failed_persist_dedup.clear();
        Ok(())
    }

    pub(super) async fn cancel_reserved(
        &self,
        reservation: Reservation,
        message: &str,
    ) -> Result<()> {
        self.repo
            .shared_daemon_runtime_set(
                self.backend_id.as_str(),
                reservation.identity.update(
                    SharedDaemonState::Failed,
                    Some(message.into()),
                    RecoveryWrite::CancelReserved { id: reservation.id },
                ),
            )
            .await?;
        self.failed_persist_dedup.clear();
        Ok(())
    }

    async fn claim_legacy(
        &self,
        mut record: SharedCodexDaemonRecord,
    ) -> Result<SharedCodexDaemonRecord> {
        if record.recovery == DaemonRecovery::LegacyUntracked
            && (record.pid.is_some()
                || record.pgid.is_some()
                || record.process_start_time.is_some()
                || record.boot_id.is_some())
        {
            let id = RecoveryId::fresh();
            self.repo
                .shared_daemon_runtime_set(
                    self.backend_id.as_str(),
                    FailedIdentity::retained(&record).update(
                        record.state,
                        record.last_error.clone(),
                        RecoveryWrite::ClaimLegacy { id },
                    ),
                )
                .await?;
            record.recovery = DaemonRecovery::Recorded { id };
        }
        Ok(record)
    }

    #[cfg(any(test, feature = "fixtures"))]
    async fn pause_before_failure_finalize(&self) {
        let barrier = self.before_failure_finalize.lock().unwrap().take();
        if let Some((entered, resume)) = barrier {
            let _ = entered.send(());
            resume.await.expect("release failure finalizer");
        }
    }

    /// The caller supplies this transition's authority. No database read, new
    /// reservation, listener acquisition or path receipt can expand it here.
    pub(super) async fn finalize_recovery_failure(
        &self,
        err: CalmError,
        attempt: RecoveryAttempt,
    ) -> FailureReport {
        #[cfg(any(test, feature = "fixtures"))]
        self.pause_before_failure_finalize().await;
        let unresolved = match &err {
            CalmError::CodexAppServer(message)
                if message.starts_with(UNRECONCILED_PREFIX)
                    || message.starts_with(UNRECONCILED_NEEDS_OPERATOR_PREFIX) =>
            {
                message.clone()
            }
            _ => format!("{UNRECONCILED_PREFIX}{err}"),
        };
        let mut attempt = attempt;
        let stage = std::mem::replace(&mut attempt.stage, Stage::Preparing);
        let result: Result<bool> = async {
            match stage {
                Stage::Preparing | Stage::Existing => Ok(false),
                Stage::Settled(identity) => {
                    self.persist_failed_deduped(
                        &err.to_string(),
                        classify_spawn_failure(&err),
                        identity,
                    )
                    .await;
                    Ok(false)
                }
                Stage::Reserved(reservation) => {
                    self.cancel_reserved(reservation, &err.to_string()).await?;
                    Ok(false)
                }
                Stage::Entered { identity, .. } => {
                    self.repo
                        .shared_daemon_runtime_set(
                            self.backend_id.as_str(),
                            identity.update(
                                SharedDaemonState::Failed,
                                Some(unresolved.clone()),
                                RecoveryWrite::Preserve {
                                    expected: identity.recovery,
                                },
                            ),
                        )
                        .await?;
                    Ok(true)
                }
                Stage::Reconciling(identity) => {
                    self.persist_failed_deduped(
                        &unresolved,
                        FailureClass::Unreconciled,
                        identity.clone(),
                    )
                    .await;
                    if let Some(id) = identity.recovery.scope_id()
                        && identity.empty_scope()
                    {
                        self.settle_scope(&mut attempt, id, &identity, &err.to_string())
                            .await?;
                        return Ok(false);
                    }
                    Ok(true)
                }
                Stage::Observed { id, identity } => {
                    self.publish_observed(id, &identity, &unresolved).await?;
                    if identity.empty_scope() {
                        self.settle_scope(&mut attempt, id, &identity, &err.to_string())
                            .await?;
                        return Ok(false);
                    }
                    Ok(true)
                }
            }
        }
        .await;
        match result {
            Ok(false) => FailureReport {
                class: classify_spawn_failure(&err),
                message: err.to_string(),
                error: err,
            },
            Ok(true) => FailureReport {
                class: FailureClass::Unreconciled,
                message: unresolved,
                error: err,
            },
            Err(error) => FailureReport {
                class: FailureClass::Unreconciled,
                message: format!("{unresolved}; recovery: {error}"),
                error: err,
            },
        }
    }

    pub(super) async fn start_body_locked(
        self: &Arc<Self>,
        serial: tokio::sync::OwnedMutexGuard<()>,
        paths: VerifiedRuntimePaths,
        increment_restart_count: bool,
        last_error: Option<String>,
    ) -> Result<StartedVia> {
        let mut attempt = RecoveryAttempt::new();
        match self.prepare_recovery_start(&paths, &mut attempt).await {
            Ok(true) => Ok(StartedVia::Takeover),
            Ok(false) => {
                self.spawn_process_transition(
                    serial,
                    paths,
                    increment_restart_count,
                    last_error,
                    attempt,
                )
                .await?;
                Ok(StartedVia::Spawn)
            }
            Err(error) => {
                let report = self.finalize_recovery_failure(error, attempt).await;
                Err(self.record_guard_refusal(report).await)
            }
        }
    }

    async fn prepare_recovery_start(
        self: &Arc<Self>,
        paths: &VerifiedRuntimePaths,
        attempt: &mut RecoveryAttempt,
    ) -> Result<bool> {
        let record = self
            .repo
            .shared_daemon_runtime_get(self.backend_id.as_str())
            .await?;
        attempt.stage = Stage::Existing;
        let record = self.claim_legacy(record).await?;
        let guard = SharedCodexHome::new(paths.paths().home.clone())
            .verify_expected_mcp_servers(EXPECTED_MCP_SERVERS);
        if guard.is_ok() && self.try_takeover_live(paths, &record).await? {
            return Ok(true);
        }
        // Only a successful non-adoption or an explicit guard refusal hands H
        // to reconciliation. Settings/path/install errors leave Existing intact.
        attempt.stage = Stage::Reconciling(FailedIdentity::retained(&record));
        match self.resolve_persisted_shape(paths, &record).await {
            ShapeResolution::Unreconciled { needs_operator } => {
                let mut message =
                    unreconciled_message(self.backend_id.as_str(), &record, needs_operator);
                if let Err(error) = &guard {
                    message.push_str(&format!(
                        "; refusing to launch shared codex app-server: {error}"
                    ));
                }
                return Err(CalmError::CodexAppServer(message));
            }
            ShapeResolution::ProvenAbsent => {
                if let Some(id) = record.recovery.scope_id() {
                    self.settle_scope(
                        attempt,
                        id,
                        &FailedIdentity::retained(&record),
                        &guard
                            .as_ref()
                            .err()
                            .map(|error| {
                                format!("refusing to launch shared codex app-server: {error}")
                            })
                            .unwrap_or_else(|| "original scope settled".into()),
                    )
                    .await?;
                } else {
                    attempt.stage = Stage::Settled(FailedIdentity::retained(&record));
                }
            }
        }
        if let Err(error) = guard {
            return Err(CalmError::CodexAppServer(format!(
                "refusing to launch shared codex app-server: {error}"
            )));
        }
        Ok(false)
    }

    /// A read error or scope conflict returns a refusal with no authority. A
    /// verified replacement carries H directly through cleanup and finalization.
    pub(super) async fn reap_current_child_or_runtime(
        &self,
        paths: &VerifiedRuntimePaths,
        running: Option<RunningProcessParts>,
    ) -> std::result::Result<(), FailureReport> {
        let Some(RunningProcessParts {
            runtime,
            mut child,
            watcher,
            recovery,
        }) = running
        else {
            return Ok(());
        };
        let mut attempt = RecoveryAttempt::new();
        let result: Result<()> = async {
            let record = self
                .repo
                .shared_daemon_runtime_get(self.backend_id.as_str())
                .await?;
            if record.recovery != recovery
                || record.pid != Some(runtime.pid)
                || record.pgid != Some(runtime.pgid)
                || record.process_start_time != Some(runtime.process_start_time)
                || record.boot_id.as_deref() != Some(runtime.boot_id.as_str())
            {
                return Err(CalmError::Conflict(
                    "replacement recovery scope changed".into(),
                ));
            }
            paths
                .check_record(&record)
                .map_err(|e| CalmError::CodexAppServer(e.to_string()))?;
            let record = self.claim_legacy(record).await?;
            attempt.stage = Stage::Reconciling(FailedIdentity::retained(&record));
            watcher.handle.abort();
            let empty = if let Some(child) = child.as_mut() {
                macos_process::terminate_owned(child, runtime.pgid, self.stop_grace)
                    .await
                    .is_ok()
            } else {
                reap_verified_process_group(
                    paths,
                    runtime.pid,
                    runtime.pgid,
                    runtime.process_start_time,
                    &runtime.boot_id,
                    self.stop_grace,
                )
                .await
            };
            if !empty {
                return Err(CalmError::CodexAppServer(
                    "replacement cleanup did not prove the original group empty".into(),
                ));
            }
            if let Some(id) = record.recovery.scope_id() {
                self.settle_scope(
                    &mut attempt,
                    id,
                    &FailedIdentity::retained(&record),
                    "replacement scope settled",
                )
                .await?;
            }
            Ok(())
        }
        .await;
        match result {
            Ok(()) => Ok(()),
            Err(error) => Err(self.finalize_recovery_failure(error, attempt).await),
        }
    }

    pub(super) async fn record_guard_refusal(self: &Arc<Self>, report: FailureReport) -> CalmError {
        self.fail_in_memory_and_arm_heal(report.message, report.class)
            .await;
        report.error
    }
}
