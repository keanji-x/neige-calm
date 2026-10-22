use super::*;

pub(super) async fn load_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
) -> Result<(LogSeq, State)> {
    let row: Option<(i64, i64, String)> = sqlx::query_as(
        "SELECT seq,state_version,state_json FROM objective_states WHERE track_id=?",
    )
    .bind(track.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    let tip: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(seq),0) FROM objective_events WHERE track_id=?")
            .bind(track.as_str())
            .fetch_one(&mut **tx)
            .await?;
    let (seq, state) = decode(row, tip)?;
    let state = match state {
        Some(state) => state,
        None => super::fold_log(&log::read_tx(tx, track).await?).0,
    };
    Ok((seq, state))
}

/// None requests pure replay, never snapshot repair.
pub(super) fn decode(row: Option<(i64, i64, String)>, tip: i64) -> Result<(LogSeq, Option<State>)> {
    match row {
        None if tip == 0 => Ok((LogSeq(0), Some(State::default()))),
        None => Err(internal("missing snapshot for nonempty log")),
        Some((seq, version, json)) => {
            if seq != tip {
                return Err(internal("snapshot does not match log tip"));
            }
            let state = if version == i64::from(objective::engine::STATE_VERSION) {
                Some(serde_json::from_str(&json).map_err(internal)?)
            } else {
                None
            };
            Ok((LogSeq(u64::try_from(seq).map_err(internal)?), state))
        }
    }
}

pub(super) async fn store_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    previous: LogSeq,
    seq: LogSeq,
    state: &State,
) -> Result<()> {
    let json = serde_json::to_string(state).map_err(internal)?;
    if serde_json::from_str::<State>(&json).map_err(internal)? != *state {
        return Err(internal("state does not round-trip"));
    }
    let changed = if previous.0 == 0 {
        sqlx::query("INSERT INTO objective_states(track_id,seq,state_version,state_json) VALUES(?,?,?,?) ON CONFLICT(track_id) DO NOTHING")
            .bind(track.as_str()).bind(sql_seq(seq)?).bind(i64::from(objective::engine::STATE_VERSION)).bind(json).execute(&mut **tx).await?.rows_affected()
    } else {
        sqlx::query("UPDATE objective_states SET seq=?,state_version=?,state_json=? WHERE track_id=? AND seq=?")
            .bind(sql_seq(seq)?).bind(i64::from(objective::engine::STATE_VERSION)).bind(json).bind(track.as_str()).bind(sql_seq(previous)?).execute(&mut **tx).await?.rows_affected()
    };
    if changed != 1 {
        return Err(internal("objective snapshot CAS failed"));
    }
    Ok(())
}
