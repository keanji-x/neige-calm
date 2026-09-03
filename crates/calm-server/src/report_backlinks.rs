use std::collections::{HashMap, HashSet};

use crate::db::RouteRepo;
use crate::error::CalmError;
use crate::track_report_read::load_report_read_snapshot;
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
    pub src_track_id: String,
    pub src_track_title: String,
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

#[derive(Debug, Clone, Copy)]
struct WireBudget {
    rest_bytes: usize,
    rest_base_bytes: usize,
    mcp_bytes: usize,
    entries: usize,
    max_skipped_sources: usize,
}

fn rest_wire_bytes(page: &BacklinkPage) -> Option<usize> {
    let response = crate::routes::tracks::TrackBacklinksResponse::from(page.clone());
    Some(serde_json::to_vec(&response).ok()?.len())
}

impl WireBudget {
    fn new(max_skipped_sources: usize) -> Option<Self> {
        let page = BacklinkPage {
            backlinks: Vec::new(),
            // `false` is one byte longer than `true` in JSON. Budget the larger final shape so a
            // page that happens to consume the exact cap cannot overflow when no truncation occurs.
            truncated: false,
            skipped_sources: max_skipped_sources,
        };
        let rest_base_bytes = rest_wire_bytes(&page)?;
        Some(Self {
            rest_bytes: rest_base_bytes,
            rest_base_bytes,
            mcp_bytes: serde_json::to_vec(&mcp_wire_envelope(&page)).ok()?.len(),
            entries: 0,
            max_skipped_sources,
        })
    }

    fn next_lengths(&self, backlink: &Backlink) -> Option<(usize, usize)> {
        let single = BacklinkPage {
            backlinks: vec![backlink.clone()],
            truncated: false,
            skipped_sources: self.max_skipped_sources,
        };
        let rest_entry_bytes = rest_wire_bytes(&single)?.checked_sub(self.rest_base_bytes)?;
        let rest_separator = usize::from(self.entries > 0);
        let rest_bytes = self
            .rest_bytes
            .checked_add(rest_separator)?
            .checked_add(rest_entry_bytes)?;

        // mcp_payload first serializes Backlink into serde_json::Value, whose object-key order can
        // differ from direct struct serialization. Build the fragment through that same Value path.
        let mcp_value = serde_json::to_value(backlink).ok()?;
        let mcp_entry = serde_json::to_string(&mcp_value).ok()?;
        let fragment = if self.entries == 0 {
            mcp_entry
        } else {
            format!(",{mcp_entry}")
        };
        // The MCP envelope carries the payload twice: once as structured JSON and once as an
        // escaped JSON string. JSON string escaping is character-local, so encoding just the array
        // fragment gives the exact incremental cost of the text copy (minus its surrounding quotes).
        let escaped_fragment_bytes = serde_json::to_vec(&fragment).ok()?.len().checked_sub(2)?;
        let mcp_bytes = self
            .mcp_bytes
            .checked_add(fragment.len())?
            .checked_add(escaped_fragment_bytes)?;
        Some((rest_bytes, mcp_bytes))
    }

