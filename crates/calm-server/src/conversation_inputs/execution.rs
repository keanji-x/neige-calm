//! The shared source decision at claim AND every fresh crossing. The returned
//! provenance/runtime are facts, never a reusable permission for a later write.
use super::*;
use calm_types::conversation::MessageAuthor;

/// The shared current resource decision before a new private effect. Success
/// is not a reusable grant: the owning task keeps the real source and calls
/// this decision again after awaits before beginning another effect.
pub(crate) async fn authorize_human_resource_tx(
    tx: &mut Transaction<'_, Sqlite>,
    source: &HumanInputSource,
    carrier: &CurrentInputCarrier,
) -> Result<()> {
    let area = conversation::conversation_area_tx(tx, carrier.card_id()).await?;
    authorize_legacy_human_area_tx(tx, source, area.id.as_str()).await
}

/// The same private deployment resource policy is used by canonical execution
/// and historical public effects. Content acceptance has its separate ACL.
pub(super) async fn authorize_legacy_human_area_tx(
    tx: &mut Transaction<'_, Sqlite>,
    source: &HumanInputSource,
    area_id: &str,
) -> Result<()> {
    let principal = source.current_principal()?;
    crate::organization_access::require_legacy_input_execution_tx(tx, area_id, &principal).await?;
    Ok(())
}

/// The one current-author/revision rule used by startup and input claims.
pub(super) async fn current_human_ref_matches_tx(
    tx: &mut Transaction<'_, Sqlite>,
    source: &HumanInputSource,
    carrier: &CurrentInputCarrier,
    message: &MessageRevisionRef,
) -> Result<bool> {
    let principal = source.current_principal()?;
    let stored = conversation::message_tx(tx, &message.message_id).await?;
    if stored.card_id != carrier.card_id()
        || stored.current != *message
        || stored.withdrawn_at_ms.is_some()
        || stored.author
            != (MessageAuthor::AuthenticatedHuman {
                registered_user_id: principal.user_id,
            })
    {
        return Ok(false);
    }
    Ok(matches!(
        conversation::revision_tx(tx, message).await?.payload,
        calm_types::conversation::PrivateHistoryPayload::Present { .. }
    ))
}
