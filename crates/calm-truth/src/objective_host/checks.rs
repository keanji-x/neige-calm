//! Authorization of host-owned check execution, shared by admission and release.
use super::*;
use objective::{
    definition::CheckDefinition,
    engine::{Evaluation, Status},
    standard::standard_hash,
};

/// An unsettled intent whose approved standard and current running evaluation still match.
/// The caller owns the writer transaction through freezing, dispatch or release.
pub async fn current_check_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &IntentId,
) -> Result<Option<(StoredIntent, CheckDefinition)>> {
    let Some(row) = lifecycle::find_tx(tx, id).await? else {
        return Ok(None);
    };
    if matches!(row.status, IntentStatus::Settled { .. }) {
        return Ok(None);
    }
    let Intent::RunCheck {
        objective,
        evaluation,
        check,
        revision,
        ..
    } = &row.intent
    else {
        return Ok(None);
    };
    let (_, state) = load_state_tx(tx, &id.track_id).await?;
    let Some(current) = state.objectives.get(objective) else {
        return Ok(None);
    };
    if current.status != Status::Open
        || !current.is_approved()
        || !matches!(&current.evaluation, Some(Evaluation::Running { id, revision: r, .. }) if id == evaluation && r == revision)
        || !matches!(&row.effect, StoredEffect::RunCheck { standard_hash: hash } if *hash == standard_hash(&current.definition))
    {
        return Ok(None);
    }
    let definition = current
        .definition
        .check(check)
        .cloned()
        .ok_or_else(|| internal("current check is missing"))?;
    Ok(Some((row, definition)))
}
