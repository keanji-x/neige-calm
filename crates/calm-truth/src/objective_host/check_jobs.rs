//! Evaluator bindings and the Report/result fences. Keys are opaque lookup identities.
use super::*;
use calm_types::report_blocks::{canonical_json, tasks::task_block_is_live};
use calm_types::track_report::ReportBlock;
use objective::{check::CheckConfig, engine::Evaluation, standard::standard_hash};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn payload_hash(payload: &Value) -> String {
    format!(
        "sha256:{:x}",
        Sha256::digest(canonical_json(payload).as_bytes())
    )
}

/// Claim and bind inside Requested's transaction. Never trusts a caller's stale row.
pub async fn bind_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &IntentId,
    task_key: &str,
    payload: &Value,
) -> Result<()> {
    capability::require_tx(tx, &id.track_id).await?;
    let row = lifecycle::find_tx(tx, id)
        .await?
        .ok_or_else(|| TruthError::NotFound("objective intent"))?;
    if row.status != IntentStatus::Pending {
        return Err(TruthError::Conflict(
            "objective check binding requires pending intent",
        ));
    }
    let Intent::RunCheck {
        objective,
        evaluation,
        check,
        revision,
        ..
    } = &row.intent
    else {
        return Err(TruthError::Conflict(
            "objective check binding requires RunCheck",
        ));
    };
    let (_, state) = load_state_tx(tx, &id.track_id).await?;
    let current = state.objectives.get(objective);
    let valid = current.is_some_and(|o| {
        o.status == objective::engine::Status::Open
        && matches!(&o.evaluation, Some(Evaluation::Running { id, revision: r, .. }) if id == evaluation && r == revision)
        && matches!(&row.effect, StoredEffect::RunCheck { standard_hash: hash } if *hash == standard_hash(&o.definition))
    });
    if !valid {
        return Err(TruthError::Conflict(
            "objective check binding is no longer current",
        ));
    }
    if !matches!(
        current.unwrap().definition.check(check).map(|c| &c.config),
        Some(CheckConfig::Evaluator(_))
    ) {
        return Err(TruthError::Conflict(
            "objective check binding requires evaluator",
        ));
    }
    if payload.get("key").and_then(Value::as_str) != Some(task_key) {
        return Err(TruthError::Conflict(
            "objective check task key differs from payload",
        ));
    }
    // A pre-existing unbound block must not be adopted, even if byte-identical.
    let blocks = report_blocks_tx(tx, &id.track_id).await?;
    if blocks
        .iter()
        .any(|b| b.kind == "task" && b.payload.get("key").and_then(Value::as_str) == Some(task_key))
    {
        return Err(TruthError::Conflict(
            "objective check task key is occupied by an unbound block",
        ));
    }
    intent_mark_dispatched_tx(tx, id).await?;
    sqlx::query("INSERT INTO objective_check_jobs(track_id,event_seq,ordinal,task_key,block_payload_hash) VALUES(?,?,?,?,?)")
        .bind(id.track_id.as_str()).bind(sql_seq(id.seq)?).bind(i64::from(id.ordinal)).bind(task_key).bind(payload_hash(payload))
        .execute(&mut **tx).await?;
    Ok(())
}

/// Inspect authoritative CRDT blocks, including reports whose cache predates the
/// materialized block layout. Reuse the Report codec's lazy layout conversion.
pub async fn report_blocks_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
) -> Result<Vec<ReportBlock>> {
    use calm_report::document::ReportDoc;
    let row: Option<(String,Option<Vec<u8>>)> = sqlx::query_as(
        "SELECT payload,body_crdt FROM cards WHERE track_id=? AND role='reportcard' AND kind='track-report'",
    ).bind(track.as_str()).fetch_optional(&mut **tx).await?;
    let Some((payload, bytes)) = row else {
        return Ok(vec![]);
    };
    let payload: calm_types::track_report::TrackReportPayload = serde_json::from_str(&payload)?;
    let mut doc = match bytes {
        Some(bytes) => ReportDoc::from_bytes(&bytes).map_err(internal)?,
        None => ReportDoc::from_payload(&payload),
    };
    doc.ensure_blocks_layout(payload.blocks.as_deref())
        .map_err(internal)?;
    doc.blocks_snapshot().map_err(internal)
}

pub async fn bound_hash_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    key: &str,
) -> Result<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT block_payload_hash FROM objective_check_jobs WHERE track_id=? AND task_key=?",
    )
    .bind(track.as_str())
    .bind(key)
    .fetch_optional(&mut **tx)
    .await?)
}

