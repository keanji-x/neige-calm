//! Endpoint effects belong to a fresh, unentered reservation, independently of
//! any process-group settlement. No connect errno authorizes file deletion.
use super::recovery::{RecoveryAttempt, Reservation, Stage};
use super::*;
use crate::db::DaemonRecovery;
use std::ffi::CString;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Endpoint {
    Missing,
    Removed,
    Busy,
}

/// Public macOS SDK sys/fcntl.h AT_NODELETEBUSY. unlinkat(2) refuses removal
/// when the vnode has open references. Older kernels may reject this flag.
/// There is deliberately no ordinary-unlink fallback. The no-follow/type
/// checks constrain the leaf; they are not an arbitrary-object atomic CAS
/// against malicious same-UID replacement between fstatat and unlinkat.
const AT_NODELETEBUSY: libc::c_int = 0x4000;

pub(super) fn conditional_delete(
    paths: &VerifiedRuntimePaths,
    _reservation: &Reservation,
) -> std::io::Result<Endpoint> {
    let socket = &paths.paths().socket;
    let parent = socket
        .parent()
        .ok_or_else(|| std::io::Error::other("socket has no parent"))?;
    let parent = std::fs::canonicalize(parent)?;
    let expected = std::fs::metadata(&parent)?;
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&parent)?;
    let held = directory.metadata()?;
    if held.dev() != expected.dev()
        || held.ino() != expected.ino()
        || held.uid() != unsafe { libc::geteuid() }
        || held.mode() & 0o022 != 0
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "socket parent identity or permissions changed",
        ));
    }
    let leaf = socket
        .file_name()
        .ok_or_else(|| std::io::Error::other("socket has no leaf"))?;
    let leaf =
        CString::new(leaf.as_bytes()).map_err(|_| std::io::Error::other("invalid socket leaf"))?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            leaf.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        let error = std::io::Error::last_os_error();
        return if error.kind() == std::io::ErrorKind::NotFound {
            Ok(Endpoint::Missing)
        } else {
            Err(error)
        };
    }
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFSOCK || stat.st_uid != unsafe { libc::geteuid() } {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "endpoint is not an owned socket leaf",
        ));
    }
    if unsafe { libc::unlinkat(directory.as_raw_fd(), leaf.as_ptr(), AT_NODELETEBUSY) } == 0 {
        return Ok(Endpoint::Removed);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::EBUSY) => Ok(Endpoint::Busy),
        Some(libc::ENOENT) => Ok(Endpoint::Missing),
        Some(libc::EINVAL | libc::ENOTSUP) => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "public busy socket deletion is unsupported",
        )),
        _ => Err(error),
    }
}

impl SharedCodexAppServer {
    pub(super) async fn prepare_endpoint(
        &self,
        paths: &VerifiedRuntimePaths,
        attempt: &mut RecoveryAttempt,
        grace: Duration,
    ) -> Result<()> {
        loop {
            attempt.stage = Stage::Reserved(self.begin_recovery(paths).await?);
            #[cfg(test)]
            {
                let barrier = self.recovery_hooks.after_begin.lock().unwrap().take();
                if let Some((entered, resume)) = barrier {
                    let _ = entered.send(());
                    resume.await.expect("release committed recovery begin");
                }
            }
            let Stage::Reserved(reservation) = &attempt.stage else {
                unreachable!()
            };
            #[cfg(test)]
            let forced_error = self
                .recovery_hooks
                .socket_cleanup_error
                .lock()
                .unwrap()
                .take();
            #[cfg(not(test))]
            let forced_error: Option<std::io::ErrorKind> = None;
            let endpoint = match forced_error {
                Some(kind) => Err(std::io::Error::new(
                    kind,
                    "injected endpoint capability failure",
                )),
                None => conditional_delete(paths, reservation),
            }?;
            match endpoint {
                Endpoint::Missing | Endpoint::Removed => return Ok(()),
                Endpoint::Busy => {
                    // Synchronously consume NoProcessEntry before calling connect,
                    // including before its first await and every possible error.
                    let id = attempt.enter()?;
                    self.recover_listener_scope(paths, attempt, id, grace)
                        .await?;
                    // This listener obligation ended. A new reservation must
                    // independently inspect the current endpoint before spawning.
                }
            }
        }
    }

