//! Kernel-owned immutable ordinary-file and restricted local Git snapshots.
//! Callers MUST hold a trusted write boundary throughout capture and exclude the store from worker writable mounts;
//! a boundary ID is an assertion, not a runtime proof.

#[cfg(target_os = "linux")]
mod capture;
#[cfg(target_os = "linux")]
mod file;
#[cfg(target_os = "linux")]
mod file_set;
#[cfg(target_os = "linux")]
mod filesystem;
#[cfg(target_os = "linux")]
mod git_index;
#[cfg(target_os = "linux")]
mod materialize;
// The portable DTOs remain available to callers that refuse artifact delivery.
// Their internal persistence validators are only consumed by the Linux backend.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod model;
#[cfg(target_os = "linux")]
mod store;

pub use model::*;
#[cfg(target_os = "linux")]
pub use store::ArtifactStore;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("artifact IO: {0}")]
    Io(#[from] std::io::Error),
    #[error("artifact manifest: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid artifact request: {0}")]
    Invalid(String),
    #[error("unsupported artifact source: {0}")]
    Unsupported(String),
    #[error("artifact limit exceeded: {0}")]
    Limit(String),
    #[error("artifact integrity mismatch: {0}")]
    Integrity(String),
    #[error("capture key already binds a different request")]
    Conflict,
    #[error("output {output} is missing declared paths: {paths:?}")]
    MissingOutput { output: String, paths: Vec<String> },
    #[error("destination already exists: {0}")]
    DestinationExists(std::path::PathBuf),
}
