//! Worker outcome belongs to the scheduler, committed with the owner's stop decision.
use crate::{
    db::sqlite,
    error::Result,
    event::BroadcastEnvelope,
    execution_quiesce::owned::DisposalOwner,
    operation::{OperationRuntime, Tx, owned_execution::pipe::PipeConfig},
};
use workspace_runtime::execution::ExecutionMetadata;

pub(crate) struct CardDisposalOwner(pub PipeConfig);

#[async_trait::async_trait]
impl DisposalOwner for CardDisposalOwner {
    async fn prepare_disposal_tx(
        &self,
        tx: &mut Tx<'_>,
        metadata: &ExecutionMetadata,
    ) -> Result<()> {
        self.0.prepare_disposal_tx(tx, metadata).await
    }

    async fn stop_tx(
        &self,
        tx: &mut Tx<'_>,
        metadata: &ExecutionMetadata,
    ) -> Result<Vec<BroadcastEnvelope>> {
        if !self.0.stop_for_disposal_tx(tx, metadata).await? {
            return Ok(Vec::new());
        }
        let card: Option<String> = sqlx::query_scalar(
            "SELECT target_id FROM operations WHERE id=?1 AND target_type='card'",
        )
        .bind(&metadata.owner.0)
        .fetch_optional(&mut **tx)
        .await?
        .flatten();
        let Some(card) = card else {
            return Ok(Vec::new());
        };
        let card = sqlite::card_for_update_tx(tx, &card).await?;
        let events: Vec<(crate::ids::ActorId, crate::event::EventScope, crate::event::Event)> = Vec::new();
        let mut envelopes = Vec::new();
        for (actor, scope, event) in events {
            let id = sqlite::append_decision_event_in_tx(tx, &actor, &scope, None, &event).await?;
            envelopes.push(BroadcastEnvelope {
                id,
                event_version: crate::event::SYNC_EVENT_VERSION,
                actor,
                scope,
                event,
            });
        }
        Ok(envelopes)
    }

    async fn finish(
        &self,
        runtime: &OperationRuntime,
        metadata: &ExecutionMetadata,
    ) -> Result<bool> {
        self.0.finish(runtime, metadata).await
    }
}
