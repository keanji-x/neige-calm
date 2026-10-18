use super::*;
use async_trait::async_trait;
use tokio::sync::watch;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreError(pub String);
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecError {
    Busy(BusyReason),
    Refused(String),
    Transport(String),
}
impl From<std::io::Error> for ExecError {
    fn from(error: std::io::Error) -> Self {
        Self::Transport(error.to_string())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DriveError {
    Store(StoreError),
    Executor(ExecError),
    Timeout,
    Missing,
}
impl From<StoreError> for DriveError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}
impl From<ExecError> for DriveError {
    fn from(error: ExecError) -> Self {
        Self::Executor(error)
    }
}

/// A subscription and snapshot taken atomically before loading/inspecting or retrying Busy.
#[derive(Clone)]
pub struct Generation {
    receiver: watch::Receiver<u64>,
    snapshot: u64,
}
impl Generation {
    pub fn snapshot(receiver: watch::Receiver<u64>) -> Self {
        let snapshot = *receiver.borrow();
        Self { receiver, snapshot }
    }
    pub async fn changed(mut self) -> Result<(), ExecError> {
        loop {
            if *self.receiver.borrow_and_update() != self.snapshot {
                return Ok(());
            }
            self.receiver
                .changed()
                .await
                .map_err(|_| ExecError::Transport("change stream closed".into()))?;
        }
    }
}

#[async_trait]
pub trait Store: Send + Sync {
    type Tx<'t>: Send;
    async fn insert(
        &self,
        tx: &mut Self::Tx<'_>,
        intent: &ExecutionIntent,
    ) -> Result<(), StoreError>;
    async fn request(
        &self,
        tx: &mut Self::Tx<'_>,
        id: &ExecutionId,
        desired: Desired,
    ) -> Result<(), StoreError>;
    async fn load(&self, id: &ExecutionId) -> Result<Option<Stored>, StoreError>;
    async fn spawned(&self, id: &ExecutionId) -> Result<(), StoreError>;
    async fn settle(&self, id: &ExecutionId, settlement: &Settlement) -> Result<(), StoreError>;
    async fn finish(&self, id: &ExecutionId) -> Result<(), StoreError>;
    async fn pending(&self) -> Result<Vec<ExecutionId>, StoreError>;
}
#[async_trait]
pub trait Executor: Send + Sync {
    async fn prepare(&self, intent: &ExecutionIntent) -> Result<Fact, ExecError>;
    async fn launch(&self, consume: &ConsumeExecution) -> Result<Fact, ExecError>;
    async fn revoke(&self, consume: &ConsumeExecution) -> Result<Fact, ExecError>;
    async fn inspect(&self, id: &ExecutionId) -> Result<Fact, ExecError>;
    /// Return only after Watching; all executions share the same change generation.
    async fn generation(&self) -> Result<Generation, ExecError>;
    fn available(&self) -> bool;
    async fn control(&self, control: ExecutionControl) -> Result<ExecutionReply, ExecError>;
    async fn retire(
        &self,
        id: &ExecutionId,
        lifecycle: &CapabilityToken,
    ) -> Result<Fact, ExecError>;
}
