//! `task_replacements` (migration 0116): one immutable row per accepted replacement. The replay
//! response and the successor's carry plan are rebuilt from its columns plus the immutable
//! `task_candidates` row it names; nothing is re-read from the task rows.

use serde_json::{Value, json};
use sqlx::Row;

use crate::db::sqlite::task_get_tx;
use crate::error::{CalmError, Result};
use crate::model::TaskStatus;
use crate::operation::Tx;

/// How the predecessor ended when the replacement was accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    /// The replacement moved a `pending` or `running` predecessor to `canceled`.
    CanceledNow,
    /// The predecessor had already ended.
    AlreadyTerminal,
}

impl Stop {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::CanceledNow => "canceled_now",
            Self::AlreadyTerminal => "already_terminal",
        }
    }

    fn from_column(value: &str) -> Result<Self> {
        match value {
            "canceled_now" => Ok(Self::CanceledNow),
            "already_terminal" => Ok(Self::AlreadyTerminal),
            other => Err(CalmError::Internal(format!(
                "task replacement stop {other:?} is not canceled_now or already_terminal"
            ))),
        }
    }
}

/// Why a successor carries nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CarryNone {
    /// The request said `carry: "none"`.
    RequestedNone,
    /// Neither the predecessor nor its own replacement had a settled candidate.
    NoCandidate,
}

impl CarryNone {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::RequestedNone => "requested_none",
            Self::NoCandidate => "no_candidate",
        }
    }

    fn from_column(value: &str) -> Result<Self> {
        match value {
            "requested_none" => Ok(Self::RequestedNone),
            "no_candidate" => Ok(Self::NoCandidate),
            other => Err(CalmError::Internal(format!(
                "task replacement carry_none_reason {other:?} is unknown"
            ))),
        }
    }
}

/// The candidate a successor carries, by the attempt that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CarrySource {
    From {
        source_attempt_id: String,
        source_candidate_id: String,
    },
    None(CarryNone),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Receipt {
    pub receipt_id: String,
    pub track_id: String,
    pub predecessor_attempt_id: String,
    pub predecessor_key: String,
    pub successor_key: String,
    pub request_idempotency_key: String,
    pub request_fingerprint: String,
    pub reason: String,
    pub prior_status: TaskStatus,
    pub stop: Stop,
    pub carry: CarrySource,
    pub created_at_ms: i64,
}

const COLUMNS: &str = "receipt_id, track_id, predecessor_attempt_id, predecessor_key, \
     successor_key, request_idempotency_key, request_fingerprint, reason, prior_status, stop, \
     source_attempt_id, source_candidate_id, carry_none_reason, created_at_ms";

fn status_str(status: TaskStatus) -> Result<String> {
    serde_json::to_value(status)?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| CalmError::Internal("task status does not serialize as a string".into()))
}

fn from_row(row: sqlx::sqlite::SqliteRow) -> Result<Receipt> {
    let prior_status: String = row.try_get("prior_status")?;
    let stop: String = row.try_get("stop")?;
    let source_attempt_id: Option<String> = row.try_get("source_attempt_id")?;
    let source_candidate_id: Option<String> = row.try_get("source_candidate_id")?;
    let none: Option<String> = row.try_get("carry_none_reason")?;
    let receipt_id: String = row.try_get("receipt_id")?;
    let carry = match (source_attempt_id, source_candidate_id, none) {
        (Some(source_attempt_id), Some(source_candidate_id), None) => CarrySource::From {
            source_attempt_id,
            source_candidate_id,
        },
        (None, None, Some(reason)) => CarrySource::None(CarryNone::from_column(&reason)?),
        _ => {
            return Err(CalmError::Internal(format!(
                "task replacement {receipt_id} carry columns disagree"
            )));
        }
    };
    Ok(Receipt {
        track_id: row.try_get("track_id")?,
        predecessor_attempt_id: row.try_get("predecessor_attempt_id")?,
        predecessor_key: row.try_get("predecessor_key")?,
        successor_key: row.try_get("successor_key")?,
        request_idempotency_key: row.try_get("request_idempotency_key")?,
        request_fingerprint: row.try_get("request_fingerprint")?,
        reason: row.try_get("reason")?,
        prior_status: serde_json::from_value(Value::String(prior_status))?,
        stop: Stop::from_column(&stop)?,
        carry,
        created_at_ms: row.try_get("created_at_ms")?,
        receipt_id,
    })
}

async fn one_tx(tx: &mut Tx<'_>, filter: &str, binds: &[&str]) -> Result<Option<Receipt>> {
    let sql = format!("SELECT {COLUMNS} FROM task_replacements WHERE {filter}");
    let mut query = sqlx::query(&sql);
    for bind in binds {
        query = query.bind(*bind);
    }
    query
        .fetch_optional(&mut **tx)
        .await?
        .map(from_row)
        .transpose()
}

