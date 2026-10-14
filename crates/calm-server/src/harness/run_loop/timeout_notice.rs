//! Previous terminal outcome is the authority; preparing an input never consumes it.
use super::Inner;
use crate::model::HarnessInputSegment;
use calm_types::harness::HarnessTimeoutEvidence;

pub(super) async fn previous(inner: &Inner) -> crate::error::Result<Option<HarnessInputSegment>> {
    let Some(row) = inner
        .repo
        .harness_latest_terminal(inner.card_id.as_str(), inner.track_id.as_str())
        .await?
    else {
        return Ok(None);
    };
    let Some(HarnessTimeoutEvidence::V1 {
        request,
        ended_at_ms,
    }) = row.timeout()
    else {
        return Ok(None);
    };
    let key = calm_types::harness::planner_timeout_key(row.id);
    let text = format!(
        include_str!("../../../prompts/previous-turn-timeout.md"),
        key = key,
        turn_id = request.turn_id,
        requested_at_ms = request.requested_at_ms,
        ended_at_ms = ended_at_ms,
        effective_elapsed_ms = request.effective_elapsed_ms,
        budget_ms = request.budget_ms,
    );
    Ok(Some(super::budget::segment(text.trim_end().to_owned())))
}
