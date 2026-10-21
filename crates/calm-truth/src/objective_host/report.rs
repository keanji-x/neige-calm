//! Definitions derived from the report row, inside the report writer's transaction.
use super::*;
use calm_types::report_blocks::objectives::objective_blocks;
use calm_types::track_report::ReportBlock;

// Source admission requires a materialized snapshot. Do not inherit the legacy
// report reader's absent-docRev default when binding a derived input.
#[derive(Deserialize)]
struct ReportSnapshot {
    #[serde(rename = "docRev")]
    doc_rev: u64,
    blocks: Vec<ReportBlock>,
}

/// Admit only the exact definition present in this track's current report revision.
pub(super) async fn verify_source_tx(
    tx: &mut Transaction<'_, Sqlite>,
    input: &ObjectiveInput,
) -> Result<()> {
    let InputSource::ReportBlock {
        report_card_id,
        doc_rev,
        block_id,
    } = &input.source
    else {
        return Err(TruthError::Forbidden(
            "objective report source required".into(),
        ));
    };
    let row: Option<String> = sqlx::query_scalar(
        "SELECT payload FROM cards WHERE id=? AND track_id=? AND kind='track-report' AND role='reportcard'",
    ).bind(report_card_id.as_str()).bind(input.track_id.as_str()).fetch_optional(&mut **tx).await?;
    let forbidden = || {
        TruthError::Forbidden("objective input does not equal the committed report block".into())
    };
    let payload: ReportSnapshot =
        serde_json::from_str(&row.ok_or_else(forbidden)?).map_err(|_| forbidden())?;
    if payload.doc_rev != *doc_rev {
        return Err(forbidden());
    }
    let definitions = objective_blocks(&payload.blocks).map_err(|_| forbidden())?;
    let expected = definitions
        .values()
        .find(|(block, _)| block.id == *block_id)
        .map(|(_, definition)| Event::DefinitionChanged {
            objective: definition.clone(),
        });
    let expected = expected.ok_or_else(forbidden)?;
    if log::payload(&expected).map_err(|_| forbidden())?
        != log::payload(&input.event).map_err(|_| forbidden())?
    {
        return Err(forbidden());
    }
    Ok(())
}

pub struct ReportObjectiveWrite<'a> {
    pub track_id: &'a TrackId,
    pub report_card_id: &'a str,
    pub doc_rev: u64,
    pub before: &'a [ReportBlock],
    pub after: &'a [ReportBlock],
}

/// Called only after persisting the report row, through the shared structural/edit funnel.
pub async fn project_report_tx(
    tx: &mut Transaction<'_, Sqlite>,
    write: ReportObjectiveWrite<'_>,
    at_ms: i64,
) -> Result<()> {
    let ReportObjectiveWrite {
        track_id: track,
        report_card_id,
        doc_rev,
        before,
        after,
    } = write;
    let before = objective_blocks(before).map_err(TruthError::BadRequest)?;
    let after = objective_blocks(after).map_err(TruthError::BadRequest)?;
    if before.is_empty() && after.is_empty() {
        return Ok(());
    }
    capability::require_tx(tx, track).await?;
    let (_, state) = snapshot::load_tx(tx, track).await?;
    for key in before.keys().filter(|key| !after.contains_key(*key)) {
        if state
            .objectives
            .get(&objective::ids::ObjectiveKey::new(key))
            .is_some_and(|o| o.status == objective::engine::Status::Open)
        {
            return Err(TruthError::Conflict("cannot remove an Open objective"));
        }
    }
    for (key, (block, definition)) in after {
        let previous = before.get(&key);
        if previous.is_some_and(|(old, _)| {
            calm_types::report_blocks::canonical_json(&old.payload)
                == calm_types::report_blocks::canonical_json(&block.payload)
        }) {
            continue;
        }
        let existing = state.objectives.get(&definition.key);
        if previous.is_none() && existing.is_some() {
            return Err(TruthError::Conflict("objective key has already been used"));
        }
        if previous.is_some()
            && !existing.is_some_and(|o| o.status == objective::engine::Status::Open)
        {
            return Err(TruthError::Conflict("only Open objectives may change"));
        }
        react_tx(
            tx,
            &ObjectiveInput {
                track_id: track.clone(),
                event: Event::DefinitionChanged {
                    objective: definition,
                },
                source: InputSource::ReportBlock {
                    report_card_id: report_card_id.into(),
                    doc_rev,
                    block_id: block.id.clone(),
                },
            },
            at_ms,
        )
        .await?;
    }
    Ok(())
}
