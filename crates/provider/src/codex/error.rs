//! Native Codex failures, independent of HTTP and persistence.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("codex app-server: {0}")]
    Transport(String),
    #[error("codex refused: {0}")]
    Refused(String),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
}
pub type Result<T> = std::result::Result<T, Error>;
