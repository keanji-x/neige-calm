//! Offline database upgrade. Back up SQLite and stop every writer before using on a live file.
use calm_truth::db::sqlite::SqlxRepo;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::args()
        .nth(1)
        .ok_or("usage: upgrade_database sqlite:/path/to/offline-copy.db")?;
    let repo = SqlxRepo::open(&url).await?;
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(repo.pool())
        .await?;
    if integrity != "ok" {
        return Err("SQLite integrity check failed".into());
    }
    let violations = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(repo.pool())
        .await?;
    if !violations.is_empty() {
        return Err("foreign-key check failed".into());
    }
    let old: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE spawn='sub-wave'")
        .fetch_one(repo.pool())
        .await?;
    if old != 0 {
        return Err("legacy child task routes remain".into());
    }
    let reports: Vec<(String, Option<Vec<u8>>)> =
        sqlx::query_as("SELECT payload,body_crdt FROM cards WHERE kind='track-report'")
            .fetch_all(repo.pool())
            .await?;
    let report_count = reports.len();
    for (raw, bytes) in reports {
        let payload: calm_types::track_report::TrackReportPayload = serde_json::from_str(&raw)?;
        if calm_types::subtrack_route::migrate_legacy_body(&payload.body) != payload.body {
            return Err("legacy child route remains in an active report".into());
        }
        if let Some(bytes) = bytes {
            let doc = calm_report::document::ReportDoc::from_bytes(&bytes)?;
            if doc.project()? != (payload.summary, payload.body) {
                return Err("report CRDT and cached body disagree".into());
            }
            if doc.has_blocks_layout()? && payload.blocks.as_ref() != Some(&doc.blocks_snapshot()?)
            {
                return Err("report CRDT and cached blocks disagree".into());
            }
        }
    }
    println!("Verified {report_count} active report projections and canonical child routes.");
    println!(
        "Upgrade complete; SQLite integrity and foreign keys passed; no legacy child task routes."
    );
    Ok(())
}
