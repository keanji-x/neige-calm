//! Registered backend contract; no database handle or resource releaser is provided.
use super::{LaunchPermit, Record};
use crate::error::{CalmError, Result};

#[async_trait::async_trait]
pub(super) trait Backend: Send + Sync {
    type Request: Send;
    async fn launch(&self, permit: LaunchPermit, request: Self::Request) -> LaunchOutcome;
    async fn recover(&self, record: &Record) -> Result<Observation>;
    async fn stop(&self, record: &Record) -> Result<Observation>;
}

pub(super) enum LaunchOutcome {
    Started(String),
    NotIssued(CalmError),
    Uncertain(CalmError),
}

/// Only backend implementations inside this private module family can create observations.
pub(super) struct Observation {
    pub execution: String,
    pub identity: Option<String>,
    pub stopped: bool,
}
