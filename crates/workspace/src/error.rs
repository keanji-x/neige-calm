#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("internal: {0}")]
    Internal(String),
}
pub type Result<T> = std::result::Result<T, Error>;