/// Only changed task keys are looked up, after the indexed capability fence.
/// Tombstones remain legal; deletion or conversion to another block kind is not.
pub async fn guard_report_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    before: &[ReportBlock],
    after: &[ReportBlock],
) -> Result<()> {
    if !capability::enabled_tx(tx, track).await? {
        return Ok(());
    }
    let before_by_id: std::collections::BTreeMap<_, _> =
        before.iter().map(|b| (&b.id, b)).collect();
    let after_by_id: std::collections::BTreeMap<_, _> = after.iter().map(|b| (&b.id, b)).collect();
    let changed = before
        .iter()
        .chain(after)
        .filter(|b| b.kind == "task" && before_by_id.get(&b.id) != after_by_id.get(&b.id));
    let mut keys = std::collections::BTreeSet::new();
    for block in changed {
        if let Some(key) = block.payload.get("key").and_then(Value::as_str) {
            keys.insert(key);
        }
    }
    for key in keys {
        let Some(hash) = bound_hash_tx(tx, track, key).await? else {
            continue;
        };
        let next: Vec<_> = after
            .iter()
            .filter(|b| {
                b.kind == "task" && b.payload.get("key").and_then(Value::as_str) == Some(key)
            })
            .collect();
        // Withdrawal evidence cannot disappear: this guard also rejects removing
        // the last bound block. An absent key is therefore only an initial
        // declaration (including repair after binding committed before the block).
        let was_withdrawn = before.iter().any(|b| {
            b.kind == "task"
                && b.payload.get("key").and_then(Value::as_str) == Some(key)
                && !task_block_is_live(b)
        });
        if was_withdrawn && next.iter().any(|b| task_block_is_live(b)) {
            return Err(TruthError::Forbidden(
                "withdrawn objective evaluator task keys cannot be revived".into(),
            ));
        }
        if next.len() != 1
            || (task_block_is_live(next[0]) && payload_hash(&next[0].payload) != hash)
        {
            return Err(TruthError::Forbidden(
                "bound objective evaluator blocks are immutable; only a tombstone is allowed"
                    .into(),
            ));
        }
    }
    Ok(())
}

/// Worker done books the sample, regardless of any later Planner verdict. The
/// caller owns result-contract admission, task CAS and rollback of this transaction.
pub async fn report_done_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    task_key: &str,
    result: &Value,
) -> Result<Option<Reaction>> {
    let binding: Option<(i64, i64)> = sqlx::query_as(
        "SELECT event_seq,ordinal FROM objective_check_jobs WHERE track_id=? AND task_key=?",
    )
    .bind(track.as_str())
    .bind(task_key)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((seq, ordinal)) = binding else {
        return Ok(None);
    };
    let id = IntentId {
        track_id: track.clone(),
        seq: LogSeq(u64::try_from(seq).map_err(internal)?),
        ordinal: u32::try_from(ordinal).map_err(internal)?,
    };
    let row = lifecycle::find_tx(tx, &id)
        .await?
        .ok_or_else(|| internal("bound intent missing"))?;
    if !matches!(row.status, IntentStatus::Dispatched { .. }) {
        tracing::info!(intent=%id,"completed evaluator intent is no longer dispatched");
        return Ok(None);
    }
    let Intent::RunCheck {
        objective,
        evaluation,
        check,
        revision,
        sample,
    } = row.intent
    else {
        return Err(internal("bound intent is not RunCheck"));
    };
    let output =
        calm_types::task_result::evaluation_v1_sample(result).map_err(TruthError::BadRequest)?;
    react_tx(
        tx,
        &ObjectiveInput {
            track_id: track.clone(),
            source: InputSource::IntentJob { intent: id },
            event: Event::CheckFinished {
                objective,
                evaluation,
                check,
                revision,
                sample,
                output: objective::check::SampleOutput::Evaluator(output),
            },
        },
        now_ms()?,
    )
    .await
    .map(Some)
}

/// Replay the immutable request prefix so later settlements or title/budget edits
/// cannot alter a retried declaration's rubric or baseline.
pub async fn request_jobs_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    seq: LogSeq,
) -> Result<(State, Vec<StoredIntent>)> {
    let log = log::read_tx(tx, track).await?;
    let prefix: Vec<_> = log.into_iter().filter(|row| row.seq.0 <= seq.0).collect();
    let state = super::fold_log(&prefix).0;
    let ordinals: Vec<i64> = sqlx::query_scalar("SELECT ordinal FROM objective_intents WHERE track_id=? AND event_seq=? AND kind='run_check' ORDER BY ordinal")
        .bind(track.as_str()).bind(sql_seq(seq)?).fetch_all(&mut **tx).await?;
    let mut rows = vec![];
    for ordinal in ordinals {
        let id = IntentId {
            track_id: track.clone(),
            seq,
            ordinal: u32::try_from(ordinal).map_err(internal)?,
        };
        rows.push(
            lifecycle::find_tx(tx, &id)
                .await?
                .ok_or_else(|| internal("request intent missing"))?,
        );
    }
    Ok((state, rows))
}

/// A restarted Planner may have a new request-source session. The current
/// evaluation's persisted RunCheck identity still owns its original declarations.
pub async fn evaluation_request_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    objective: &objective::ids::ObjectiveKey,
    evaluation: objective::ids::EvaluationId,
) -> Result<Option<LogSeq>> {
    let seq: Option<i64> = sqlx::query_scalar("SELECT event_seq FROM objective_intents WHERE track_id=? AND kind='run_check' AND json_extract(intent_json,'$.objective')=? AND json_extract(intent_json,'$.evaluation')=? ORDER BY event_seq LIMIT 1")
        .bind(track.as_str()).bind(objective.as_str()).bind(i64::try_from(evaluation.sequence()).map_err(internal)?)
        .fetch_optional(&mut **tx).await?;
    seq.map(|seq| u64::try_from(seq).map(LogSeq).map_err(internal))
        .transpose()
}