    fn push_if_fits(&mut self, backlink: &Backlink, max_bytes: usize) -> bool {
        let Some((rest_bytes, mcp_bytes)) = self.next_lengths(backlink) else {
            return false;
        };
        if rest_bytes > max_bytes || mcp_bytes > max_bytes {
            return false;
        }
        self.rest_bytes = rest_bytes;
        self.mcp_bytes = mcp_bytes;
        self.entries += 1;
        true
    }
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

pub async fn backlinks_for_track(
    repo: &dyn RouteRepo,
    track_id: &str,
) -> Result<BacklinkPage, CalmError> {
    backlinks_for_track_with_byte_cap(repo, track_id, MAX_BACKLINK_BYTES).await
}

async fn backlinks_for_track_with_byte_cap(
    repo: &dyn RouteRepo,
    track_id: &str,
    max_bytes: usize,
) -> Result<BacklinkPage, CalmError> {
    let target_track = repo
        .track_get(track_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {track_id}")))?;
    let mut report_cards = repo
        .track_report_cards_by_area(target_track.area_id.as_str())
        .await?;
    report_cards.sort_by(|left, right| left.track_id.as_str().cmp(right.track_id.as_str()));
    let target_card = report_cards
        .iter()
        .find(|card| card.track_id.as_str() == track_id)
        .ok_or_else(|| {
            CalmError::Internal(format!(
                "report_backlinks: track {track_id} has no track-report card (invariant violation)"
            ))
        })?;
    let target_card_id = target_card.id.clone();
    let target_snapshot = load_report_read_snapshot(repo, target_card.id.as_str()).await?;
    let target_block_ids: HashSet<&str> = target_snapshot
        .blocks
        .iter()
        .map(|block| block.id.as_str())
        .collect();
    let tracks: HashMap<_, _> = repo
        .tracks_by_area(target_track.area_id.as_str())
        .await?
        .into_iter()
        .map(|track| (track.id.as_str().to_owned(), track.title))
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
    let mut wire_budget = WireBudget::new(max_skipped_sources);
    'cards: for card in report_cards {
        let source_title = tracks.get(card.track_id.as_str()).ok_or_else(|| {
            CalmError::Internal(format!(
                "report_backlinks: source track {} vanished mid-read",
                card.track_id
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
                    if link.dst_track_id != track_id {
                        continue;
                    }
                    let quote = quote_for_link(&scan.plain, &link);
                    let backlink = Backlink {
                        src_track_id: card.track_id.as_str().to_string(),
                        src_track_title: source_title.clone(),
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
                    if !wire_budget
                        .as_mut()
                        .is_some_and(|budget| budget.push_if_fits(&backlink, max_bytes))
                    {
                        truncated = true;
                        break 'cards;
                    }
                    backlinks.push(backlink);
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
    use crate::model::{NewArea, NewTrack, RequestTheme};
    use crate::state::WriteContext;
    use crate::track_area_cache::TrackAreaCache;
    use crate::track_report::{ReportBlock, TrackReportPayload, persist_report};
    use serde_json::json;

    async fn area(repo: &SqlxRepo, name: &str) -> crate::model::Area {
        repo.area_create(NewArea {
            name: name.into(),
            color: "#123456".into(),
            sort: None,
        })
        .await
        .unwrap()
    }

    async fn track(repo: &SqlxRepo, area_id: &str, title: &str) -> crate::model::Track {
        repo.track_create(NewTrack {
            area_id: area_id.into(),
            title: title.into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap()
    }

    async fn report(repo: &SqlxRepo, track_id: &str, payload: serde_json::Value) {
        report_as(repo, track_id, payload, EditAuthor::Kernel).await;
    }

    async fn report_as(
        repo: &SqlxRepo,
        track_id: &str,
        payload: serde_json::Value,
        author: EditAuthor,
    ) {
        let initial = TrackReportPayload::initial();
        let card = repo
            .card_create(crate::model::NewCard {
                track_id: track_id.into(),
                kind: "track-report".into(),
                sort: Some(-1.0),
                payload: serde_json::to_value(&initial).unwrap(),
                title: Some("Report".into()),
            })
            .await
            .unwrap();
        let track = repo.track_get(track_id).await.unwrap().unwrap();
        let next: TrackReportPayload = serde_json::from_value(payload).unwrap();
        persist_report(
            repo,
            &EventBus::new(),
            &WriteContext::new(CardRoleCache::new(), TrackAreaCache::new()),
            ActorId::Kernel,
            author,
            track,
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
        serde_json::to_value(TrackReportPayload {
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
    async fn backlink_found_across_two_tracks_in_one_area() {
        let repo = fresh_repo().await;
        let area = area(&repo, "one").await;
        let target = track(&repo, area.id.as_str(), "Target").await;
        let source = track(&repo, area.id.as_str(), "Source").await;
        report(&repo, target.id.as_str(), target_payload()).await;
        report(
            &repo,
            source.id.as_str(),
            v1(format!("[target](neige://wave/{})\n", target.id)),
        )
        .await;

        let found = backlinks_for_track(&repo as &dyn RouteRepo, target.id.as_str())
            .await
            .unwrap();
        assert_eq!(found.backlinks.len(), 1);
        assert_eq!(found.backlinks[0].src_track_id, source.id.as_str());
        assert_eq!(found.backlinks[0].src_track_title, "Source");
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
        let area = area(&repo, "one").await;
        let target = track(&repo, area.id.as_str(), "Target").await;
        let source = track(&repo, area.id.as_str(), "Source").await;
        report(&repo, target.id.as_str(), target_payload()).await;
        let target_card = repo
            .cards_by_track(target.id.as_str())
            .await
            .unwrap()
            .into_iter()
            .find(|card| card.kind == "track-report")
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
        report_as(&repo, source.id.as_str(), v1(fence), EditAuthor::Planner).await;

        let found = backlinks_for_track(&repo as &dyn RouteRepo, target.id.as_str())
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
    async fn backlink_from_another_area_is_absent() {
        let repo = fresh_repo().await;
        let target_area = area(&repo, "target area").await;
        let other_area = area(&repo, "other area").await;
        let target = track(&repo, target_area.id.as_str(), "Target").await;
        let outside = track(&repo, other_area.id.as_str(), "Outside").await;
        report(&repo, target.id.as_str(), target_payload()).await;
        report(
            &repo,
            outside.id.as_str(),
            v1(format!("[outside](neige://wave/{})\n", target.id)),
        )
        .await;

        let found = backlinks_for_track(&repo as &dyn RouteRepo, target.id.as_str())
            .await
            .unwrap();
        assert!(found.backlinks.is_empty());
    }

    #[tokio::test]
    async fn missing_destination_block_degrades_without_dropping_backlink() {
        let repo = fresh_repo().await;
        let area = area(&repo, "one").await;
        let target = track(&repo, area.id.as_str(), "Target").await;
        let source = track(&repo, area.id.as_str(), "Source").await;
        report(&repo, target.id.as_str(), target_payload()).await;
        report(
            &repo,
            source.id.as_str(),
            v1(format!("[missing](neige://wave/{}#b_dead)\n", target.id)),
        )
        .await;

        let found = backlinks_for_track(&repo as &dyn RouteRepo, target.id.as_str())
            .await
            .unwrap();
        assert_eq!(found.backlinks.len(), 1);
        assert_eq!(found.backlinks[0].dst_block_id, None);
    }

    #[tokio::test]
    async fn v1_report_without_blocks_or_crdt_yields_backlinks() {
        let repo = fresh_repo().await;
        let area = area(&repo, "one").await;
        let target = track(&repo, area.id.as_str(), "Target").await;
        let source = track(&repo, area.id.as_str(), "Legacy").await;
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
             WHERE track_id = ?1 AND kind = 'track-report'",
        )
        .bind(source.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();

        let found = backlinks_for_track(&repo as &dyn RouteRepo, target.id.as_str())
            .await
            .unwrap();
        assert_eq!(found.backlinks.len(), 1);
        assert!(found.backlinks[0].src_block_id.starts_with("b_"));
    }

    #[tokio::test]
    async fn links_inside_fenced_code_blocks_do_not_yield_backlinks() {
        let repo = fresh_repo().await;
        let area = area(&repo, "one").await;
        let target = track(&repo, area.id.as_str(), "Target").await;
        let source = track(&repo, area.id.as_str(), "Source").await;
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

        let found = backlinks_for_track(&repo as &dyn RouteRepo, target.id.as_str())
            .await
            .unwrap();
        assert!(found.backlinks.is_empty());
    }

    #[tokio::test]
    async fn unreadable_source_report_does_not_blind_other_backlinks() {
        let repo = fresh_repo().await;
        let area = area(&repo, "one").await;
        let target = track(&repo, area.id.as_str(), "Target").await;
        let corrupt = track(&repo, area.id.as_str(), "Corrupt").await;
        let healthy = track(&repo, area.id.as_str(), "Healthy").await;
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
             WHERE track_id = ?1",
        )
        .bind(corrupt.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();

        let found = backlinks_for_track(&repo, target.id.as_str())
            .await
            .unwrap();
        assert_eq!(found.backlinks.len(), 1);
        assert_eq!(found.backlinks[0].src_track_id, healthy.id.as_str());
        assert_eq!(found.skipped_sources, 1);
    }

    #[tokio::test]
    async fn every_non_target_source_unreadable_is_an_error() {
        let repo = fresh_repo().await;
        let area = area(&repo, "one").await;
        let target = track(&repo, area.id.as_str(), "Target").await;
        let corrupt = track(&repo, area.id.as_str(), "Corrupt").await;
        report(&repo, target.id.as_str(), target_payload()).await;
        report(&repo, corrupt.id.as_str(), v1("ignored")).await;
        sqlx::query(
            "UPDATE cards SET body_crdt = X'00', payload = json_remove(payload, '$.blocks') \
             WHERE track_id = ?1",
        )
        .bind(corrupt.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();

        let error = backlinks_for_track(&repo, target.id.as_str())
            .await
            .expect_err("all unreadable sources must fail");
        assert!(
            matches!(error, CalmError::Internal(message) if message.contains("all 1 source reports were unreadable"))
        );
    }

    #[tokio::test]
    async fn backlink_byte_cap_bounds_rest_and_mcp_wire_envelopes() {
        let repo = fresh_repo().await;
        let area = area(&repo, "one").await;
        let target = track(&repo, area.id.as_str(), "Target").await;
        let source = track(&repo, area.id.as_str(), "Source").await;
        report(&repo, target.id.as_str(), target_payload()).await;
        // Control characters expand to six bytes in structured JSON and are escaped again in the
        // MCP text copy. This crosses the real 64 KiB cap with a much smaller CRDT fixture than a
        // quarter-megabyte run of plain ASCII while exercising the harder wire-escaping boundary.
        let large_label = "\u{0001}".repeat(32);
        let body = (0..MAX_BACKLINK_ENTRIES)
            .map(|index| format!("[{index}-{large_label}](neige://wave/{})", target.id))
            .collect::<Vec<_>>()
            .join("\n");
        report(&repo, source.id.as_str(), v1(body)).await;

        let found = backlinks_for_track(&repo, target.id.as_str())
            .await
            .unwrap();
        assert!(found.truncated);
        assert!(found.backlinks.len() < MAX_BACKLINK_ENTRIES);
        assert!(
            serde_json::to_vec(&crate::routes::tracks::TrackBacklinksResponse::from(
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
        let area = area(&repo, "one").await;
        let target = track(&repo, area.id.as_str(), "Target").await;
        let source = track(&repo, area.id.as_str(), "Source").await;
        report(&repo, target.id.as_str(), target_payload()).await;
        let before = "甲".repeat(QUOTE_BEFORE_CHARS);
        let after = "乙".repeat(QUOTE_AFTER_CHARS);
        let body = (0..MAX_BACKLINK_ENTRIES)
            .map(|index| format!("{before}[{index}](neige://wave/{}){after}", target.id))
            .collect::<Vec<_>>()
            .join("\n");
        report(&repo, source.id.as_str(), v1(body)).await;

        let found = backlinks_for_track(&repo, target.id.as_str())
            .await
            .unwrap();
        let rest_bytes = serde_json::to_vec(&crate::routes::tracks::TrackBacklinksResponse::from(
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
            src_track_id: "source".into(),
            src_track_title: "Source \"quoted\"".into(),
            src_block_id: "b_1234\\tail".into(),
            dst_block_id: None,
            label: "target\n\u{0001}".into(),
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
            backlinks: vec![backlink.clone()],
            truncated: false,
            skipped_sources: 0,
        };

        let rest = serde_json::to_value(crate::routes::tracks::TrackBacklinksResponse::from(
            page.clone(),
        ))
        .unwrap();
        assert_eq!(rest["backlinks"][0]["quote"]["before"], "before ");
        assert!(mcp_payload(&page)["backlinks"][0].get("quote").is_none());

        // Exactness fixture for incremental accounting, including commas and both MCP payload
        // copies. A one-byte-short cap rejects; the exact larger length admits the same prefix as
        // full serialization.
        let mut prefix = Vec::new();
        let mut budget = WireBudget::new(7).unwrap();
        for _ in 0..3 {
            let (next_rest, next_mcp) = budget.next_lengths(&backlink).unwrap();
            let exact_cap = next_rest.max(next_mcp);
            let mut short = budget;
            assert!(!short.push_if_fits(&backlink, exact_cap - 1));
            assert!(budget.push_if_fits(&backlink, exact_cap));
            prefix.push(backlink.clone());
            let page = BacklinkPage {
                backlinks: prefix.clone(),
                truncated: false,
                skipped_sources: 7,
            };
            assert_eq!(next_rest, rest_wire_bytes(&page).unwrap());
            assert_eq!(
                next_mcp,
                serde_json::to_vec(&mcp_wire_envelope(&page)).unwrap().len()
            );
            let shorter = BacklinkPage {
                backlinks: prefix.clone(),
                truncated: true,
                skipped_sources: 7,
            };
            assert!(rest_wire_bytes(&shorter).unwrap() <= next_rest);
            assert!(
                serde_json::to_vec(&mcp_wire_envelope(&shorter))
                    .unwrap()
                    .len()
                    <= next_mcp
            );
        }
    }

    #[tokio::test]
    async fn backlink_entry_cap_returns_exactly_500_entries() {
        let repo = fresh_repo().await;
        let area = area(&repo, "one").await;
        let target = track(&repo, area.id.as_str(), "Target").await;
        let source = track(&repo, area.id.as_str(), "Source").await;
        report(&repo, target.id.as_str(), target_payload()).await;
        let body = (0..=MAX_BACKLINK_ENTRIES)
            .map(|index| format!("[{index}](neige://wave/{})", target.id))
            .collect::<Vec<_>>()
            .join("\n");
        report(&repo, source.id.as_str(), v1(body)).await;

        let found = backlinks_for_track_with_byte_cap(&repo, target.id.as_str(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(found.backlinks.len(), MAX_BACKLINK_ENTRIES);
        assert!(found.truncated);
    }
}
