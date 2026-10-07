//! Errors and narrow kernel contracts; no server, storage or HTTP types.
#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("internal: {0}")]
    Internal(String),
}
pub type DomainResult<T> = Result<T, DomainError>;
/// Host errors retain their composing kernel's type and exact route mappings.
pub trait ErrorFactory: std::error::Error + Send + Sync + From<DomainError> + 'static {
    fn not_found(reason: String) -> Self;
    fn conflict(reason: String) -> Self;
    fn bad_request(reason: String) -> Self;
    fn internal(reason: String) -> Self;
    fn service_unavailable(reason: String) -> Self;
    fn plugin_install(reason: String) -> Self;
    fn plugin_conflict(reason: String) -> Self;
    fn plugin_dir_occupied(reason: String) -> Self;
    fn plugin_busy(reason: String) -> Self;
    fn plugin_kernel_too_old(reason: String) -> Self;
    fn is_client_refusal(&self) -> bool;
    fn reason(&self) -> String;
    fn to_rpc(self) -> crate::mcp::RpcError;
    fn is_conflict(&self, reason: &str) -> bool;
}
