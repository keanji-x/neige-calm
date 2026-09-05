//! Kernel-owned immutable snapshots for restricted local Git workspaces.
//!
//! Callers MUST establish and retain a trusted write boundary throughout capture,
//! exclude the store and preparation parent from worker writable mounts, and check
//! consumption authority. A boundary ID is an assertion, not a runtime proof.
//! Git only inspects index modes; it never transforms captured content. This crate
//! does not assess acceptance, schedule work, or collect retained snapshots. See
//! the crate README for persistence and recovery contracts.

#[cfg(target_os = "linux")]
mod capture;
#[cfg(target_os = "linux")]
mod filesystem;
#[cfg(target_os = "linux")]
mod git_index;
#[cfg(target_os = "linux")]
mod materialize;
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
