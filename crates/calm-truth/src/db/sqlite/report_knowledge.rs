//! Storage only: Report knowledge policy belongs to the server's Report owner.
use crate::error::{CalmError, Result};
use calm_types::report_knowledge::{HistoryRef, LastRead, ReadBlock};
use sqlx::{Row, Sqlite, Transaction};
use std::collections::HashMap;

fn corrupt(error: impl std::fmt::Display) -> CalmError {
    CalmError::Internal(format!("invalid Report knowledge: {error}"))
}

pub async fn load_tx(
    tx: &mut Transaction<'_, Sqlite>,
    session: &str,
    report: &str,
) -> Result<Option<LastRead>> {
    let row = sqlx::query("SELECT doc_rev, whole, sections_json FROM report_read_ledger WHERE worker_session_id=? AND report_card_id=?")
        .bind(session).bind(report).fetch_optional(&mut **tx).await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let doc_rev = u64::try_from(row.try_get::<i64, _>("doc_rev")?).map_err(corrupt)?;
    let whole = match row.try_get::<i64, _>("whole")? {
        0 => false,
        1 => true,
        other => return Err(corrupt(other)),
    };
    let sections =
        serde_json::from_str(&row.try_get::<String, _>("sections_json")?).map_err(corrupt)?;
    let mut read = LastRead {
        doc_rev,
        whole,
        blocks: HashMap::new(),
        sections,
    };
    for row in sqlx::query("SELECT block_id, rev, history_ref FROM report_read_ledger_blocks WHERE worker_session_id=? AND report_card_id=?")
        .bind(session).bind(report).fetch_all(&mut **tx).await? {
        let id: String = row.try_get("block_id")?;
        let rev = u32::try_from(row.try_get::<i64,_>("rev")?).map_err(corrupt)?;
        let history: HistoryRef = serde_json::from_str(&row.try_get::<String,_>("history_ref")?).map_err(corrupt)?;
        read.blocks.insert(id, ReadBlock { rev, history_ref: history });
    }
    Ok(Some(read))
}

pub async fn save_tx(
    tx: &mut Transaction<'_, Sqlite>,
    session: &str,
    report: &str,
    read: &LastRead,
) -> Result<()> {
    let rev = i64::try_from(read.doc_rev).map_err(corrupt)?;
    sqlx::query(
        "INSERT INTO report_read_ledger(worker_session_id,report_card_id,doc_rev,whole,sections_json)
         VALUES(?,?,?,?,?) ON CONFLICT(worker_session_id,report_card_id) DO UPDATE SET
         doc_rev=excluded.doc_rev,whole=excluded.whole,sections_json=excluded.sections_json",
    )
        .bind(session).bind(report).bind(rev).bind(i64::from(read.whole))
        .bind(serde_json::to_string(&read.sections).map_err(corrupt)?).execute(&mut **tx).await?;
    sqlx::query(
        "DELETE FROM report_read_ledger_blocks WHERE worker_session_id=? AND report_card_id=?",
    )
    .bind(session)
    .bind(report)
    .execute(&mut **tx)
    .await?;
    for (id, block) in &read.blocks {
        sqlx::query("INSERT INTO report_read_ledger_blocks(worker_session_id,report_card_id,block_id,rev,history_ref) VALUES(?,?,?,?,?)")
            .bind(session).bind(report).bind(id).bind(i64::from(block.rev))
            .bind(serde_json::to_string(&block.history_ref).map_err(corrupt)?).execute(&mut **tx).await?;
    }
    Ok(())
}

/// A consistent read through the repository's serialized transaction boundary.
pub async fn load(
    repo: &dyn crate::db::RepoEventWrite,
    session: &str,
    report: &str,
) -> Result<Option<LastRead>> {
    let session = session.to_owned();
    let report = report.to_owned();
    crate::db::write_in_tx_typed(repo, move |tx| {
        let session = session.clone();
        let report = report.clone();
        Box::pin(async move { load_tx(tx, &session, &report).await })
    })
    .await
}

/// Session retirement is atomic with its caller's state transition.
pub(super) async fn clear_session_tx(
    tx: &mut Transaction<'_, Sqlite>,
    session: &str,
) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM report_read_ledger WHERE worker_session_id=?")
        .bind(session)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
