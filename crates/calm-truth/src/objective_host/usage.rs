//! Atomic per-turn accumulation; retries are keyed by source, not the cumulative payload.
use super::*;
use objective::definition::Usage;

#[derive(Clone, Copy, Debug)]
pub struct TurnUsage {
    pub context_growth: u64,
    pub secs: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TurnUsageResult {
    Skipped,
    Recorded(Reaction),
}

pub async fn record_turn_usage(
    db: &SqliteDb,
    track: &TrackId,
    session: WorkerSessionId,
    turn_id: String,
    delta: TurnUsage,
) -> Result<TurnUsageResult> {
    let mut tx = begin_immediate_tx(db).await?;
    if !capability::enabled_tx(&mut tx, track).await? {
        tx.commit().await?;
        return Ok(TurnUsageResult::Skipped);
    }
    let source = InputSource::HarnessTurn {
        session_id: session,
        turn_id,
    };
    let (kind, key) = source.identity()?;
    let seq: Option<i64> = sqlx::query_scalar(
        "SELECT seq FROM objective_events WHERE track_id=? AND source_kind=? AND source_key=? AND kind='usage'",
    ).bind(track.as_str()).bind(kind).bind(key).fetch_optional(&mut **tx).await?;
    if let Some(seq) = seq {
        tx.commit().await?;
        return Ok(TurnUsageResult::Recorded(Reaction::Duplicate {
            seq: LogSeq(u64::try_from(seq).map_err(internal)?),
        }));
    }
    let (_, state) = snapshot::load_tx(&mut tx, track).await?;
    let usage = Usage {
        turns: state.usage.turns.saturating_add(1),
        context_tokens: state
            .usage
            .context_tokens
            .saturating_add(delta.context_growth),
        elapsed_secs: state.usage.elapsed_secs.saturating_add(delta.secs),
    };
    let input = ObjectiveInput {
        track_id: track.clone(),
        source,
        event: Event::Usage { usage },
    };
    let reaction = react_tx(&mut tx, &input, now_ms()?).await?;
    tx.commit().await?;
    Ok(TurnUsageResult::Recorded(reaction))
}

/// One SELECT is one SQLite read transaction. Fetch all data before folding, so no
/// read lock can park across a second SQL operation in a shared-cache database.
/// Only a mismatched snapshot joins the log; current snapshots do not scan it.
pub async fn read_state(pool: &sqlx::SqlitePool, track: &TrackId) -> Result<(LogSeq, State)> {
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT s.seq AS state_seq,s.state_version,s.state_json,
         (SELECT COALESCE(MAX(seq),0) FROM objective_events WHERE track_id=q.id) AS tip,
         t.area_id,e.* FROM (SELECT ? AS id) q
         LEFT JOIN tracks t ON t.id=q.id
         LEFT JOIN objective_states s ON s.track_id=q.id
         LEFT JOIN objective_events e ON e.track_id=CASE WHEN s.state_version != ? THEN q.id END
         ORDER BY e.seq",
    )
    .bind(track.as_str())
    .bind(i64::from(objective::engine::STATE_VERSION))
    .fetch_all(pool)
    .await?;
    let row = rows
        .first()
        .ok_or_else(|| internal("missing read envelope"))?;
    let stored = row
        .try_get::<Option<i64>, _>("state_seq")?
        .map(|seq| {
            Ok::<_, TruthError>((
                seq,
                row.try_get("state_version")?,
                row.try_get("state_json")?,
            ))
        })
        .transpose()?;
    let (seq, state) = snapshot::decode(stored, row.try_get("tip")?)?;
    let state = match state {
        Some(state) => state,
        None => super::fold_log(&log::decode_rows(&rows, track)?).0,
    };
    Ok((seq, state))
}
