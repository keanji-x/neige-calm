//! Portable Legacy service remains available; the registered executor requires Linux.
use calm_session::control::execution::*;
use std::{path::PathBuf, sync::Arc};

#[derive(Clone)]
pub struct ExecutorConfig {
    pub journal_directory: PathBuf,
    pub helper_program: PathBuf,
    pub kernel_client_secret: CapabilityToken,
}
pub struct Executor {
    boot: ExecutorBootId,
}
fn unavailable() -> anyhow::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "registered executor requires Linux",
    )
    .into()
}
impl Executor {
    pub fn open(_: ExecutorConfig) -> anyhow::Result<Arc<Self>> {
        Err(unavailable())
    }
    pub fn boot(&self) -> &ExecutorBootId {
        &self.boot
    }
    pub fn authenticates(&self, _: &CapabilityToken) -> bool {
        false
    }
    pub async fn dispatch(
        self: &Arc<Self>,
        _: &CapabilityToken,
        _: ExecutionRequest,
    ) -> ExecutionReply {
        ExecutionReply::Refused {
            reason: unavailable().to_string(),
        }
    }
    pub(crate) async fn serve_registered(
        self: &Arc<Self>,
        _: &tokio::net::UnixStream,
        _: &CapabilityToken,
        _: ExecutionRequest,
        _: Vec<std::os::fd::OwnedFd>,
    ) -> anyhow::Result<()> {
        Err(unavailable())
    }
    pub(crate) fn legacy_allowed(&self, _: &str) -> anyhow::Result<()> {
        Err(unavailable())
    }
    pub(crate) fn reserve_legacy(&self, _: &str) -> anyhow::Result<()> {
        Err(unavailable())
    }
    pub async fn stop_known(&self) {}
}
pub fn run_scope_helper(_: i32, _: i32) -> anyhow::Result<()> {
    Err(unavailable())
}
