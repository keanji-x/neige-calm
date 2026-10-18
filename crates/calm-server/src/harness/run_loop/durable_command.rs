//! Durable acceptance belongs to the queue owner, even after its HTTP caller leaves.
use super::*;

pub(super) async fn handle_durable(
    inner: &Arc<Inner>,
    deliveries: Vec<HarnessObservationDelivery>,
    key: &SendKey,
    source: Option<crate::conversation_inputs::HistoricalHumanSource>,
) -> Result<DurableAck> {
    let _issuance = inner.issuance.lock().await;
    if let Some(source) = &source {
        source
            .require_card(inner.repo.as_ref(), inner.card_id.as_str())
            .await?;
    }
    crate::conversation_archive::require_unarchived(inner.repo.as_ref(), inner.card_id.as_str())
        .await?;
    let (ack, checkpoint) = stage_durable_entries(inner, deliveries).await?;
    if let Err(error) =
        persist_snapshot_for_durable_send(inner, key, ack.entry_id.as_ref(), source).await
    {
        restore_durable_user_message(inner, checkpoint).await?;
        return Err(error);
    }
    Ok(ack)
}
