use crate::db::RouteRepo;
use crate::error::CalmError;
use crate::track_report::TrackReportPayload;
use calm_types::report_blocks::tasks::normalize_legacy_terminal_task_blocks;
use calm_types::track_report::ReportBlock;

/// One self-consistent `calm.report.read` snapshot: `summary`, flat
/// `body` text, and the block index all derived from a SINGLE row
/// read (`card_get_with_body_crdt` fetches payload JSON + CRDT bytes
/// atomically), so a concurrent persist between two awaits can never
/// tear `text` against `blocks` (#960 PR2 review round 2).
pub struct ReportReadSnapshot {
    pub updated_at: i64,
    pub schema_version: u32,
    pub doc_rev: u64,
    pub summary: String,
    pub body: String,
    pub blocks: Vec<ReportBlock>,
    pub task_diagnostics: Vec<crate::db::sqlite::BlockVerdict>,
}

/// Load the read snapshot for the report card.
///
/// Source selection (the CRDT is the source of truth, the JSON cache
/// is best-effort — #960 PR2 review):
///
///   1. `payload.blocks` present (the common case — the persist
///      boundary rewrites the cache on every write): everything comes
///      from the JSON payload of the one fetched row.
///   2. Cache missing but the row holds a migrated (v2) doc:
///      `summary`/`body`/`blocks` are ALL projected from that one doc
///      — ids/revs the write path will actually check, and
///      `flatten(blocks) == body` holds by construction. Never mix
///      `payload.body` with CRDT-derived blocks. (A cache dropped by
///      a pre-#960 binary — design D8 — must not make `read` hand out
///      re-derived ids that diverge from the doc, e.g. after a
///      `blocks.move`.)
///   3. `body_crdt` NULL (pure v1 row) or a legacy not-yet-migrated
///      doc layout: derive the index deterministically (`reassign_ids`
///      over `split_body` of the served body) — byte-identical to
///      what the CRDT seed / lazy migrator will mint on first write
///      with the same (absent) hint, so the ids stay valid targets.
pub async fn load_report_read_snapshot(
    repo: &dyn RouteRepo,
    report_card_id: &str,
    task_budget_default: i64,
) -> Result<ReportReadSnapshot, CalmError> {
    let (card, bytes) = repo
        .card_get_with_body_crdt(report_card_id)
        .await?
        .ok_or_else(|| {
            CalmError::Internal(format!(
                "track_report: report card {report_card_id} vanished mid-read"
            ))
        })?;
    let payload: TrackReportPayload =
        serde_json::from_value(card.payload.clone()).map_err(|e| {
            CalmError::Internal(format!(
                "track_report: malformed payload on card {report_card_id}: {e}"
            ))
        })?;
    let derive = |body: &str| {
        calm_types::report_blocks::reassign_ids(&[], &calm_types::report_blocks::split_body(body))
    };
    let flatten = |blocks: &[ReportBlock]| {
        blocks
            .iter()
            .map(calm_types::report_blocks::flat_text)
            .collect::<String>()
    };
    // Pure legacy row (no CRDT yet): doc_rev is zero and the seed will run `reassign_ids`
    //     over the same body with the same (absent) hints.
    let Some(bytes) = bytes else {
        let blocks = normalize_legacy_terminal_task_blocks(&derive(&payload.body));
        let body = flatten(&blocks);
        let task_diagnostics = repo
            .task_diagnostics(card.track_id.as_str(), &blocks, task_budget_default)
            .await?;
        return Ok(ReportReadSnapshot {
            updated_at: card.updated_at,
            schema_version: TrackReportPayload::SCHEMA_VERSION,
            doc_rev: 0,
            summary: payload.summary,
            body,
            blocks,
            task_diagnostics,
        });
    };
    let mut doc = crate::track_report_doc::ReportDoc::from_bytes(&bytes).map_err(|e| {
        CalmError::Internal(format!(
            "track_report: load CRDT for card {report_card_id}: {e}"
        ))
    })?;
    let doc_rev = doc.doc_rev().map_err(|e| {
        CalmError::Internal(format!(
            "track_report: read doc rev for card {report_card_id}: {e}"
        ))
    })?;
    // Cache may provide the projection, but revision always comes from
    // the CRDT root rather than the JSON mirror.
    if let Some(blocks) = payload.blocks {
        let blocks = normalize_legacy_terminal_task_blocks(&blocks);
        let body = flatten(&blocks);
        let task_diagnostics = repo
            .task_diagnostics(card.track_id.as_str(), &blocks, task_budget_default)
            .await?;
        return Ok(ReportReadSnapshot {
            updated_at: card.updated_at,
            schema_version: TrackReportPayload::SCHEMA_VERSION,
            doc_rev,
            summary: payload.summary,
            body,
            blocks,
            task_diagnostics,
        });
    }
    let internal =
        |e: anyhow::Error| CalmError::Internal(format!("track_report: card {report_card_id}: {e}"));
    // 2 + 3b. Everything from the one doc: summary, body, and (for a
    //     v2 layout) the block snapshot — internally consistent by
    //     construction.
    doc.ensure_blocks_layout(None).map_err(internal)?;
    let (summary, body) = doc.project().map_err(internal)?;
    let blocks = doc.blocks_snapshot().map_err(internal)?;
    let task_diagnostics = repo
        .task_diagnostics(card.track_id.as_str(), &blocks, task_budget_default)
        .await?;
    Ok(ReportReadSnapshot {
        updated_at: card.updated_at,
        schema_version: TrackReportPayload::SCHEMA_VERSION,
        doc_rev,
        summary,
        body,
        blocks,
        task_diagnostics,
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
    use crate::model::{NewArea, NewTrack, RequestTheme};
    use crate::state::WriteContext;
    use crate::track_area_cache::TrackAreaCache;
    use crate::track_report::{TrackReportPayload, persist_report};
    use automerge::{AutoCommit, ObjType, ROOT, transaction::Transactable};
    use serde_json::json;

    async fn fixture(legacy_crdt: bool) -> (SqlxRepo, crate::model::Track, crate::model::Card) {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let area = repo
            .area_create(NewArea {
                name: "read-id-stability".into(),
                color: "#123456".into(),
                sort: None,
            })
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                area_id: area.id,
                title: "report".into(),
                sort: None,
                cwd: "/tmp".into(),
                template_id: None,
                plugin_scope: None,
                template_input: None,
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
             (id, track_id, kind, sort, payload, role, deletable, body_crdt, created_at, updated_at) \
             VALUES ('report', ?1, 'track-report', -1, ?2, 'reportcard', 0, ?3, 1, 1)",
        )
        .bind(track.id.as_str())
        .bind(serde_json::to_string(&payload).unwrap())
        .bind(crdt)
        .execute(repo.pool())
        .await
        .unwrap();
        let card = repo.card_get("report").await.unwrap().unwrap();
        (repo, track, card)
    }

    async fn assert_first_write_preserves_read_ids(legacy_crdt: bool) {
        let (repo, track, card) = fixture(legacy_crdt).await;
        let before =
            load_report_read_snapshot(&repo, "report", crate::scheduler::DEFAULT_TRACK_TASK_BUDGET)
                .await
                .unwrap();
        let before_ids: Vec<_> = before.blocks.iter().map(|block| block.id.clone()).collect();
        let current: TrackReportPayload = serde_json::from_value(card.payload.clone()).unwrap();
        let next = TrackReportPayload::new(current.summary.clone(), current.body.clone());
        let events = EventBus::new();
        let write = WriteContext::new(CardRoleCache::new(), TrackAreaCache::new());
        let persisted = persist_report(
            &repo,
            &events,
            &write,
            ActorId::Kernel,
            EditAuthor::Kernel,
            track,
            card,
            current,
            next,
            0,
            None,
            None,
            false,
        )
        .await
        .unwrap();
        let persisted_payload: TrackReportPayload =
            serde_json::from_value(persisted.payload).unwrap();
        let persisted_ids: Vec<_> = persisted_payload
            .blocks
            .unwrap()
            .into_iter()
            .map(|block| block.id)
            .collect();
        let after =
            load_report_read_snapshot(&repo, "report", crate::scheduler::DEFAULT_TRACK_TASK_BUDGET)
                .await
                .unwrap();
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

    #[tokio::test]
    async fn v3_terminal_goal_reads_as_v4_command_before_first_write() {
        let (repo, _track, _card) = fixture(false).await;
        let block = ReportBlock {
            id: "b_terminal".into(),
            kind: calm_types::report_blocks::KIND_TASK.into(),
            rev: 5,
            payload: json!({
                "key": "compile", "kind": "terminal", "goal": "cargo check",
                "ready": true, "declared_by": "spec"
            }),
        };
        let payload = json!({
            "schemaVersion": 3,
            "docRev": 9,
            "summary": "legacy",
            "body": calm_types::report_blocks::flat_text(&block),
            "blocks": [block]
        });
        sqlx::query("UPDATE cards SET payload=?1 WHERE id='report'")
            .bind(serde_json::to_string(&payload).unwrap())
            .execute(repo.pool())
            .await
            .unwrap();

        let snapshot =
            load_report_read_snapshot(&repo, "report", crate::scheduler::DEFAULT_TRACK_TASK_BUDGET)
                .await
                .unwrap();
        assert_eq!(snapshot.schema_version, 4);
        assert_eq!(
            snapshot.doc_rev, 0,
            "without CRDT the read anchor stays zero"
        );
        assert_eq!(
            snapshot.blocks[0].rev, 1,
            "the pre-CRDT read path derives fresh block revisions as before"
        );
        assert_eq!(snapshot.blocks[0].payload["command"], "cargo check");
        assert!(snapshot.blocks[0].payload.get("goal").is_none());
        assert!(snapshot.body.contains(r#""command": "cargo check""#));
    }
}
