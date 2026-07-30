use std::collections::HashSet;

use crate::db::RouteRepo;
use crate::error::CalmError;
use crate::wave_report::load_report_read_snapshot;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backlink {
    pub src_wave_id: String,
    pub src_wave_title: String,
    pub src_block_id: String,
    pub dst_block_id: Option<String>,
    pub label: String,
    pub updated_at: i64,
}

pub async fn backlinks_for_wave(
    repo: &dyn RouteRepo,
    wave_id: &str,
) -> Result<Vec<Backlink>, CalmError> {
    let target_wave = repo
        .wave_get(wave_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {wave_id}")))?;
    let mut report_cards = repo
        .wave_report_cards_by_cove(target_wave.cove_id.as_str())
        .await?;
    report_cards.sort_by(|left, right| left.wave_id.as_str().cmp(right.wave_id.as_str()));
    let target_card = report_cards
        .iter()
        .find(|card| card.wave_id.as_str() == wave_id)
        .ok_or_else(|| {
            CalmError::Internal(format!(
                "report_backlinks: wave {wave_id} has no wave-report card (invariant violation)"
            ))
        })?;
    let target_snapshot = load_report_read_snapshot(repo, target_card.id.as_str()).await?;
    let target_block_ids: HashSet<&str> = target_snapshot
        .blocks
        .iter()
        .map(|block| block.id.as_str())
        .collect();

    let mut backlinks = Vec::new();
    for card in report_cards {
        let source_wave = repo.wave_get(card.wave_id.as_str()).await?.ok_or_else(|| {
            CalmError::Internal(format!(
                "report_backlinks: source wave {} vanished mid-read",
                card.wave_id
            ))
        })?;
        let snapshot = load_report_read_snapshot(repo, card.id.as_str()).await?;
        for block in snapshot
            .blocks
            .iter()
            .filter(|block| block.kind == calm_types::report_blocks::KIND_PROSE)
        {
            let markdown = calm_types::report_blocks::flat_text(block);
            for link in calm_types::report_links::extract_links(&markdown)
                .into_iter()
                .filter(|link| link.dst_wave_id == wave_id)
            {
                backlinks.push(Backlink {
                    src_wave_id: source_wave.id.as_str().to_string(),
                    src_wave_title: source_wave.title.clone(),
                    src_block_id: block.id.clone(),
                    dst_block_id: link
                        .dst_block_id
                        .filter(|id| target_block_ids.contains(id.as_str())),
                    label: link.label,
                    updated_at: snapshot.updated_at,
                });
            }
        }
    }
    Ok(backlinks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::SqlxRepo;
    use crate::db::{RepoSyncDomainRaw, RouteRepo};
    use crate::model::{NewCard, NewCove, NewWave, RequestTheme};
    use crate::wave_report::{ReportBlock, WaveReportPayload};
    use serde_json::json;

    async fn cove(repo: &SqlxRepo, name: &str) -> crate::model::Cove {
        repo.cove_create(NewCove {
            name: name.into(),
            color: "#123456".into(),
            sort: None,
        })
        .await
        .unwrap()
    }

    async fn wave(repo: &SqlxRepo, cove_id: &str, title: &str) -> crate::model::Wave {
        repo.wave_create(NewWave {
            cove_id: cove_id.into(),
            title: title.into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            workflow_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap()
    }

    async fn report(repo: &SqlxRepo, wave_id: &str, payload: serde_json::Value) {
        repo.card_create(NewCard {
            wave_id: wave_id.into(),
            kind: "wave-report".into(),
            sort: None,
            payload,
            title: Some("Report".into()),
        })
        .await
        .unwrap();
    }

    fn v1(body: impl Into<String>) -> serde_json::Value {
        json!({
            "schemaVersion": 1,
            "summary": "",
            "body": body.into()
        })
    }

    fn target_payload() -> serde_json::Value {
        serde_json::to_value(WaveReportPayload {
            schema_version: 2,
            summary: String::new(),
            body: "# Target\n".into(),
            blocks: Some(vec![ReportBlock {
                id: "b_1f3a".into(),
                kind: "prose".into(),
                rev: 1,
                payload: json!({ "markdown": "# Target\n" }),
            }]),
        })
        .unwrap()
    }

    async fn fresh_repo() -> SqlxRepo {
        SqlxRepo::open("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn backlink_found_across_two_waves_in_one_cove() {
        let repo = fresh_repo().await;
        let cove = cove(&repo, "one").await;
        let target = wave(&repo, cove.id.as_str(), "Target").await;
        let source = wave(&repo, cove.id.as_str(), "Source").await;
        report(&repo, target.id.as_str(), target_payload()).await;
        report(
            &repo,
            source.id.as_str(),
            v1(format!("[target](neige://wave/{})\n", target.id)),
        )
        .await;

        let found = backlinks_for_wave(&repo as &dyn RouteRepo, target.id.as_str())
            .await
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].src_wave_id, source.id.as_str());
        assert_eq!(found[0].src_wave_title, "Source");
        assert_eq!(found[0].label, "target");
    }

    #[tokio::test]
    async fn backlink_from_another_cove_is_absent() {
        let repo = fresh_repo().await;
        let target_cove = cove(&repo, "target cove").await;
        let other_cove = cove(&repo, "other cove").await;
        let target = wave(&repo, target_cove.id.as_str(), "Target").await;
        let outside = wave(&repo, other_cove.id.as_str(), "Outside").await;
        report(&repo, target.id.as_str(), target_payload()).await;
        report(
            &repo,
            outside.id.as_str(),
            v1(format!("[outside](neige://wave/{})\n", target.id)),
        )
        .await;

        let found = backlinks_for_wave(&repo as &dyn RouteRepo, target.id.as_str())
            .await
            .unwrap();
        assert!(found.is_empty());
    }

    #[tokio::test]
    async fn missing_destination_block_degrades_without_dropping_backlink() {
        let repo = fresh_repo().await;
        let cove = cove(&repo, "one").await;
        let target = wave(&repo, cove.id.as_str(), "Target").await;
        let source = wave(&repo, cove.id.as_str(), "Source").await;
        report(&repo, target.id.as_str(), target_payload()).await;
        report(
            &repo,
            source.id.as_str(),
            v1(format!("[missing](neige://wave/{}#b_dead)\n", target.id)),
        )
        .await;

        let found = backlinks_for_wave(&repo as &dyn RouteRepo, target.id.as_str())
            .await
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].dst_block_id, None);
    }

    #[tokio::test]
    async fn v1_report_without_blocks_or_crdt_yields_backlinks() {
        let repo = fresh_repo().await;
        let cove = cove(&repo, "one").await;
        let target = wave(&repo, cove.id.as_str(), "Target").await;
        let source = wave(&repo, cove.id.as_str(), "Legacy").await;
        report(&repo, target.id.as_str(), target_payload()).await;
        report(
            &repo,
            source.id.as_str(),
            v1(format!("# Legacy\n\n[old](neige://wave/{})\n", target.id)),
        )
        .await;

        let found = backlinks_for_wave(&repo as &dyn RouteRepo, target.id.as_str())
            .await
            .unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].src_block_id.starts_with("b_"));
    }

    #[tokio::test]
    async fn links_inside_fenced_code_blocks_do_not_yield_backlinks() {
        let repo = fresh_repo().await;
        let cove = cove(&repo, "one").await;
        let target = wave(&repo, cove.id.as_str(), "Target").await;
        let source = wave(&repo, cove.id.as_str(), "Source").await;
        report(&repo, target.id.as_str(), target_payload()).await;
        report(
            &repo,
            source.id.as_str(),
            v1(format!(
                "```markdown\n[hidden](neige://wave/{})\n```\n",
                target.id
            )),
        )
        .await;

        let found = backlinks_for_wave(&repo as &dyn RouteRepo, target.id.as_str())
            .await
            .unwrap();
        assert!(found.is_empty());
    }
}
