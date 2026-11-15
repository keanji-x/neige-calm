//! Injected task-session services. The shared host never owns concrete task control state.
use super::host::AcpHost;
use crate::{
    error::Result,
    operation::{Operation, SpawnCtx, TxOutput},
    provider_execution::{Claim, CompletionEvidence, KnownEnd, Record},
};
use std::sync::Arc;

#[async_trait::async_trait]
pub(crate) trait NativeWorkers: std::fmt::Debug + Send + Sync {
    fn active_turn(&self, session: &str) -> Option<Option<String>>;
    fn fence(&self, session: &str, turn: Option<&str>) -> bool;
    async fn start(
        self: Arc<Self>,
        host: Arc<AcpHost>,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<()>;
    async fn stop(&self, host: &AcpHost, session: &str, turn: Option<&str>) -> Result<()>;
    /// Return false only if the durable session belongs to another role.
    async fn stop_if_owned(&self, host: &AcpHost, session: &str) -> Result<bool>;
    async fn revoke_owned_tx(
        &self,
        tx: &mut crate::operation::Tx<'_>,
        track: Option<&str>,
    ) -> Result<Vec<String>>;
    async fn recover(&self, repo: &dyn crate::db::Repo, confirmed: bool) -> Result<()>;
    async fn reconcile(&self, host: &AcpHost, session: &str, claim: &Claim) -> Result<()>;
    async fn complete(
        &self,
        session: &str,
        record: &Record,
        evidence: CompletionEvidence,
        known: Option<KnownEnd>,
    ) -> Result<()>;
}
