//! Objective author policy, independent of task attribution and retirement.
use crate::error::CalmError;
use calm_types::{
    event::EditAuthor,
    report_blocks::{KIND_OBJECTIVE, objectives::objective_blocks},
    track_report::ReportBlock,
};

pub(crate) fn guard_objective_blocks(
    before: &[ReportBlock],
    after: &[ReportBlock],
    author: EditAuthor,
) -> Result<(), CalmError> {
    objective_blocks(after).map_err(CalmError::BadRequest)?;
    for old in before.iter().filter(|b| b.kind == KIND_OBJECTIVE) {
        if let Some(new) = after.iter().find(|b| b.id == old.id)
            && (new.kind != old.kind || new.payload.get("key") != old.payload.get("key"))
        {
            return Err(CalmError::BadRequest(
                "objective key and kind are immutable in place".into(),
            ));
        }
    }
    for new in after.iter().filter(|b| b.kind == KIND_OBJECTIVE) {
        if before
            .iter()
            .any(|old| old.id == new.id && old.kind != new.kind)
        {
            return Err(CalmError::BadRequest(
                "block kind cannot change to objective in place".into(),
            ));
        }
    }
    if !matches!(author, EditAuthor::Planner | EditAuthor::User) {
        let unchanged = |a: &ReportBlock, b: &ReportBlock| {
            a.id == b.id
                && a.kind == b.kind
                && calm_types::report_blocks::canonical_json(&a.payload)
                    == calm_types::report_blocks::canonical_json(&b.payload)
        };
        let old: Vec<_> = before.iter().filter(|b| b.kind == KIND_OBJECTIVE).collect();
        let new: Vec<_> = after.iter().filter(|b| b.kind == KIND_OBJECTIVE).collect();
        if old.len() != new.len() || old.iter().any(|a| !new.iter().any(|b| unchanged(a, b))) {
            return Err(CalmError::BadRequest(
                "only Planner and User may create, modify or delete objective blocks".into(),
            ));
        }
    }
    Ok(())
}