/// The receipt a request key already produced on this Track.
pub(crate) async fn by_request_tx(
    tx: &mut Tx<'_>,
    track_id: &str,
    request_idempotency_key: &str,
) -> Result<Option<Receipt>> {
    one_tx(
        tx,
        "track_id = ?1 AND request_idempotency_key = ?2",
        &[track_id, request_idempotency_key],
    )
    .await
}

/// The receipt that replaced one predecessor attempt.
pub(crate) async fn by_predecessor_tx(
    tx: &mut Tx<'_>,
    predecessor_attempt_id: &str,
) -> Result<Option<Receipt>> {
    one_tx(tx, "predecessor_attempt_id = ?1", &[predecessor_attempt_id]).await
}

/// The receipt that declared `key` on this Track as a successor.
pub(crate) async fn by_successor_tx(
    tx: &mut Tx<'_>,
    track_id: &str,
    successor_key: &str,
) -> Result<Option<Receipt>> {
    one_tx(
        tx,
        "track_id = ?1 AND successor_key = ?2",
        &[track_id, successor_key],
    )
    .await
}

pub(crate) async fn insert_tx(tx: &mut Tx<'_>, receipt: &Receipt) -> Result<()> {
    let (source_attempt_id, source_candidate_id, none) = match &receipt.carry {
        CarrySource::From {
            source_attempt_id,
            source_candidate_id,
        } => (
            Some(source_attempt_id.as_str()),
            Some(source_candidate_id.as_str()),
            None,
        ),
        CarrySource::None(reason) => (None, None, Some(reason.as_str())),
    };
    let sql = format!(
        "INSERT INTO task_replacements ({COLUMNS}) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"
    );
    sqlx::query(&sql)
        .bind(&receipt.receipt_id)
        .bind(&receipt.track_id)
        .bind(&receipt.predecessor_attempt_id)
        .bind(&receipt.predecessor_key)
        .bind(&receipt.successor_key)
        .bind(&receipt.request_idempotency_key)
        .bind(&receipt.request_fingerprint)
        .bind(&receipt.reason)
        .bind(status_str(receipt.prior_status)?)
        .bind(receipt.stop.as_str())
        .bind(source_attempt_id)
        .bind(source_candidate_id)
        .bind(none)
        .bind(receipt.created_at_ms)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// The commit a candidate pins (`task_candidates` rows never change).
pub(crate) async fn candidate_commit_tx(tx: &mut Tx<'_>, candidate_id: &str) -> Result<String> {
    sqlx::query_scalar("SELECT commit_sha FROM task_candidates WHERE candidate_id = ?1")
        .bind(candidate_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| {
            CalmError::Internal(format!(
                "task replacement names candidate {candidate_id}, which has no row"
            ))
        })
}

/// The tool response, rebuilt from the receipt alone (plus the candidate commit it names).
pub(crate) async fn response_tx(
    tx: &mut Tx<'_>,
    receipt: &Receipt,
    replayed: bool,
) -> Result<Value> {
    let carry = match &receipt.carry {
        CarrySource::From {
            source_attempt_id,
            source_candidate_id,
        } => json!({
            "source_attempt_id": source_attempt_id,
            "source_candidate_id": source_candidate_id,
            "candidate_sha": candidate_commit_tx(tx, source_candidate_id).await?,
        }),
        CarrySource::None(reason) => json!({ "none": reason.as_str() }),
    };
    Ok(json!({
        "replayed": replayed,
        "receipt_id": receipt.receipt_id,
        "predecessor": {
            "key": receipt.predecessor_key,
            "attempt_id": receipt.predecessor_attempt_id,
            "prior_status": receipt.prior_status,
            "stop": receipt.stop.as_str(),
        },
        "successor": {
            "key": receipt.successor_key,
            "attempt_id": format!("{}:{}", receipt.track_id, receipt.successor_key),
        },
        "carry": carry,
    }))
}

/// What a successor attempt's lease prepare carries: the candidate commit its receipt names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CarryPlan {
    pub receipt_id: String,
    pub source_attempt_id: String,
    pub candidate_sha: String,
    pub created_at_ms: i64,
}

/// The carry plan of the attempt being prepared: its key's receipt, when that receipt names a
/// source. `None` for every attempt that is not a replacement successor, and for `carry.none`.
pub(crate) async fn carry_plan_tx(tx: &mut Tx<'_>, attempt_id: &str) -> Result<Option<CarryPlan>> {
    let Some(task) = task_get_tx(tx, attempt_id).await? else {
        return Ok(None);
    };
    let Some(receipt) = by_successor_tx(tx, &task.track_id, &task.key).await? else {
        return Ok(None);
    };
    let CarrySource::From {
        source_attempt_id,
        source_candidate_id,
    } = receipt.carry
    else {
        return Ok(None);
    };
    Ok(Some(CarryPlan {
        candidate_sha: candidate_commit_tx(tx, &source_candidate_id).await?,
        receipt_id: receipt.receipt_id,
        source_attempt_id,
        created_at_ms: receipt.created_at_ms,
    }))
}
