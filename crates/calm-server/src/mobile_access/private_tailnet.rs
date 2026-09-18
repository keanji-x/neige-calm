//! One fixed, revocable ingress and one typed app-control client. No admin token.
use super::{ingress::RevocableListener, pairing::PairingState};
use crate::auth::SessionStore;
use crate::error::{CalmError, Result};
use axum::Router;
use calm_tailnet_control::TailnetClient;
use calm_types::tailnet::{TailnetAction, TailnetResponse, TailnetStatus};
use serde::Deserialize;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrivateTailnetConfig {
    provider: String,
    control_socket: PathBuf,
    ingress_socket: PathBuf,
}
impl PrivateTailnetConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let config: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        anyhow::ensure!(cfg!(target_os = "linux"), "Private Tailnet requires Linux");
        anyhow::ensure!(
            config.provider == "private-tailnet"
                && config.control_socket.is_absolute()
                && config.ingress_socket.is_absolute(),
            "Invalid private Tailnet configuration"
        );
        anyhow::ensure!(
            config.control_socket.parent() == config.ingress_socket.parent()
                && config.ingress_socket.file_name() == Some(std::ffi::OsStr::new("ingress.sock"))
                && config.control_socket.file_name() == Some(std::ffi::OsStr::new("app.sock")),
            "Tailnet sockets must use the fixed private ingress and control paths"
        );
        anyhow::ensure!(
            config.ingress_socket.as_os_str().len() < 104
                && config.control_socket.as_os_str().len() < 104,
            "Tailnet state path is too long for Unix sockets; configure a shorter private path"
        );
        Ok(config)
    }
}

async fn bind_private(path: &Path) -> anyhow::Result<UnixListener> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Private ingress needs a parent"))?;
    let metadata = std::fs::symlink_metadata(parent)?;
    let uid = unsafe { nix::libc::geteuid() };
    anyhow::ensure!(
        metadata.is_dir()
            && metadata.uid() == uid
            && metadata.permissions().mode() & 0o777 == 0o700,
        "Private ingress directory must be owned by this user with mode 0700"
    );
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.file_type().is_socket()
                    && metadata.uid() == uid
                    && metadata.permissions().mode() & 0o077 == 0,
                "Refusing to replace non-private ingress path"
            );
            match tokio::time::timeout(Duration::from_secs(1), UnixStream::connect(path)).await {
                Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                    std::fs::remove_file(path)?
                }
                _ => anyhow::bail!("Private ingress is already active; it was left untouched"),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

pub(super) struct Controller {
    client: TailnetClient,
    operation: Arc<tokio::sync::Mutex<()>>,
    pairings: Arc<Mutex<PairingState>>,
    sessions: SessionStore,
}
/// Held by the binary outside AuthState to break the router/auth ownership cycle.
pub struct PrivateIngress {
    stop: CancellationToken,
}
impl Drop for PrivateIngress {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
impl Controller {
    pub fn enrollment_client(&self) -> TailnetClient {
        self.client.clone()
    }
    pub async fn configured(
        config: PrivateTailnetConfig,
        router: Arc<Router>,
        pairings: Arc<Mutex<PairingState>>,
        sessions: SessionStore,
    ) -> anyhow::Result<(Self, PrivateIngress)> {
        let listener = bind_private(&config.ingress_socket).await?;
        let client = TailnetClient::new(config.control_socket);
        let stop = CancellationToken::new();
        let weak = Arc::downgrade(&router);
        let operation = Arc::new(tokio::sync::Mutex::new(()));
        let task_operation = operation.clone();
        let task_pairings = pairings.clone();
        let task_sessions = sessions.clone();
        let task_stop = stop.clone();
        let task_client = client.clone();
        tokio::spawn(async move {
            let pairings = task_pairings;
            let sessions = task_sessions;
            let Some(router) = weak.upgrade() else { return };
            let server = axum::serve(
                RevocableListener {
                    listener,
                    state: pairings.clone(),
                },
                (*router).clone(),
            );
            let monitor = async {
                loop {
                    {
                        let _guard = task_operation.lock().await;
                        let status = task_client
                            .request(TailnetAction::Status)
                            .await
                            .ok()
                            .map(|r| r.status);
                        reconcile_origin(&pairings, &sessions, status);
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            };
            tokio::select! { _=task_stop.cancelled()=>{},_ = std::future::IntoFuture::into_future(server)=>{},_=monitor=>{} }
            if let Ok(mut state) = pairings.lock() {
                state.disable(&sessions);
            }
        });
        Ok((
            Self {
                client,
                operation,
                pairings,
                sessions,
            },
            PrivateIngress { stop },
        ))
    }
    pub async fn request(&self, action: TailnetAction) -> Result<TailnetResponse> {
        let _guard = self.operation.lock().await;
        let response = self.client.request(action).await.map_err(|_| {
            CalmError::BadRequest(
                "Private Tailnet service unavailable. Check the local Neige service and retry."
                    .into(),
            )
        })?;
        reconcile_origin(
            &self.pairings,
            &self.sessions,
            Some(response.status.clone()),
        );
        Ok(response)
    }
    pub async fn status(&self) -> Result<TailnetStatus> {
        self.request(TailnetAction::Status).await.map(|r| r.status)
    }
}

fn reconcile_origin(
    pairings: &Arc<Mutex<PairingState>>,
    sessions: &SessionStore,
    status: Option<TailnetStatus>,
) {
    // A network outage or a temporarily unavailable control socket is not an
    // owner revocation. Preserve paired sessions until an explicit stop/logout
    // or a newly verified origin changes the selected node.
    let Some(status) = status else { return };
    if let Ok(mut state) = pairings.lock() {
        if !status.desired_enabled {
            state.disable(sessions);
            return;
        }
        if status.https_ready
            && let Some(origin) = status.origin
            && state.origin.as_ref() != Some(&origin)
        {
            state.disable(sessions);
            state.origin = Some(origin);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn private_tailnet_never_unlinks_active_or_non_socket_ingress() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = temp.path().join("ingress.sock");
        let listener = bind_private(&path).await.unwrap();
        assert!(bind_private(&path).await.is_err());
        assert!(UnixStream::connect(&path).await.is_ok());
        drop(listener);
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, "do-not-delete").unwrap();
        assert!(bind_private(&path).await.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "do-not-delete");
    }
    #[tokio::test]
    async fn private_tailnet_rejects_unprotected_directory_and_recovers_only_stale_socket() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("ingress.sock");
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(bind_private(&path).await.is_err());
        assert!(!path.exists());
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        drop(bind_private(&path).await.unwrap());
        let replacement = bind_private(&path).await.unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(replacement);
    }
}
