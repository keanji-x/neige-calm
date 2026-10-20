//! Opaque admission carried to the actual transport send boundary.
use futures_util::future::BoxFuture;

/// Caller refusal; policy and its original diagnostic stay with the caller.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct SendRefusal(pub String);

/// Invoked under the transport's outbound lock, never as an early grant.
/// Callbacks must not recursively send through that transport.
pub struct PreSendGate<'a> {
    check: Box<dyn Fn() -> BoxFuture<'a, Result<(), SendRefusal>> + Send + Sync + 'a>,
}
impl<'a> PreSendGate<'a> {
    pub fn new(
        check: impl Fn() -> BoxFuture<'a, Result<(), SendRefusal>> + Send + Sync + 'a,
    ) -> Self {
        Self {
            check: Box::new(check),
        }
    }
    pub(crate) async fn run(&self) -> Result<(), SendRefusal> {
        (self.check)().await
    }
}
