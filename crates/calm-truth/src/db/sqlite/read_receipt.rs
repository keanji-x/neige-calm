//! `read_receipts` storage (#2596): the one user's read state, kept by the kernel so a refresh or a
//! second device reads the same state. A mark read keeps the larger instant and clears a manual
//! unread; a mark unread sets it. Both are idempotent. Out of the sync domain: no event is written.

use calm_types::read_receipts::{ReadReceipt, ReadReceiptKind, ReadReceipts};
use sqlx::{Sqlite, SqlitePool, Transaction};

use crate::error::{CalmError, Result};

#[derive(sqlx::FromRow)]
struct ReceiptRow {
    kind: String,
    target_id: String,
    read_through_ms: i64,
    manual_unread: bool,
}

impl TryFrom<ReceiptRow> for ReadReceipt {
    type Error = CalmError;

    fn try_from(row: ReceiptRow) -> Result<Self> {
        let kind = ReadReceiptKind::parse(&row.kind).ok_or_else(|| {
            CalmError::Internal(format!("read_receipts: stored kind `{}`", row.kind))
        })?;
        Ok(ReadReceipt {
            kind,
            target_id: row.target_id,
            read_through_ms: row.read_through_ms,
            manual_unread: row.manual_unread,
        })
    }
}

/// The baseline and every stored receipt.
pub(super) async fn read_receipts_load(pool: &SqlitePool) -> Result<ReadReceipts> {
    let baseline_ms =
        sqlx::query_scalar("SELECT baseline_ms FROM read_receipt_baseline WHERE singleton = 1")
            .fetch_one(pool)
            .await?;
    let rows = sqlx::query_as::<_, ReceiptRow>(
        "SELECT kind, target_id, read_through_ms, manual_unread FROM read_receipts \
         ORDER BY kind, target_id",
    )
    .fetch_all(pool)
    .await?;
    Ok(ReadReceipts {
        baseline_ms,
        receipts: rows
            .into_iter()
            .map(ReadReceipt::try_from)
            .collect::<Result<_>>()?,
    })
}

/// Raise the receipt to `read_through_ms` (never lower it) and clear a manual unread.
pub(super) async fn read_receipt_mark_read_tx(
    tx: &mut Transaction<'_, Sqlite>,
    kind: ReadReceiptKind,
    target_id: &str,
    read_through_ms: i64,
) -> Result<ReadReceipt> {
    sqlx::query_as::<_, ReceiptRow>(
        "INSERT INTO read_receipts (kind, target_id, read_through_ms, manual_unread) \
         VALUES (?1, ?2, ?3, 0) \
         ON CONFLICT(kind, target_id) DO UPDATE SET \
           read_through_ms = MAX(read_through_ms, excluded.read_through_ms), manual_unread = 0 \
         RETURNING kind, target_id, read_through_ms, manual_unread",
    )
    .bind(kind.as_str())
    .bind(target_id)
    .bind(read_through_ms)
    .fetch_one(&mut **tx)
    .await?
    .try_into()
}

/// Set the manual unread; the instant read through is kept (zero for a target never read).
pub(super) async fn read_receipt_mark_unread_tx(
    tx: &mut Transaction<'_, Sqlite>,
    kind: ReadReceiptKind,
    target_id: &str,
) -> Result<ReadReceipt> {
    sqlx::query_as::<_, ReceiptRow>(
        "INSERT INTO read_receipts (kind, target_id, read_through_ms, manual_unread) \
         VALUES (?1, ?2, 0, 1) \
         ON CONFLICT(kind, target_id) DO UPDATE SET manual_unread = 1 \
         RETURNING kind, target_id, read_through_ms, manual_unread",
    )
    .bind(kind.as_str())
    .bind(target_id)
    .fetch_one(&mut **tx)
    .await?
    .try_into()
}
