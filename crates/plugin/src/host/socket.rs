//! Manifest v6 `http_socket` at runtime: where a running `app` plugin serves HTTP, and a token
//! that tells every connection proxied there to close once that run stops serving.
use super::*;
use std::path::Path;
use tokio_util::sync::CancellationToken;

/// One run's socket. `serving` is cancelled by [`RunningPlugin::stop_serving`].
pub(super) struct RunSocket {
    path: PathBuf,
    pub(super) serving: CancellationToken,
}

impl RunSocket {
    /// `None` when `manifest` declares no `http_socket`.
    pub(super) fn declared(manifest: &Manifest, work_dir: &Path) -> Option<Self> {
        let name = manifest.http_socket.as_deref()?;
        Some(Self {
            path: crate::http_socket::path(work_dir, name),
            serving: CancellationToken::new(),
        })
    }
}

/// A serving plugin's socket, as handed to a proxy.
#[derive(Debug, Clone)]
pub struct PluginSocket {
    pub path: PathBuf,
    /// Cancelled when the run that serves `path` stops serving: a stop, disable, reload, restart
    /// or crash. Every connection proxied to `path` must close when it fires.
    pub serving: CancellationToken,
}

/// Why [`PluginHost::http_socket`] has no socket to hand out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketUnavailable {
    /// No plugin with this id is installed, or its manifest declares no `http_socket`.
    NotDeclared,
    /// It declares one, but no run of it is serving: not enabled, starting, stopping or crashed.
    NotRunning,
}

impl<E: ErrorFactory> PluginHost<E> {
    /// The socket of the plugin's current run while that run is `Running` and not stopping, the
    /// same availability tool dispatch uses. `disable` stops the run before it clears `enabled`,
    /// so a serving run is always an enabled one.
    pub fn http_socket(&self, id: &str) -> Result<PluginSocket, SocketUnavailable> {
        {
            let table = self.lock_table();
            if let Some(rp) = table.live.get(id)
                && matches!(rp.status, PluginRuntimeStatus::Running)
                && !rp.stopping
            {
                let socket = rp
                    .http_socket
                    .as_ref()
                    .ok_or(SocketUnavailable::NotDeclared)?;
                // A child: a holder can observe the run's end but never end it for others.
                return Ok(PluginSocket {
                    path: socket.path.clone(),
                    serving: socket.serving.child_token(),
                });
            }
        }
        match self.registry.get(id) {
            Some(manifest) if manifest.http_socket.is_some() => Err(SocketUnavailable::NotRunning),
            _ => Err(SocketUnavailable::NotDeclared),
        }
    }
}
