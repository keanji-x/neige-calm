//! `calm.task.replace`'s report half, owned by the report transaction (design §4.4): the replay
//! check before any state read (1), the admission, stop and carry source (2–4, in
//! [`crate::task_replace::admission`]), the appended successor block (5) and the receipt (6).
//! All of it commits with the report write or none of it does.

use super::{ReportDoc, ReportDocOp};
use crate::error::{CalmError, Result};
use crate::model::{Card, TaskStatus, now_ms};
use crate::operation::Tx;
use crate::task_replace::ReplaceArgs;
use crate::task_replace::admission::{self, Admitted};
use crate::task_replace::receipt::{self, CarrySource, Receipt, Stop};
use serde_json::Value;

/// A replacement admitted and stopped in this transaction, waiting for its block and receipt.
pub(crate) struct Staged {
    args: ReplaceArgs,
    admitted: Admitted,
    prior_status: TaskStatus,
    stop: Stop,
    carry: CarrySource,
}

impl Staged {
    /// The predecessor joins the `plan.updated` keys when this replacement canceled it.
    pub(super) fn add_stopped_key(&self, changed_keys: &mut Vec<String>) {
        if self.stop == Stop::CanceledNow {
            changed_keys.push(self.admitted.predecessor.key.clone());
            changed_keys.sort();
            changed_keys.dedup();
        }
    }
}

/// Step 1: a request key this Track has seen replays its receipt when the request is the same,
/// and conflicts when it is not. Reads nothing but the receipt.
pub(super) async fn replay_tx(
    tx: &mut Tx<'_>,
    track_id: &str,
    args: &ReplaceArgs,
) -> Result<Option<Value>> {
    let Some(existing) = receipt::by_request_tx(tx, track_id, &args.idempotency_key).await? else {
        return Ok(None);
    };
    if existing.request_fingerprint != args.fingerprint() {
        return Err(crate::task_replace::idempotency_conflict());
    }
    Ok(Some(receipt::response_tx(tx, &existing, true).await?))
}

/// Steps 2–4, then the op that appends the successor right after the predecessor's block.
pub(super) async fn stage_tx(
    tx: &mut Tx<'_>,
    track_id: &str,
    doc: &ReportDoc,
    args: &ReplaceArgs,
) -> Result<(ReportDocOp, Staged)> {
    let blocks = doc
        .blocks_snapshot()
        .map_err(|e| CalmError::Internal(e.to_string()))?;
    let admitted = admission::admit_tx(tx, track_id, &blocks, args).await?;
    let (prior_status, stop) = admission::stop_tx(tx, &admitted).await?;
    let carry = admission::carry_source_tx(tx, &admitted.predecessor, args.carry).await?;
    let op = ReportDocOp::UpsertBlock {
        id: None,
        kind: calm_types::report_blocks::KIND_TASK.into(),
        content: calm_types::report_blocks::render_data_block(
            calm_types::report_blocks::KIND_TASK,
            &admitted.successor_payload,
        )
        .map_err(CalmError::BadRequest)?,
        if_rev: None,
        if_doc_rev: Some(
            doc.doc_rev()
                .map_err(|e| CalmError::Internal(e.to_string()))?,
        ),
        position: Some(admitted.position),
    };
    Ok((
        op,
        Staged {
            args: args.clone(),
            admitted,
            prior_status,
            stop,
            carry,
        },
    ))
}

/// Step 6: the receipt, then the response rebuilt from it exactly as a replay would.
pub(super) async fn finish_tx(tx: &mut Tx<'_>, track_id: &str, staged: Staged) -> Result<Value> {
    let receipt = Receipt {
        receipt_id: uuid::Uuid::new_v4().to_string(),
        track_id: track_id.to_string(),
        predecessor_attempt_id: staged.admitted.predecessor.id.clone(),
        predecessor_key: staged.admitted.predecessor.key.clone(),
        successor_key: staged.admitted.successor_key.clone(),
        request_idempotency_key: staged.args.idempotency_key.clone(),
        request_fingerprint: staged.args.fingerprint(),
        reason: staged.args.reason.clone(),
        prior_status: staged.prior_status,
        stop: staged.stop,
        carry: staged.carry,
        created_at_ms: now_ms(),
    };
    receipt::insert_tx(tx, &receipt).await?;
    receipt::response_tx(tx, &receipt, false).await
}

/// The report card row, for a replay that writes nothing.
pub(super) async fn report_card_tx(tx: &mut Tx<'_>, id: &str) -> Result<Card> {
    let row = sqlx::query_as::<_, crate::db::rows::CardRow>(
        "SELECT id,track_id,kind,sort,payload,title,deletable,created_at,updated_at FROM cards WHERE id=?1",
    )
    .bind(id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(Card::from(row))
}
