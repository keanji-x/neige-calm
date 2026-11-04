//! Select from Truth's sole queue using the current source's existing scope.
use super::*;
use calm_types::conversation::PendingInput;
use std::num::NonZeroU32;

pub(super) async fn plan_tx(
    tx: &mut Transaction<'_, Sqlite>,
    source: &InputSource,
    carrier: &CurrentInputCarrier,
) -> Result<SubmissionRenderPlan> {
    source.require_submission_intent(None)?;
    authorize_resource_tx(tx, source, carrier).await?;
    let pending = &carrier.checkpoint().pending;
    let count = match (source, pending.first()) {
        (InputSource::Human(human), Some(PendingInput::Human { .. })) => {
            let mut count = 0u32;
            for input in pending {
                let PendingInput::Human { message, .. } = input else {
                    break;
                };
                if !execution::current_human_ref_matches_tx(tx, human, carrier, message).await? {
                    break;
                }
                count = count.checked_add(1).ok_or_else(|| {
                    CalmError::Conflict("pending Human prefix is too large".into())
                })?;
            }
            count
        }
        // Dispatcher inputs keep their exact producer-bound selection. Retry
        // intents use their separate relation-backed admission, never this path.
        (_, Some(_)) => 1,
        (_, None) => 0,
    };
    let count = NonZeroU32::new(count)
        .ok_or_else(|| CalmError::Forbidden("source has no qualified pending head".into()))?;
    Ok(conversation::pending_plan_for_prefix_tx(tx, carrier, count, vec![]).await?)
}
