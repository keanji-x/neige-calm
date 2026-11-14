//! Read-only contract selection shared by template preflight and report writes.
use super::{CalmError, ReportBlock, TrackReportPayload};
use calm_types::report_blocks::{KIND_PROSE, flat_text};

/// Select the contract only when preserving a written report. Both recommendation preflight
/// and the authoritative write use this rule; headerless templates can still initialize reports.
pub(crate) fn template_report_contract<'a>(
    current: &TrackReportPayload,
    template: &'a TrackReportPayload,
) -> Result<Option<&'a ReportBlock>, CalmError> {
    if !current.report_startup_read_required() {
        return Ok(None);
    }
    let unsupported = || {
        CalmError::BadRequest(
        "the selected template needs a pure comment contract Block 0 to preserve a written report".into(),
    )
    };
    let contract = template
        .blocks
        .as_deref()
        .and_then(|blocks| blocks.first())
        .ok_or_else(unsupported)?;
    let text = flat_text(contract);
    if contract.kind != KIND_PROSE
        || !text.starts_with(calm_types::report_contract::HEADER_OPEN)
        || !calm_types::report_contract::is_pure_comment_block(&text)
    {
        return Err(unsupported());
    }
    Ok(Some(contract))
}
