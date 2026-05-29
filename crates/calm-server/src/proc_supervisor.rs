use crate::error::{CalmError, Result};
use calm_session::control::{ControlMsg, ControlReply, EnsureProcRequest};
use calm_session::{read_frame, write_frame};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::UnixStream;

/// Two-phase call: invokes `on_spawned(pid)` between the supervisor's
/// `Spawned` frame and its `Ready`/`ReadyFailed` frame so the caller can
/// persist pid + daemon_handle BEFORE readiness — the pre-#388 ordering
/// the dispatcher's fast-exit-preserve discriminator and partial-spawn
/// reap path both depend on (the rollback path reads daemon_handle to
/// find `.exit` sidecars and reads pid to SIGTERM hung daemons).
///
/// `on_spawned` returning Err is treated as a fatal persistence failure;
/// it aborts before waiting for readiness. Best-effort persistence
/// (e.g. pid that the sweeper can fall back without) should log + drop
/// the error inside the closure.
pub(crate) async fn ensure_proc<F, Fut>(
    control_sock: Option<&Path>,
    request: EnsureProcRequest,
    on_spawned: F,
) -> Result<u32>
where
    F: FnOnce(u32) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let sock = resolve_control_sock(control_sock).await?;
    let mut stream = UnixStream::connect(&sock).await.map_err(|e| {
        CalmError::Internal(format!(
            "connect calm-proc-supervisor {}: {e}",
            sock.display()
        ))
    })?;
    write_frame(&mut stream, &ControlMsg::EnsureProc(request))
        .await
        .map_err(|e| CalmError::Internal(format!("write proc-supervisor request: {e}")))?;
    let first: ControlReply = read_frame(&mut stream)
        .await
        .map_err(|e| CalmError::Internal(format!("read proc-supervisor reply: {e}")))?;
    let pid = match first {
        ControlReply::Spawned { pid } => pid,
        ControlReply::SpawnFailed { error, .. } => return Err(CalmError::Internal(error)),
        other => {
            return Err(CalmError::Internal(format!(
                "unexpected first reply from proc-supervisor: {other:?}"
            )));
        }
    };
    on_spawned(pid).await?;
    let second: ControlReply = read_frame(&mut stream)
        .await
        .map_err(|e| CalmError::Internal(format!("read proc-supervisor readiness reply: {e}")))?;
    match second {
        ControlReply::Ready => Ok(pid),
        ControlReply::ReadyFailed { error, .. } => Err(CalmError::Internal(error)),
        other => Err(CalmError::Internal(format!(
            "unexpected second reply from proc-supervisor: {other:?}"
        ))),
    }
}

#[cfg(feature = "fixtures")]
pub(crate) async fn resolve_control_sock(control_sock: Option<&Path>) -> Result<PathBuf> {
    if let Some(sock) = control_sock {
        return Ok(sock.to_path_buf());
    }
    // Per-call fixture: cross-runtime sharing isn't viable because every
    // `#[tokio::test]` builds its own runtime, so a globally-cached
    // supervisor task would die between tests. The fixture binds the
    // listener synchronously before returning (see test_support) so
    // there's no listen race despite the per-call cost.
    let fixture = calm_proc_supervisor::test_support::InProcessProcSupervisor::start()
        .await
        .map_err(|e| CalmError::Internal(format!("start in-process proc-supervisor: {e:#}")))?;
    let sock = fixture.sock().to_path_buf();
    let _ = Box::leak(Box::new(fixture));
    Ok(sock)
}

#[cfg(not(feature = "fixtures"))]
pub(crate) async fn resolve_control_sock(control_sock: Option<&Path>) -> Result<PathBuf> {
    control_sock
        .map(Path::to_path_buf)
        .ok_or_else(|| CalmError::Internal("proc-supervisor socket is not configured".into()))
}

pub(crate) const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(10);