    async fn recover_listener_scope(
        &self,
        paths: &VerifiedRuntimePaths,
        attempt: &mut RecoveryAttempt,
        id: crate::db::RecoveryId,
        grace: Duration,
    ) -> Result<()> {
        let stream = UnixStream::connect(&paths.paths().socket)
            .await
            .map_err(|error| {
                CalmError::CodexAppServer(format!(
                    "{}listener scope unavailable (backend={}, phase/scope={:?}): {error}",
                    unreconciled_prefix(true),
                    self.backend_id.as_str(),
                    DaemonRecovery::Pending { id }
                ))
            })?;
        let peer_pid = stream
            .peer_cred()
            .map_err(|e| CalmError::CodexAppServer(format!("read listener credentials: {e}")))?
            .pid()
            .filter(|pid| *pid > 0)
            .ok_or_else(|| CalmError::CodexAppServer("listener peer PID is unavailable".into()))?;
        #[cfg(test)]
        if let Some(hook) = self.before_identity_capture.lock().unwrap().take() {
            hook(peer_pid);
        }
        #[cfg(test)]
        self.recovery_hooks.pause_after_acquire(peer_pid).await;
        let (process, boot_id) = macos_process::observe_identity(
            peer_pid,
            #[cfg(test)]
            *self.identity_capture_fault.lock().unwrap(),
        );
        let mut identity =
            FailedIdentity::proven_absent(&paths.paths().socket, &paths.paths().home);
        identity.recovery = DaemonRecovery::Recorded { id };
        identity.pid = Some(peer_pid);
        identity.pgid = process.map(|p| p.pgid);
        identity.process_start_time = process.map(|p| p.start_time);
        identity.boot_id = boot_id.clone();
        identity.started_at = Some(now_ms());
        attempt.stage = Stage::Observed {
            id,
            identity: identity.clone(),
        };
        let publication = self
            .publish_observed(
                id,
                &identity,
                "unreconciled: stale listener cleanup pending",
            )
            .await;
        #[cfg(test)]
        if publication.is_err() {
            let barrier = self
                .recovery_hooks
                .after_listener_publish_failure
                .lock()
                .unwrap()
                .take();
            if let Some((entered, resume)) = barrier {
                let _ = entered.send(());
                resume.await.expect("release failed listener publication");
            }
        }
        publication?;
        drop(stream);
        let listener = process
            .filter(|p| !p.is_zombie)
            .zip(boot_id)
            .map(|(process, boot_id)| macos_process::ListenerIdentity { process, boot_id })
            .ok_or_else(|| {
                CalmError::CodexAppServer(format!(
                    "listener process {peer_pid} identity unavailable"
                ))
            })?;
        macos_process::terminate_listener(listener, identity.pgid, grace)
            .await
            .map_err(|e| {
                CalmError::CodexAppServer(format!("reap listener process {peer_pid}: {e}"))
            })?;
        self.settle_scope(attempt, id, &identity, "listener scope settled")
            .await
    }

    #[cfg(test)]
    pub(super) async fn recover_socket(
        &self,
        paths: &VerifiedRuntimePaths,
        grace: Duration,
    ) -> Result<()> {
        let mut attempt = RecoveryAttempt::new();
        match self.prepare_endpoint(paths, &mut attempt, grace).await {
            Ok(()) => {
                let Stage::Reserved(reservation) = attempt.stage else {
                    unreachable!()
                };
                self.cancel_reserved(reservation, "endpoint prepared without process entry")
                    .await
            }
            Err(error) => {
                let report = self.finalize_recovery_failure(error, attempt).await;
                Err(report.error)
            }
        }
    }
}
