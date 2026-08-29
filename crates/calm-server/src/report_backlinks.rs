use std::collections::{HashMap, HashSet};

use crate::db::RouteRepo;
use crate::error::CalmError;
use crate::wave_report_read::load_report_read_snapshot;
use serde::Serialize;
use utoipa::ToSchema;

pub const MAX_BACKLINK_ENTRIES: usize = 500;
pub const MAX_BACKLINK_BYTES: usize = 64 * 1024;
const QUOTE_BEFORE_CHARS: usize = 34;
const QUOTE_AFTER_CHARS: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct BacklinkQuote {
    pub before: String,
    pub label: String,
    pub after: String,
    pub head_elided: bool,
    pub tail_elided: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Backlink {
    pub src_wave_id: String,
    pub src_wave_title: String,
    pub src_block_id: String,
    pub dst_block_id: Option<String>,
    pub label: String,
    #[serde(skip_serializing)]
    pub quote: BacklinkQuote,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BacklinkPage {
    pub backlinks: Vec<Backlink>,
    pub truncated: bool,
    pub skipped_sources: usize,
}

pub(crate) fn mcp_payload(page: &BacklinkPage) -> serde_json::Value {
    serde_json::json!({
        "backlinks": page.backlinks,
        "truncated": page.truncated,
        "skipped_sources": page.skipped_sources,
    })
}

pub(crate) fn mcp_wire_envelope(page: &BacklinkPage) -> serde_json::Value {
    let payload = mcp_payload(page);
    let text = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    serde_json::json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": payload,
        "isError": false,
    })
}

fn page_fits_wire_caps(page: &BacklinkPage, max_bytes: usize) -> bool {
    let rest_page = crate::routes::waves::WaveBacklinksResponse::from(page.clone());
    let rest_fits = serde_json::to_vec(&rest_page).is_ok_and(|encoded| encoded.len() <= max_bytes);
    let mcp_fits = serde_json::to_vec(&mcp_wire_envelope(page))
        .is_ok_and(|encoded| encoded.len() <= max_bytes);
    rest_fits && mcp_fits
}

fn quote_for_link(plain: &str, link: &calm_types::report_links::ScannedLink) -> BacklinkQuote {
    let prefix = plain
        .get(..link.label_start)
        .expect("scanned link start is a character boundary");
    let before_start = prefix
        .char_indices()
        .rev()
        .nth(QUOTE_BEFORE_CHARS - 1)
        .map_or(0, |(index, _)| index);

    let suffix = plain
        .get(link.label_end..)
        .expect("scanned link end is a character boundary");
    let after_end = suffix
        .char_indices()
        .nth(QUOTE_AFTER_CHARS)
        .map_or(plain.len(), |(index, _)| link.label_end + index);

    let before = plain
        .get(before_start..link.label_start)
        .expect("quote start is a character boundary")
        .trim_matches('\n')
        .to_string();
    let after = plain
        .get(link.label_end..after_end)
        .expect("quote end is a character boundary")
        .trim_matches('\n')
        .to_string();

    BacklinkQuote {
        before,
        label: link.label.clone(),
        after,
        head_elided: before_start > 0,
        tail_elided: after_end < plain.len(),
    }
}

pub async fn backlinks_for_wave(
    repo: &dyn RouteRepo,
    wave_id: &str,
) -> Result<BacklinkPage, CalmError> {
    backlinks_for_wave_with_byte_cap(repo, wave_id, MAX_BACKLINK_BYTES).await
}

