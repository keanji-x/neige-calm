//! Reap a Linux socket peer before the supervisor may unlink its socket and replace it.
use super::*;

pub(super) async fn reap_listener_if_alive(
    paths: &VerifiedRuntimePaths,
    grace: Duration,
) -> Result<()> {
    reap_listener_observing_pgid(paths, grace, |pid| {
        // SAFETY: getpgid only observes the peer PID obtained from the connected socket.
        let pgid = unsafe { libc::getpgid(pid) };
        if pgid < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(pgid)
        }
    })
    .await
}

async fn reap_listener_observing_pgid(
    paths: &VerifiedRuntimePaths,
    grace: Duration,
    getpgid: impl FnOnce(i32) -> std::io::Result<i32>,
) -> Result<()> {
    let sock_path = &paths.paths().socket;
    let Ok(stream) = UnixStream::connect(sock_path).await else {
        return Ok(());
    };
    let peer_pid = match stream.peer_cred().and_then(|cred| {
        cred.pid().filter(|pid| *pid > 0).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "listener peer PID is unavailable",
            )
        })
    }) {
        Ok(pid) => pid,
        Err(error) => {
            tracing::warn!(
                target: "shared_codex_daemon::stop",
                %error,
                sock = %sock_path.display(),
                "SO_PEERCRED failed; proceeding to unlink listener-bound socket without reap"
            );
            return Ok(());
        }
    };
    let scope = match getpgid(peer_pid) {
        Ok(pgid) => {
            tracing::warn!(
                target: "shared_codex_daemon::stop",
                peer_pid,
                pgid,
                sock = %sock_path.display(),
                "stale socket has live listener; reaping orphaned daemon pgid before unlink"
            );
            SignalScope::Group { pgid }
        }
        Err(error) => {
            tracing::warn!(
                target: "shared_codex_daemon::stop",
                peer_pid,
                %error,
                sock = %sock_path.display(),
                "getpgid failed; falling back to pid-only reap of stale socket listener"
            );
            SignalScope::Pid { pid: peer_pid }
        }
    };
    drop(stream);
    let outcome = terminate_group_with_grace(
        paths,
        scope,
        ExitWait::ProcPresence { pid: peer_pid },
        grace,
    )
    .await;
    // Both scopes must preserve this failed observation through the unlink/spawn boundary.
    // A later successful read cannot erase Unknown from this termination attempt.
    if outcome == LeaderExit::Unknown {
        return Err(CalmError::CodexAppServer(format!(
            "stale listener process {peer_pid} exit could not be observed; refusing replacement"
        )));
    }
    Ok(())
}

#[cfg(feature = "fixtures")]
impl SharedCodexAppServer {
    /// Inject just getpgid's result; peer credentials, signals and exit observation stay native.
    pub async fn reap_stale_listener_with_pgid_for_test(
        &self,
        pgid: std::io::Result<i32>,
    ) -> Result<()> {
        let paths = self.verify_backend_isolation().await?;
        reap_listener_observing_pgid(&paths, self.stop_grace, |_| pgid).await
    }
}
