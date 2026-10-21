//! Roster admission for objective-bearing templates, shared by builtin and site sources.
use calm_types::report_blocks::{objectives::objective_blocks, reassign_ids, split_body};
use calm_types::track_capability::TrackCapability;

pub(super) fn validate(body: &str, capabilities: &[TrackCapability]) -> Result<(), String> {
    // Use the same block parser as ReportDoc::from_payload, without building a disposable
    // CRDT for every template at boot. Admission only needs the parsed definitions.
    let blocks = reassign_ids(&[], &split_body(body));
    let objectives = objective_blocks(&blocks)?;
    if !objectives.is_empty() && !capabilities.contains(&TrackCapability::Objectives) {
        return Err("objective blocks require the objectives capability".into());
    }
    Ok(())
}
