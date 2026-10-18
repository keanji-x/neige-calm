#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl From<workspace::error::Error> for WorkspaceError {
    fn from(error: workspace::error::Error) -> Self {
        match error {
            workspace::error::Error::Internal(message) => Self::Internal(message),
        }
    }
}

pub type Result<T> = std::result::Result<T, WorkspaceError>;
