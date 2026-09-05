//! Owned Linux process boundaries for explicitly admitted local Workers.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unsupported boundary: {0}")]
    Unsupported(String),
    #[error("boundary conflict: {0}")]
    Conflict(String),
    #[error("boundary evidence unavailable: {0}")]
    Evidence(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Mount {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub writable: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchConfig {
    pub attempt_id: String,
    pub network: NetworkPolicy,
    pub workspace: PathBuf,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub mounts: Vec<Mount>,
}

/// Provider networking does not grant networking to untrusted code. The provider
/// integration must enforce its inner sandbox separately when using this mode.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicy {
    Provider,
    Isolated,
}

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub state_root: PathBuf,
    pub helper: PathBuf,
    pub bwrap: PathBuf,
    pub timeout: Duration,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: i32,
    pub start_time: u64,
    pub boot_id: String,
    pub namespace_inode: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BoundaryHandle {
    pub run_id: String,
    pub attempt_id: String,
    pub config_digest: String,
    pub init: ProcessIdentity,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuiescenceProof {
    pub handle: BoundaryHandle,
    pub observed_at_ms: u64,
    pub method: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum BoundaryState {
    Prepared,
    Running,
    Quiesced(QuiescenceProof),
    Unknown(String),
}

#[cfg(target_os = "linux")]
mod init;
#[cfg(target_os = "linux")]
mod launcher;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod runtime;
#[cfg(target_os = "linux")]
mod storage;
#[cfg(target_os = "linux")]
mod transport;

pub struct Runtime {
    config: RuntimeConfig,
}

/// Trusted helper entry point. It never evaluates a shell command string.
pub fn helper_main() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let args: Vec<String> = std::env::args().skip(1).collect();
        match args.first().map(String::as_str) {
            Some("init") if args.len() == 2 => init::run(&args[1]),
            Some("check") if args.len() == 3 => {
                let policy = serde_json::from_str(&format!("\"{}\"", args[1]))?;
                let inode = args[2]
                    .parse()
                    .map_err(|_| Error::Conflict("invalid parent network identity".into()))?;
                init::check_network(policy, inode)
            }
            Some("launch") if args.len() == 2 => launcher::run(std::path::Path::new(&args[1])),
            _ => Err(Error::Conflict("invalid helper arguments".into())),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(Error::Unsupported("Linux PID namespaces required".into()))
    }
}

#[cfg(not(target_os = "linux"))]
impl Runtime {
    pub fn new(config: RuntimeConfig) -> Result<Self> {
        Ok(Self { config })
    }
    pub fn preflight(&self, _: NetworkPolicy) -> Result<()> {
        let _ = &self.config;
        Err(Error::Unsupported("Linux required".into()))
    }
    pub fn prepare(&self, _: &str, _: &LaunchConfig) -> Result<BoundaryHandle> {
        Err(Error::Unsupported("Linux required".into()))
    }
    pub fn start(&self, _: &BoundaryHandle) -> Result<()> {
        Err(Error::Unsupported("Linux required".into()))
    }
    pub fn probe(&self, _: &BoundaryHandle) -> Result<BoundaryState> {
        Err(Error::Unsupported("Linux required".into()))
    }
    pub fn stop(&self, _: &BoundaryHandle, _: Duration) -> Result<BoundaryState> {
        Err(Error::Unsupported("Linux required".into()))
    }
}
