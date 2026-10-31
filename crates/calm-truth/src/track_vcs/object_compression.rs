//! Explicit operator conversion; compression and verification never hold a writer.
use super::object_codec::{self, StoredObject};
use crate::db::sqlite::{SqliteDb, begin_immediate_tx};
use crate::error::{CalmError, Result};
use serde::Serialize;
use sqlx::SqlitePool;
use std::time::Duration;

const PAGE_OBJECTS: i64 = 32;
const PAGE_BYTES: usize = object_codec::MAX_DECODED_BYTES;

#[derive(Debug, Default, Serialize)]
pub struct CompressionPage {
    pub dry_run: bool,
    pub after: String,
    pub complete: bool,
    pub scanned: u64,
    pub converted: u64,
    pub candidates: u64,
    pub skipped_oversized: u64,
    pub changed_since_read: u64,
    pub payload_bytes_saved: u64,
}

/// No migration, writer acquisition or object mutation in preview mode.
pub async fn preview_page(pool: &SqlitePool, after: &str) -> Result<CompressionPage> {
    page(pool, None, after).await
}

/// Restart with an empty cursor to revisit changed rows; replaying a completed page is safe.
pub async fn convert_page(db: &SqliteDb, after: &str) -> Result<CompressionPage> {
    page(db.read_pool(), Some(db), after).await
}

/// Require the matching deployed schema. The maintenance executable never installs it.
pub async fn require_schema(pool: &SqlitePool) -> Result<()> {
    let latest = crate::MIGRATOR
        .iter()
        .last()
        .expect("repository has migrations");
    let row: (i64, Vec<u8>, bool) = sqlx::query_as(
        "SELECT version, checksum, success FROM _sqlx_migrations ORDER BY version DESC LIMIT 1",
    )
    .fetch_one(pool)
    .await?;
    if row.0 != latest.version || row.1 != latest.checksum.as_ref() || !row.2 {
        return Err(CalmError::Internal("object compression requires the matching updated server schema; deploy its readers first".into()));
    }
    sqlx::query("SELECT encoding, decoded_size FROM track_vcs_objects LIMIT 0")
        .execute(pool)
        .await?;
    Ok(())
}

async fn page(pool: &SqlitePool, db: Option<&SqliteDb>, after: &str) -> Result<CompressionPage> {
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT hash, encoding, length(bytes) FROM track_vcs_objects WHERE hash>? ORDER BY hash LIMIT ?",
    ).bind(after).bind(PAGE_OBJECTS).fetch_all(pool).await?;
    let mut report = CompressionPage {
        dry_run: db.is_none(),
        after: after.into(),
        complete: rows.len() < PAGE_OBJECTS as usize,
        ..Default::default()
    };
    let mut read_bytes = 0;
    for (hash, encoding, size) in rows {
        let size = usize::try_from(size)
            .map_err(|_| CalmError::Internal("negative object size".into()))?;
        if encoding == "raw" && size <= object_codec::MAX_DECODED_BYTES {
            if read_bytes + size > PAGE_BYTES {
                report.complete = false;
                break;
            }
            read_bytes += size;
            let object: Option<StoredObject> = sqlx::query_as(
                "SELECT hash, kind, bytes, encoding, decoded_size FROM track_vcs_objects
                 WHERE hash=? AND encoding='raw' AND length(bytes)=?",
            )
            .bind(&hash)
            .bind(size as i64)
            .fetch_optional(pool)
            .await?;
            if let Some(object) = object {
                let (object, encoded) = tokio::task::spawn_blocking(move || -> Result<_> {
                    let encoded = object_codec::encode(&object.bytes)?;
                    object.verify_encoded(&encoded)?;
                    Ok((object, encoded))
                })
                .await
                .map_err(|e| CalmError::Internal(format!("object compression task: {e}")))??;
                if encoded.encoding != "raw" {
                    report.candidates += 1;
                    if let Some(db) = db {
                        #[cfg(test)]
                        super::object_compression_tests::after_prepare().await;
                        let mut tx = begin_immediate_tx(db).await?;
                        let updated = sqlx::query(
                            "UPDATE track_vcs_objects SET bytes=?, encoding=?, decoded_size=?
                             WHERE hash=? AND kind=? AND encoding='raw'
                               AND decoded_size IS NULL AND bytes=?",
                        )
                        .bind(&encoded.bytes)
                        .bind(encoded.encoding)
                        .bind(encoded.decoded_size)
                        .bind(&object.hash)
                        .bind(&object.kind)
                        .bind(&object.bytes)
                        .execute(&mut **tx)
                        .await?
                        .rows_affected();
                        tx.commit().await?;
                        report.converted += updated;
                        report.changed_since_read += 1 - updated;
                        if updated != 0 {
                            report.payload_bytes_saved +=
                                (object.bytes.len() - encoded.bytes.len()) as u64;
                        }
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    } else {
                        report.payload_bytes_saved +=
                            (object.bytes.len() - encoded.bytes.len()) as u64;
                    }
                }
            } else {
                report.changed_since_read += 1;
            }
        } else if encoding == "raw" {
            report.skipped_oversized += 1;
        } else if encoding != "zlib-v1" {
            return Err(CalmError::Internal("unknown VCS object encoding".into()));
        }
        report.after = hash;
        report.scanned += 1;
    }
    Ok(report)
}
