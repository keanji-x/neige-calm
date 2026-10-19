//! Kernel workspace policy. Mechanisms live in the `workspace` package.
pub(crate) mod identity;
pub(crate) mod ports;
pub(crate) mod sweep;
pub mod trash;

/// Short digest of a workspace path for an idempotency key: a key that does not name the path turns any
/// re-point into a permanent 409, since operation rows are never deleted.
pub(crate) fn workspace_key_digest(path: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(path.as_bytes());
    hex::encode(hasher.finalize())[..16].to_string()
}
