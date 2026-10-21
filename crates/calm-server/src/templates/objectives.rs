//! Roster admission for objective-bearing templates, shared by builtin and site sources.
use crate::track_report_doc::ReportDoc;
use calm_types::report_blocks::objectives::objective_blocks;
use calm_types::track_capability::TrackCapability;
use calm_types::track_report::TrackReportPayload;

pub(super) fn validate(body: &str, capabilities: &[TrackCapability]) -> Result<(), String> {
    let doc = ReportDoc::from_payload(&TrackReportPayload::new("", body));
    let blocks = doc.blocks_snapshot().map_err(|error| error.to_string())?;
    let objectives = objective_blocks(&blocks)?;
    if !objectives.is_empty() && !capabilities.contains(&TrackCapability::Objectives) {
        return Err("objective blocks require the objectives capability".into());
    }
    Ok(())
}
