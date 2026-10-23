use super::*;

/// The executor boundary deliberately cannot express Reported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "settlement", rename_all = "snake_case")]
pub enum ExecutorSettlement {
    Executed,
    Delivered,
    Superseded { reason: String },
    Failed { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "settlement", rename_all = "snake_case")]
pub enum IntentSettlement {
    Reported { result_seq: LogSeq },
    Executed,
    Delivered,
    Superseded { reason: String },
    Failed { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntentStatus {
    Pending,
    Dispatched {
        at_ms: i64,
    },
    Settled {
        at_ms: i64,
        settlement: IntentSettlement,
    },
}

impl From<ExecutorSettlement> for IntentSettlement {
    fn from(value: ExecutorSettlement) -> Self {
        match value {
            ExecutorSettlement::Executed => Self::Executed,
            ExecutorSettlement::Delivered => Self::Delivered,
            ExecutorSettlement::Superseded { reason } => Self::Superseded { reason },
            ExecutorSettlement::Failed { reason } => Self::Failed { reason },
        }
    }
}

/// A stable boot-resume view, in deterministic intent identity order.
pub async fn unsettled_intents(db: &SqliteDb) -> Result<Vec<StoredIntent>> {
    let mut tx = begin_immediate_tx(db).await?;
    let rows = sqlx::query("SELECT * FROM objective_intents WHERE status<>'settled' ORDER BY track_id,event_seq,ordinal")
        .fetch_all(&mut **tx).await?;
    let result = rows.iter().map(decode).collect::<Result<Vec<_>>>()?;
    tx.commit().await?;
    Ok(result)
}

/// The per-track executor view; avoids reading other tracks' durable work.
pub async fn unsettled_intents_for_track(
    db: &SqliteDb,
    track: &TrackId,
) -> Result<Vec<StoredIntent>> {
    let mut tx = begin_immediate_tx(db).await?;
    let rows = sqlx::query("SELECT * FROM objective_intents WHERE track_id=? AND status<>'settled' ORDER BY event_seq,ordinal")
        .bind(track.as_str()).fetch_all(&mut **tx).await?;
    let result = rows.iter().map(decode).collect::<Result<Vec<_>>>()?;
    tx.commit().await?;
    Ok(result)
}

fn decode(row: &sqlx::sqlite::SqliteRow) -> Result<StoredIntent> {
    use sqlx::Row;
    let intent: Intent = serde_json::from_str(row.try_get("intent_json")?).map_err(internal)?;
    let effect = match (&intent, row.try_get::<&str, _>("kind")?) {
        (Intent::RunCheck { .. }, "run_check") => StoredEffect::RunCheck {
            standard_hash: serde_json::from_value(serde_json::Value::String(
                row.try_get("standard_hash")?,
            ))
            .map_err(internal)?,
        },
        (Intent::Execute { .. }, "execute") => StoredEffect::Execute {
            target_session_id: WorkerSessionId::from(
                row.try_get::<String, _>("target_session_id")?,
            ),
        },
        (Intent::Notify { .. }, "notify") => StoredEffect::Notify,
        _ => return Err(internal("intent kind mismatch")),
    };
    let status = match row.try_get::<&str, _>("status")? {
        "pending" => IntentStatus::Pending,
        "dispatched" => IntentStatus::Dispatched {
            at_ms: row.try_get("dispatched_at_ms")?,
        },
        "settled" => IntentStatus::Settled {
            at_ms: row.try_get("settled_at_ms")?,
            settlement: serde_json::from_str(row.try_get("settlement_json")?).map_err(internal)?,
        },
        _ => return Err(internal("unknown intent status")),
    };
    Ok(StoredIntent {
        id: IntentId {
            track_id: TrackId::from(row.try_get::<String, _>("track_id")?),
            seq: LogSeq(u64::try_from(row.try_get::<i64, _>("event_seq")?).map_err(internal)?),
            ordinal: u32::try_from(row.try_get::<i64, _>("ordinal")?).map_err(internal)?,
        },
        intent,
        effect,
        status,
        created_at_ms: row.try_get("created_at_ms")?,
    })
}

async fn find_tx(tx: &mut Transaction<'_, Sqlite>, id: &IntentId) -> Result<Option<StoredIntent>> {
    let row = sqlx::query(
        "SELECT * FROM objective_intents WHERE track_id=? AND event_seq=? AND ordinal=?",
    )
    .bind(id.track_id.as_str())
    .bind(sql_seq(id.seq)?)
    .bind(i64::from(id.ordinal))
    .fetch_optional(&mut **tx)
    .await?;
    row.as_ref().map(decode).transpose()
}

/// Only pending intents can be claimed. Missing ids are NotFound, all repeated or
/// backwards transitions are Conflict; neither is silently treated as success.
pub async fn intent_mark_dispatched_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &IntentId,
) -> Result<()> {
    let row = find_tx(tx, id)
        .await?
        .ok_or_else(|| TruthError::NotFound("objective intent"))?;
    if row.status != IntentStatus::Pending {
        return Err(TruthError::Conflict("objective intent is not pending"));
    }
    let changed = sqlx::query(
        "UPDATE objective_intents SET status='dispatched',dispatched_at_ms=?
        WHERE track_id=? AND event_seq=? AND ordinal=? AND status='pending'",
    )
    .bind(now_ms()?)
    .bind(id.track_id.as_str())
    .bind(sql_seq(id.seq)?)
    .bind(i64::from(id.ordinal))
    .execute(&mut **tx)
    .await?
    .rows_affected();
    transition_changed(changed)
}

/// Executors may settle pending or dispatched work, but cannot report a check result.
/// Caller owns writer admission and must roll back on error, as for react_tx.
pub async fn intent_settle_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &IntentId,
    settlement: ExecutorSettlement,
) -> Result<()> {
    settle_tx(tx, id, settlement.into(), now_ms()?).await
}

