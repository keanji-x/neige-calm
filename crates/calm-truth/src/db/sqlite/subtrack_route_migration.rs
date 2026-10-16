//! Forward-only data migration; runs before repository open exposes projections or recovery.
//! Historical events and operation snapshots are evidence and are never rewritten.

use calm_report::document::ReportDoc;
use calm_types::report_blocks::KIND_TASK;
use calm_types::subtrack_route::{LEGACY_CHILD_ROUTE, migrate_legacy_body};
use calm_types::track_report::TrackReportPayload;
use sqlx::SqlitePool;

use crate::error::{CalmError, Result};

const NAME: &str = "subtrack-route-v1";

pub(super) async fn migrate(pool: &SqlitePool) -> Result<()> {
    let mut tx = super::begin_immediate_tx(pool).await?;
    let applied: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM report_data_migrations WHERE name=?1)")
            .bind(NAME)
            .fetch_one(&mut *tx)
            .await?;
    if applied {
        tx.commit().await?;
        return Ok(());
    }

    let reports: Vec<(String, String, Option<Vec<u8>>)> = sqlx::query_as(
        "SELECT id,payload,body_crdt FROM cards WHERE kind='track-report' ORDER BY id",
    )
    .fetch_all(&mut *tx)
    .await?;
    for (id, raw, bytes) in reports {
        let mut cache: serde_json::Value = serde_json::from_str(&raw)?;
        let payload: TrackReportPayload = serde_json::from_value(cache.clone())?;
        let mut doc = match bytes {
            Some(bytes) => ReportDoc::from_bytes(&bytes).map_err(migration_error)?,
            None => ReportDoc::from_payload(&payload),
        };
        let has_legacy = doc
            .blocks_snapshot()
            .map_err(migration_error)?
            .iter()
            .any(|block| {
                block.kind == KIND_TASK
                    && block
                        .payload
                        .get("spawn")
                        .and_then(serde_json::Value::as_str)
                        == Some(LEGACY_CHILD_ROUTE)
            });
        let body = doc.project().map_err(migration_error)?.1;
        let has_legacy = has_legacy || migrate_legacy_body(&body) != body;
        if !has_legacy {
            continue;
        }
        // The report-owned migration preserves block identities/revisions and updates actual
        // Automerge text; SQL never edits only one representation of the document.
        doc.migrate_subtrack_route(payload.blocks.as_deref())
            .map_err(migration_error)?;
        let (summary, body) = doc.project().map_err(migration_error)?;
        let blocks = doc.blocks_snapshot().map_err(migration_error)?;
        cache["summary"] = summary.into();
        cache["body"] = body.into();
        cache["blocks"] = serde_json::to_value(blocks)?;
        cache["docRev"] = doc.doc_rev().map_err(migration_error)?.into();
        sqlx::query("UPDATE cards SET payload=?1,body_crdt=?2 WHERE id=?3")
            .bind(serde_json::to_string(&cache)?)
            .bind(doc.to_bytes())
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    let recipes: Vec<(String, String)> = sqlx::query_as("SELECT id,body FROM track_recipes")
        .fetch_all(&mut *tx)
        .await?;
    for (id, body) in recipes {
        let migrated = migrate_legacy_body(&body);
        if migrated != body {
            sqlx::query("UPDATE track_recipes SET body=?1,revision=revision+1,updated_at=MAX(updated_at+1,?2) WHERE id=?3")
                .bind(migrated).bind(crate::model::now_ms()).bind(id).execute(&mut *tx).await?;
        }
    }
    sqlx::query("UPDATE tasks SET spawn=?1 WHERE spawn=?2")
        .bind(calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE)
        .bind(LEGACY_CHILD_ROUTE)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO report_data_migrations(name,completed_at_ms) VALUES(?1,?2)")
        .bind(NAME)
        .bind(crate::model::now_ms())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

fn migration_error(error: impl std::fmt::Display) -> CalmError {
    CalmError::Internal(format!("subtrack route migration: {error}"))
}

#[cfg(test)]
mod tests;
