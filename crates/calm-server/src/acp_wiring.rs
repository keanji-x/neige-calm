//! Application composition for the independent ACP conversation and task roles.
use crate::acp_session::host::{AcpHost as AcpPlannerHost, AcpPlannerConfig};
use crate::error::Result;
use std::path::{Path, PathBuf};
impl AcpPlannerHost {
    pub fn new(
        config: Option<AcpPlannerConfig>,
        data_dir: &Path,
        mcp_shim: PathBuf,
        mcp_socket: PathBuf,
        managed_tokens: crate::mcp_server::managed_tokens::ManagedSessionTokens,
    ) -> Result<Self> {
        let native_workers = std::sync::Arc::new(crate::acp_worker::Workers::default());
        let (execution, services) = crate::acp_planner::execution::owner();
        Self::with_services(
            config,
            data_dir,
            mcp_shim,
            mcp_socket,
            managed_tokens,
            execution,
            services,
            native_workers,
        )
    }
    pub fn unconfigured_scratch() -> Result<Self> {
        Self::scratch_with_tokens(Default::default())
    }
    pub(crate) fn scratch_with_tokens(
        tokens: crate::mcp_server::managed_tokens::ManagedSessionTokens,
    ) -> Result<Self> {
        let scratch = tempfile::Builder::new().prefix("neige-acp-").tempdir()?;
        let mut host = Self::new(
            None,
            scratch.path(),
            "neige-mcp-stdio-shim".into(),
            scratch.path().join("mcp.sock"),
            tokens,
        )?;
        host._scratch = Some(scratch);
        Ok(host)
    }
}
