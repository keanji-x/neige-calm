//! Ordinary wakes freeze Report suggestions in the existing trusted context segment.
//! Stored claims and explicit retries keep their original context without regenerating it.
use super::*;
use calm_types::observation::{Observation, ReportEditPresentation};
use sqlx::{Sqlite, Transaction};

pub(super) async fn append_tx(
    tx: &mut Transaction<'_, Sqlite>,
    source: &InputSource,
    carrier: &CurrentInputCarrier,
    track: &TrackId,
    contexts: &mut Vec<SystemRenderContext>,
) -> Result<()> {
    crate::conversation_inputs::authorize_resource_tx(tx, source, carrier).await?;
    // Canonical claims select exactly one pending input. Its frozen Report suggestion
    // already covers that observation; historical observations retain their own copy.
    if matches!(carrier.checkpoint().pending.first(),
    Some(PendingInput::System { observation, .. })
    if matches!(observation.observation(), Observation::ReportEdited {
        presentation: ReportEditPresentation::Knowledge { .. }, ..
    })) {
        return Ok(());
    }
    let text =
        crate::report_read_ledger::delta::for_track_tx(tx, track, carrier.worker_session_id())
            .await?;
    if !text.is_empty() {
        contexts.push(
            SystemRenderContext::new(HarnessInputPresentation::SystemReportEdited, text)
                .map_err(|error| CalmError::Conflict(error.into()))?,
        );
    }
    Ok(())
}