async fn settle_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &IntentId,
    settlement: IntentSettlement,
    at_ms: i64,
) -> Result<()> {
    let row = find_tx(tx, id)
        .await?
        .ok_or_else(|| TruthError::NotFound("objective intent"))?;
    if matches!(row.status, IntentStatus::Settled { .. }) {
        return Err(TruthError::Conflict("objective intent is already settled"));
    }
    if matches!(settlement, IntentSettlement::Reported { .. })
        && !matches!(row.status, IntentStatus::Dispatched { .. })
    {
        return Err(TruthError::Conflict(
            "objective report requires dispatched intent",
        ));
    }
    let changed = sqlx::query(
        "UPDATE objective_intents SET status='settled',settled_at_ms=?,settlement_json=?
        WHERE track_id=? AND event_seq=? AND ordinal=? AND status IN('pending','dispatched')",
    )
    .bind(at_ms)
    .bind(serde_json::to_string(&settlement)?)
    .bind(id.track_id.as_str())
    .bind(sql_seq(id.seq)?)
    .bind(i64::from(id.ordinal))
    .execute(&mut **tx)
    .await?
    .rows_affected();
    transition_changed(changed)
}

fn transition_changed(rows: u64) -> Result<()> {
    if rows == 1 {
        Ok(())
    } else {
        Err(TruthError::Conflict("objective intent transition lost"))
    }
}

/// Dedupe must precede this check: a reported job's identical retry is Duplicate.
pub(super) async fn check_job_tx(
    tx: &mut Transaction<'_, Sqlite>,
    input: &ObjectiveInput,
) -> Result<()> {
    let InputSource::IntentJob { intent: id } = &input.source else {
        return Ok(());
    };
    let stale = || TruthError::Conflict("stale objective check job");
    if id.track_id != input.track_id || sql_seq(id.seq).is_err() {
        return Err(stale());
    }
    let row = find_tx(tx, id).await?.ok_or_else(stale)?;
    if !matches!(row.status, IntentStatus::Dispatched { .. }) {
        return Err(stale());
    }
    match (&row.intent, &input.event) {
        (
            Intent::RunCheck {
                objective,
                evaluation,
                check,
                revision,
                sample,
            },
            Event::CheckFinished {
                objective: got_objective,
                evaluation: got_evaluation,
                check: got_check,
                revision: got_revision,
                sample: got_sample,
                ..
            },
        ) if (objective, evaluation, check, revision, sample)
            == (
                got_objective,
                got_evaluation,
                got_check,
                got_revision,
                got_sample,
            ) =>
        {
            Ok(())
        }
        _ => Err(stale()),
    }
}

/// Called only after the authorized CheckFinished and its effects have been appended.
pub(super) async fn report_tx(
    tx: &mut Transaction<'_, Sqlite>,
    input: &ObjectiveInput,
    seq: LogSeq,
    at_ms: i64,
) -> Result<()> {
    if let InputSource::IntentJob { intent } = &input.source {
        settle_tx(
            tx,
            intent,
            IntentSettlement::Reported { result_seq: seq },
            at_ms,
        )
        .await?;
    }
    Ok(())
}
