use crate::db::RouteRepo;
use crate::error::CalmError;
use crate::wave_report::WaveReportPayload;
use calm_types::wave_report::ReportBlock;

/// One self-consistent report snapshot derived from a single row read.
pub struct ReportReadSnapshot {
    pub updated_at: i64,
    pub schema_version: u32,
    pub summary: String,
    pub body: String,
    pub blocks: Vec<ReportBlock>,
}

/// Load a report snapshot, preferring the JSON block cache, then the
/// authoritative CRDT, and deterministically deriving ids for legacy rows.
pub async fn load_report_read_snapshot(
    repo: &dyn RouteRepo,
    report_card_id: &str,
) -> Result<ReportReadSnapshot, CalmError> {
    let (card, bytes) = repo
        .card_get_with_body_crdt(report_card_id)
        .await?
        .ok_or_else(|| {
            CalmError::Internal(format!(
                "wave_report: report card {report_card_id} vanished mid-read"
            ))
        })?;
    let payload: WaveReportPayload = serde_json::from_value(card.payload.clone()).map_err(|e| {
        CalmError::Internal(format!(
            "wave_report: malformed payload on card {report_card_id}: {e}"
        ))
    })?;
    let derive = |body: &str| {
        calm_types::report_blocks::reassign_ids(&[], &calm_types::report_blocks::split_body(body))
    };
    if let Some(blocks) = payload.blocks {
        return Ok(ReportReadSnapshot {
            updated_at: card.updated_at,
            schema_version: payload.schema_version,
            summary: payload.summary,
            body: payload.body,
            blocks,
        });
    }
    let Some(bytes) = bytes else {
        let blocks = derive(&payload.body);
        return Ok(ReportReadSnapshot {
            updated_at: card.updated_at,
            schema_version: payload.schema_version,
            summary: payload.summary,
            body: payload.body,
            blocks,
        });
    };
    let doc = crate::wave_report_doc::ReportDoc::from_bytes(&bytes).map_err(|e| {
        CalmError::Internal(format!(
            "wave_report: load CRDT for card {report_card_id}: {e}"
        ))
    })?;
    let internal =
        |e: anyhow::Error| CalmError::Internal(format!("wave_report: card {report_card_id}: {e}"));
    let (summary, body) = doc.project().map_err(internal)?;
    let blocks = if doc.has_blocks_layout().map_err(internal)? {
        doc.blocks_snapshot().map_err(internal)?
    } else {
        derive(&body)
    };
    Ok(ReportReadSnapshot {
        updated_at: card.updated_at,
        schema_version: payload.schema_version,
        summary,
        body,
        blocks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card_role_cache::CardRoleCache;
    use crate::db::sqlite::SqlxRepo;
    use crate::db::{RepoSyncDomainRaw, ServerRepoReadExt};
    use crate::event::{EditAuthor, EventBus};
    use crate::ids::ActorId;
    use crate::model::{NewCove, NewWave, RequestTheme};
    use crate::state::WriteContext;
    use crate::wave_cove_cache::WaveCoveCache;
    use crate::wave_report::{WaveReportPayload, persist_report};
    use automerge::{AutoCommit, ObjType, ROOT, transaction::Transactable};
    use serde_json::json;

    async fn fixture(legacy_crdt: bool) -> (SqlxRepo, crate::model::Wave, crate::model::Card) {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let cove = repo
            .cove_create(NewCove {
                name: "read-id-stability".into(),
                color: "#123456".into(),
                sort: None,
            })
            .await
            .unwrap();
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id,
                title: "report".into(),
                sort: None,
                cwd: "/tmp".into(),
                workflow_id: None,
                workflow_input: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let body = "# Goal\n\nalpha\n\n# Result\n\nbeta\n";
        let payload = json!({"schemaVersion": 1, "summary": "s", "body": body});
        let crdt = if legacy_crdt {
            let mut doc = AutoCommit::new();
            let summary = doc.put_object(&ROOT, "summary", ObjType::Text).unwrap();
            doc.update_text(&summary, "s").unwrap();
            let body_obj = doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
            doc.update_text(&body_obj, body).unwrap();
            Some(doc.save())
        } else {
            None
        };
        sqlx::query(
            "INSERT INTO cards \
             (id, wave_id, kind, sort, payload, role, deletable, body_crdt, created_at, updated_at) \
             VALUES ('report', ?1, 'wave-report', -1, ?2, 'reportcard', 0, ?3, 1, 1)",
        )
        .bind(wave.id.as_str())
        .bind(serde_json::to_string(&payload).unwrap())
        .bind(crdt)
        .execute(repo.pool())
        .await
        .unwrap();
        let card = repo.card_get("report").await.unwrap().unwrap();
        (repo, wave, card)
    }

    async fn assert_first_write_preserves_read_ids(legacy_crdt: bool) {
        let (repo, wave, card) = fixture(legacy_crdt).await;
        let before = load_report_read_snapshot(&repo, "report").await.unwrap();
        let before_ids: Vec<_> = before.blocks.iter().map(|block| block.id.clone()).collect();
        let current: WaveReportPayload = serde_json::from_value(card.payload.clone()).unwrap();
        let next = WaveReportPayload::new(current.summary.clone(), current.body.clone());
        let events = EventBus::new();
        let write = WriteContext::new(CardRoleCache::new(), WaveCoveCache::new());
        let persisted = persist_report(
            &repo,
            &events,
            &write,
            ActorId::Kernel,
            EditAuthor::Kernel,
            wave,
            card,
            current,
            next,
            None,
            None,
            false,
        )
        .await
        .unwrap();
        let persisted_payload: WaveReportPayload =
            serde_json::from_value(persisted.payload).unwrap();
        let persisted_ids: Vec<_> = persisted_payload
            .blocks
            .unwrap()
            .into_iter()
            .map(|block| block.id)
            .collect();
        let after = load_report_read_snapshot(&repo, "report").await.unwrap();
        let after_ids: Vec<_> = after.blocks.into_iter().map(|block| block.id).collect();
        assert_eq!(before_ids, persisted_ids);
        assert_eq!(persisted_ids, after_ids);
    }

    #[tokio::test]
    async fn null_crdt_read_ids_survive_first_real_write() {
        assert_first_write_preserves_read_ids(false).await;
    }

    #[tokio::test]
    async fn legacy_crdt_read_ids_survive_first_real_write() {
        assert_first_write_preserves_read_ids(true).await;
    }
}