async fn backlinks_for_wave_with_byte_cap(
    repo: &dyn RouteRepo,
    wave_id: &str,
    max_bytes: usize,
) -> Result<BacklinkPage, CalmError> {
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
    let target_card_id = target_card.id.clone();
    let target_snapshot = load_report_read_snapshot(repo, target_card.id.as_str()).await?;
    let target_block_ids: HashSet<&str> = target_snapshot
        .blocks
        .iter()
        .map(|block| block.id.as_str())
        .collect();
    let waves: HashMap<_, _> = repo
        .waves_by_cove(target_wave.cove_id.as_str())
        .await?
        .into_iter()
        .map(|wave| (wave.id.as_str().to_owned(), wave.title))
        .collect();

    let mut backlinks = Vec::new();
    let mut truncated = false;
    let mut skipped_sources = 0;
    let mut readable_non_target_sources = 0;
    let non_target_sources = report_cards
        .iter()
        .filter(|card| card.id != target_card_id)
        .count();
    let max_skipped_sources = non_target_sources;
    'cards: for card in report_cards {
        let source_title = waves.get(card.wave_id.as_str()).ok_or_else(|| {
            CalmError::Internal(format!(
                "report_backlinks: source wave {} vanished mid-read",
                card.wave_id
            ))
        })?;
        let snapshot = match load_report_read_snapshot(repo, card.id.as_str()).await {
            Ok(snapshot) => snapshot,
            Err(error) if card.id != target_card_id => {
                tracing::warn!(card_id = %card.id, %error, "skipping unreadable backlink source report");
                skipped_sources += 1;
                continue;
            }
            Err(error) => return Err(error),
        };
        if card.id != target_card_id {
            readable_non_target_sources += 1;
        }
        for block in &snapshot.blocks {
            for markdown in
                calm_types::report_blocks::scannable_text_fields(&block.kind, &block.payload)
            {
                let scan = calm_types::report_links::scan_links(markdown);
                for link in scan.links {
                    if link.dst_wave_id != wave_id {
                        continue;
                    }
                    let quote = quote_for_link(&scan.plain, &link);
                    let backlink = Backlink {
                        src_wave_id: card.wave_id.as_str().to_string(),
                        src_wave_title: source_title.clone(),
                        src_block_id: block.id.clone(),
                        dst_block_id: link
                            .dst_block_id
                            .filter(|id| target_block_ids.contains(id.as_str())),
                        label: link.label.clone(),
                        quote,
                        updated_at: snapshot.updated_at,
                    };
                    if backlinks.len() == MAX_BACKLINK_ENTRIES {
                        truncated = true;
                        break 'cards;
                    }
                    backlinks.push(backlink);
                    let bounded_envelope = BacklinkPage {
                        backlinks: backlinks.clone(),
                        truncated: true,
                        skipped_sources: max_skipped_sources,
                    };
                    if !page_fits_wire_caps(&bounded_envelope, max_bytes) {
                        backlinks.pop();
                        truncated = true;
                        break 'cards;
                    }
                }
            }
        }
    }
    if non_target_sources > 0 && readable_non_target_sources == 0 {
        return Err(CalmError::Internal(format!(
            "report_backlinks: all {skipped_sources} source reports were unreadable"
        )));
    }
    Ok(BacklinkPage {
        backlinks,
        truncated,
        skipped_sources,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card_role_cache::CardRoleCache;
    use crate::db::sqlite::SqlxRepo;
    use crate::db::{RepoSyncDomainRaw, RouteRepo, ServerRepoReadExt};
    use crate::event::{EditAuthor, EventBus};
    use crate::ids::ActorId;
    use crate::model::{NewCove, NewWave, RequestTheme};
    use crate::state::WriteContext;
    use crate::wave_cove_cache::WaveCoveCache;
    use crate::wave_report::{ReportBlock, WaveReportPayload, persist_report};
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
            plugin_scope: None,
            workflow_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap()
    }

    async fn report(repo: &SqlxRepo, wave_id: &str, payload: serde_json::Value) {
        report_as(repo, wave_id, payload, EditAuthor::Kernel).await;
    }

    async fn report_as(
        repo: &SqlxRepo,
        wave_id: &str,
        payload: serde_json::Value,
        author: EditAuthor,
    ) {
        let initial = WaveReportPayload::initial();
        let card = repo
            .card_create(crate::model::NewCard {
                wave_id: wave_id.into(),
                kind: "wave-report".into(),
                sort: Some(-1.0),
                payload: serde_json::to_value(&initial).unwrap(),
                title: Some("Report".into()),
            })
            .await
            .unwrap();
        let wave = repo.wave_get(wave_id).await.unwrap().unwrap();
        let next: WaveReportPayload = serde_json::from_value(payload).unwrap();
        persist_report(
            repo,
            &EventBus::new(),
            &WriteContext::new(CardRoleCache::new(), WaveCoveCache::new()),
            ActorId::Kernel,
            author,
            wave,
            card,
            initial,
            next,
            0,
            None,
            None,
            false,
        )
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
            doc_rev: 0,
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

    fn scan_quote(markdown: &str) -> BacklinkQuote {
        let scan = calm_types::report_links::scan_links(markdown);
        assert_eq!(scan.links.len(), 1);
        quote_for_link(&scan.plain, &scan.links[0])
    }

    #[test]
    fn quote_window_counts_pure_chinese_characters() {
        let before = "甲".repeat(36);
        let after = "乙".repeat(42);
        let quote = scan_quote(&format!("{before}[目标](neige://wave/w1){after}"));

        assert_eq!(quote.before, "甲".repeat(QUOTE_BEFORE_CHARS));
        assert_eq!(quote.label, "目标");
        assert_eq!(quote.after, "乙".repeat(QUOTE_AFTER_CHARS));
        assert!(quote.head_elided);
        assert!(quote.tail_elided);
    }

    #[test]
    fn quote_window_preserves_mixed_chinese_and_english() {
        let quote = scan_quote("甲a乙b [混合](neige://wave/w1) 丙c丁d");

        assert_eq!(quote.before, "甲a乙b ");
        assert_eq!(quote.label, "混合");
        assert_eq!(quote.after, " 丙c丁d");
        assert!(!quote.head_elided);
        assert!(!quote.tail_elided);
    }

    #[test]
    fn quote_window_counts_emoji_as_characters() {
        let before = "😀".repeat(35);
        let after = "🧭".repeat(41);
        let quote = scan_quote(&format!("{before}[方向](neige://wave/w1){after}"));

        assert_eq!(quote.before.chars().count(), QUOTE_BEFORE_CHARS);
        assert_eq!(quote.after.chars().count(), QUOTE_AFTER_CHARS);
        assert!(quote.head_elided);
        assert!(quote.tail_elided);
    }

    #[test]
    fn quote_at_plain_text_start_is_not_head_elided() {
        let quote = scan_quote(&format!(
            "[start](neige://wave/w1){}",
            "x".repeat(QUOTE_AFTER_CHARS + 1)
        ));

        assert_eq!(quote.before, "");
        assert!(!quote.head_elided);
        assert!(quote.tail_elided);
    }

    #[test]
    fn quote_at_markdown_end_is_not_tail_elided() {
        let quote = scan_quote("prefix [end](neige://wave/w1)");

        assert_eq!(quote.after, "");
        assert!(!quote.tail_elided);
    }

    #[test]
    fn quote_trims_block_boundary_newlines_without_changing_elision() {
        let quote = scan_quote("first\n\n[second](neige://wave/w1)\n\nthird");

        assert_eq!(quote.before, "first");
        assert_eq!(quote.after, "third");
        assert!(!quote.head_elided);
        assert!(!quote.tail_elided);
    }

    #[test]
    fn empty_link_label_remains_empty_in_quote() {
        let quote = scan_quote("left [](neige://wave/w1) right");

        assert_eq!(quote.before, "left ");
        assert_eq!(quote.label, "");
        assert_eq!(quote.after, " right");
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
        assert_eq!(found.backlinks.len(), 1);
        assert_eq!(found.backlinks[0].src_wave_id, source.id.as_str());
        assert_eq!(found.backlinks[0].src_wave_title, "Source");
        assert_eq!(found.backlinks[0].label, "target");
        assert_eq!(
            found.backlinks[0].quote,
            BacklinkQuote {
                before: String::new(),
                label: "target".into(),
                after: String::new(),
                head_elided: false,
                tail_elided: false,
            }
        );
    }

    #[tokio::test]
    async fn task_backlinks_scan_declared_text_fields_not_canonical_json() {
        let repo = fresh_repo().await;
        let cove = cove(&repo, "one").await;
        let target = wave(&repo, cove.id.as_str(), "Target").await;
        let source = wave(&repo, cove.id.as_str(), "Source").await;
        report(&repo, target.id.as_str(), target_payload()).await;
        let target_card = repo
            .cards_by_wave(target.id.as_str())
            .await
            .unwrap()
            .into_iter()
            .find(|card| card.kind == "wave-report")
            .unwrap();
        let target_block_id = load_report_read_snapshot(&repo, target_card.id.as_str())
            .await
            .unwrap()
            .blocks[0]
            .id
            .clone();
        let task = json!({
            "key": "linked", "kind": "codex",
            "goal": format!("Read [target](neige://wave/{}#{target_block_id})", target.id),
            "acceptance": "Done", "ready": false, "declared_by": "spec"
        });
        let fence = calm_types::report_blocks::render_data_block("task", &task).unwrap();
        report_as(&repo, source.id.as_str(), v1(fence), EditAuthor::Spec).await;

        let found = backlinks_for_wave(&repo as &dyn RouteRepo, target.id.as_str())
            .await
            .unwrap();
        assert_eq!(found.backlinks.len(), 1);
        assert_eq!(found.backlinks[0].label, "target");
        assert_eq!(
            found.backlinks[0].dst_block_id.as_deref(),
            Some(target_block_id.as_str())
        );
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
        assert!(found.backlinks.is_empty());
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
        assert_eq!(found.backlinks.len(), 1);
        assert_eq!(found.backlinks[0].dst_block_id, None);
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
        sqlx::query(
            "UPDATE cards SET body_crdt = NULL, \
             payload = json_set(json_remove(payload, '$.blocks'), '$.schemaVersion', 1) \
             WHERE wave_id = ?1 AND kind = 'wave-report'",
        )
        .bind(source.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();

        let found = backlinks_for_wave(&repo as &dyn RouteRepo, target.id.as_str())
            .await
            .unwrap();
        assert_eq!(found.backlinks.len(), 1);
        assert!(found.backlinks[0].src_block_id.starts_with("b_"));
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
        assert!(found.backlinks.is_empty());
    }

    #[tokio::test]
    async fn unreadable_source_report_does_not_blind_other_backlinks() {
        let repo = fresh_repo().await;
        let cove = cove(&repo, "one").await;
        let target = wave(&repo, cove.id.as_str(), "Target").await;
        let corrupt = wave(&repo, cove.id.as_str(), "Corrupt").await;
        let healthy = wave(&repo, cove.id.as_str(), "Healthy").await;
        report(&repo, target.id.as_str(), target_payload()).await;
        report(&repo, corrupt.id.as_str(), v1("ignored")).await;
        report(
            &repo,
            healthy.id.as_str(),
            v1(format!("[healthy](neige://wave/{})", target.id)),
        )
        .await;
        sqlx::query(
            "UPDATE cards SET body_crdt = X'00', payload = json_remove(payload, '$.blocks') \
             WHERE wave_id = ?1",
        )
        .bind(corrupt.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();

        let found = backlinks_for_wave(&repo, target.id.as_str()).await.unwrap();
        assert_eq!(found.backlinks.len(), 1);
        assert_eq!(found.backlinks[0].src_wave_id, healthy.id.as_str());
        assert_eq!(found.skipped_sources, 1);
    }

    #[tokio::test]
    async fn every_non_target_source_unreadable_is_an_error() {
        let repo = fresh_repo().await;
        let cove = cove(&repo, "one").await;
        let target = wave(&repo, cove.id.as_str(), "Target").await;
        let corrupt = wave(&repo, cove.id.as_str(), "Corrupt").await;
        report(&repo, target.id.as_str(), target_payload()).await;
        report(&repo, corrupt.id.as_str(), v1("ignored")).await;
        sqlx::query(
            "UPDATE cards SET body_crdt = X'00', payload = json_remove(payload, '$.blocks') \
             WHERE wave_id = ?1",
        )
        .bind(corrupt.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();

        let error = backlinks_for_wave(&repo, target.id.as_str())
            .await
            .expect_err("all unreadable sources must fail");
        assert!(
            matches!(error, CalmError::Internal(message) if message.contains("all 1 source reports were unreadable"))
        );
    }

    #[tokio::test]
    async fn backlink_byte_cap_bounds_rest_and_mcp_wire_envelopes() {
        let repo = fresh_repo().await;
        let cove = cove(&repo, "one").await;
        let target = wave(&repo, cove.id.as_str(), "Target").await;
        let source = wave(&repo, cove.id.as_str(), "Source").await;
        report(&repo, target.id.as_str(), target_payload()).await;
        let large_label = "x".repeat(512);
        let body = (0..MAX_BACKLINK_ENTRIES)
            .map(|index| format!("[{index}-{large_label}](neige://wave/{})", target.id))
            .collect::<Vec<_>>()
            .join("\n");
        report(&repo, source.id.as_str(), v1(body)).await;

        let found = backlinks_for_wave(&repo, target.id.as_str()).await.unwrap();
        assert!(found.truncated);
        assert!(found.backlinks.len() < MAX_BACKLINK_ENTRIES);
        assert!(
            serde_json::to_vec(&crate::routes::waves::WaveBacklinksResponse::from(
                found.clone()
            ))
            .unwrap()
            .len()
                <= MAX_BACKLINK_BYTES,
            "serialized REST envelope exceeds byte cap"
        );
        assert!(
            serde_json::to_vec(&mcp_wire_envelope(&found))
                .unwrap()
                .len()
                <= MAX_BACKLINK_BYTES,
            "serialized MCP response envelope exceeds byte cap"
        );
    }

    #[tokio::test]
    async fn backlink_byte_cap_is_enforced_when_rest_quote_is_the_tighter_envelope() {
        let repo = fresh_repo().await;
        let cove = cove(&repo, "one").await;
        let target = wave(&repo, cove.id.as_str(), "Target").await;
        let source = wave(&repo, cove.id.as_str(), "Source").await;
        report(&repo, target.id.as_str(), target_payload()).await;
        let before = "甲".repeat(QUOTE_BEFORE_CHARS);
        let after = "乙".repeat(QUOTE_AFTER_CHARS);
        let body = (0..MAX_BACKLINK_ENTRIES)
            .map(|index| format!("{before}[{index}](neige://wave/{}){after}", target.id))
            .collect::<Vec<_>>()
            .join("\n");
        report(&repo, source.id.as_str(), v1(body)).await;

        let found = backlinks_for_wave(&repo, target.id.as_str()).await.unwrap();
        let rest_bytes = serde_json::to_vec(&crate::routes::waves::WaveBacklinksResponse::from(
            found.clone(),
        ))
        .unwrap()
        .len();
        let mcp_bytes = serde_json::to_vec(&mcp_wire_envelope(&found))
            .unwrap()
            .len();

        assert!(found.truncated);
        assert!(
            rest_bytes <= MAX_BACKLINK_BYTES,
            "REST envelope is {rest_bytes} bytes"
        );
        assert!(
            mcp_bytes <= MAX_BACKLINK_BYTES,
            "MCP envelope is {mcp_bytes} bytes"
        );
        assert!(
            rest_bytes > mcp_bytes,
            "fixture must make the REST quote payload the tighter envelope"
        );
    }

    #[test]
    fn quote_is_present_on_rest_dto_and_absent_from_mcp_payload() {
        let backlink = Backlink {
            src_wave_id: "source".into(),
            src_wave_title: "Source".into(),
            src_block_id: "b_1234".into(),
            dst_block_id: None,
            label: "target".into(),
            quote: BacklinkQuote {
                before: "before ".into(),
                label: "target".into(),
                after: " after".into(),
                head_elided: false,
                tail_elided: false,
            },
            updated_at: 1,
        };
        let page = BacklinkPage {
            backlinks: vec![backlink],
            truncated: false,
            skipped_sources: 0,
        };

        let rest = serde_json::to_value(crate::routes::waves::WaveBacklinksResponse::from(
            page.clone(),
        ))
        .unwrap();
        assert_eq!(rest["backlinks"][0]["quote"]["before"], "before ");
        assert!(mcp_payload(&page)["backlinks"][0].get("quote").is_none());
    }

    #[tokio::test]
    async fn backlink_entry_cap_returns_exactly_500_entries() {
        let repo = fresh_repo().await;
        let cove = cove(&repo, "one").await;
        let target = wave(&repo, cove.id.as_str(), "Target").await;
        let source = wave(&repo, cove.id.as_str(), "Source").await;
        report(&repo, target.id.as_str(), target_payload()).await;
        let body = (0..=MAX_BACKLINK_ENTRIES)
            .map(|index| format!("[{index}](neige://wave/{})", target.id))
            .collect::<Vec<_>>()
            .join("\n");
        report(&repo, source.id.as_str(), v1(body)).await;

        let found = backlinks_for_wave_with_byte_cap(&repo, target.id.as_str(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(found.backlinks.len(), MAX_BACKLINK_ENTRIES);
        assert!(found.truncated);
    }
}
