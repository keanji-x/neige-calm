use crate::error::{CalmError, Result};
use std::path::{Path, PathBuf};

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
