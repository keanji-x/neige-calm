use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backlink {
    pub src_track_id: String,
    pub src_track_title: String,
    pub src_block_id: String,
    pub dst_block_id: Option<String>,
    pub label: String,
    pub quote: BacklinkQuote,
    pub updated_at: i64,
}

impl Backlink {
    /// The `neige_link_ls` row: no quote, and `updated_at` as RFC 3339 (agent-commands §4).
    fn mcp_row(&self) -> serde_json::Value {
        serde_json::json!({
            "src_track_id": self.src_track_id,
            "src_track_title": self.src_track_title,
            "src_block_id": self.src_block_id,
            "dst_block_id": self.dst_block_id,
            "label": self.label,
            "updated_at": crate::time_format::at(self.updated_at),
        })
    }
}

/// The REST page: at most [`MAX_BACKLINK_ENTRIES`] links in [`MAX_BACKLINK_BYTES`], `truncated`
/// when more exist. `neige_link_ls` pages instead ([`mcp_page`]).
#[derive(Debug, Clone)]
pub struct BacklinkPage {
    pub backlinks: Vec<Backlink>,
    pub truncated: bool,
    pub skipped_sources: usize,
}

#[derive(Debug, Clone, Copy)]
struct WireBudget {
    bytes: usize,
    base_bytes: usize,
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
        let base_bytes = rest_wire_bytes(&page)?;
        Some(Self {
            bytes: base_bytes,
            base_bytes,
            entries: 0,
            max_skipped_sources,
        })
    }

    fn next_length(&self, backlink: &Backlink) -> Option<usize> {
        let single = BacklinkPage {
            backlinks: vec![backlink.clone()],
            truncated: false,
            skipped_sources: self.max_skipped_sources,
        };
        let entry_bytes = rest_wire_bytes(&single)?.checked_sub(self.base_bytes)?;
        self.bytes
            .checked_add(usize::from(self.entries > 0))?
            .checked_add(entry_bytes)
    }

    fn push_if_fits(&mut self, backlink: &Backlink, max_bytes: usize) -> bool {
        let Some(bytes) = self.next_length(backlink) else {
            return false;
        };
        if bytes > max_bytes {
            return false;
        }
        self.bytes = bytes;
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
    let mut backlinks = Vec::new();
    let mut truncated = false;
    let mut wire_budget = None;
    let skipped_sources = scan_backlinks(repo, track_id, None, |sources, backlink, _| {
        let budget = wire_budget.get_or_insert_with(|| WireBudget::new(sources));
        if backlinks.len() == MAX_BACKLINK_ENTRIES
            || !budget
                .as_mut()
                .is_some_and(|budget| budget.push_if_fits(&backlink, max_bytes))
        {
            truncated = true;
            return ControlFlow::Break(());
        }
        backlinks.push(backlink);
        ControlFlow::Continue(())
    })
    .await?;
    Ok(BacklinkPage {
        backlinks,
        truncated,
        skipped_sources,
    })
}

/// The position a `neige_link_ls` cursor resumes after: `<src_track_id>:<ordinal>`, the last row's
/// source track and the link's index among that track's links; `None` for any other text.
pub(crate) fn parse_cursor(cursor: &str) -> Option<(&str, usize)> {
    let (track, ordinal) = cursor.rsplit_once(':')?;
    Some((track, ordinal.parse().ok()?))
}

/// `neige_link_ls`: one page of links to `track_id` after `after` ([`parse_cursor`]), at most
/// `page_rows` rows in `page_bytes`, with the unreadable source reports this page passed over;
/// `None` when `after` names no link of the listing.
pub(crate) async fn mcp_page(
    repo: &dyn RouteRepo,
    track_id: &str,
    after: Option<(&str, usize)>,
    page_rows: usize,
    page_bytes: usize,
) -> Result<Option<serde_json::Value>, CalmError> {
    let mut cursor_found = after.is_none();
    let mut page = crate::mcp_server::tools::paging::Page::with_budget(page_rows, page_bytes);
    let skipped_sources = scan_backlinks(
        repo,
        track_id,
        after.map(|(track, _)| track),
        |_, backlink, ordinal| {
            if let Some((track, last)) = after
                && backlink.src_track_id == track
                && ordinal <= last
            {
                cursor_found |= ordinal == last;
                return ControlFlow::Continue(());
            }
            if !cursor_found {
                return ControlFlow::Break(());
            }
            let key = format!("{}:{ordinal}", backlink.src_track_id);
            if page.push(key, backlink.mcp_row()) {
                ControlFlow::Continue(())
            } else {
                ControlFlow::Break(())
            }
        },
    )
    .await?;
    if !cursor_found {
        return Ok(None);
    }
    let (backlinks, next_cursor) = page.finish(false);
    Ok(Some(serde_json::json!({
        "backlinks": backlinks,
        "next_cursor": next_cursor,
        "skipped_sources": skipped_sources,
    })))
}

/// Offers `visit` every link to `track_id` from its area's reports (the target's own included),
/// ordered by source track id, then document order, with the link's index among its source
/// track's links, until `visit` breaks. Source tracks before `from_track` are not read. `visit`
/// also gets the count of non-target sources. Returns the unreadable source reports skipped; a
/// complete scan from the start whose every non-target source is unreadable is an error.
async fn scan_backlinks(
    repo: &dyn RouteRepo,
    track_id: &str,
    from_track: Option<&str>,
    mut visit: impl FnMut(usize, Backlink, usize) -> ControlFlow<()>,
) -> Result<usize, CalmError> {
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
                "track {track_id} has no track-report card (invariant violation)"
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

    let mut skipped_sources = 0;
    let mut readable_non_target_sources = 0;
    let non_target_sources = report_cards
        .iter()
        .filter(|card| card.id != target_card_id)
        .count();
    let mut complete = true;
    'cards: for card in report_cards
        .into_iter()
        .filter(|card| from_track.is_none_or(|from| card.track_id.as_str() >= from))
    {
        let source_title = tracks.get(card.track_id.as_str()).ok_or_else(|| {
            CalmError::Internal(format!("source track {} vanished mid-read", card.track_id))
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
        let mut ordinal = 0;
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
                    if visit(non_target_sources, backlink, ordinal).is_break() {
                        complete = false;
                        break 'cards;
                    }
                    ordinal += 1;
                }
            }
        }
    }
    if complete
        && from_track.is_none()
        && non_target_sources > 0
        && readable_non_target_sources == 0
    {
        return Err(CalmError::Internal(format!(
            "all {skipped_sources} source reports were unreadable"
        )));
    }
    Ok(skipped_sources)
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

    /// A Markdown link to `track`'s report.
    fn link_to(label: &str, track: impl std::fmt::Display) -> String {
        format!("[{label}](neige://wave/{track})")
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
            v1(format!("{}\n", link_to("target", &target.id))),
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

    /// A `neige://source/…` citation is not a track reference: it never becomes a backlink, whatever it sits next to.
    #[tokio::test]
    async fn source_links_never_produce_backlinks() {
        let repo = fresh_repo().await;
        let area = area(&repo, "one").await;
        let target = track(&repo, area.id.as_str(), "Target").await;
        let source = track(&repo, area.id.as_str(), "Source").await;
        report(&repo, target.id.as_str(), target_payload()).await;
        report(
            &repo,
            source.id.as_str(),
            v1(format!(
                "[cite](neige://source/src_2c9e0a1b#q1) and [cite2](neige://source/{})\n",
                target.id
            )),
        )
        .await;

        let found = backlinks_for_track(&repo as &dyn RouteRepo, target.id.as_str())
            .await
            .unwrap();
        assert!(found.backlinks.is_empty(), "{:?}", found.backlinks);
        let mut scanned = Vec::new();
        assert!(calm_types::report_links::visit_links(
            "[cite](neige://source/src_2c9e0a1b#q1)",
            |link| {
                scanned.push(link);
                true
            }
        ));
        assert!(
            scanned.is_empty(),
            "the track-link scanner sees no source link"
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
            v1(format!("{}\n", link_to("outside", &target.id))),
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
            v1(link_to("healthy", &target.id)),
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
    async fn backlink_byte_cap_bounds_the_rest_envelope() {
        let repo = fresh_repo().await;
        let area = area(&repo, "one").await;
        let target = track(&repo, area.id.as_str(), "Target").await;
        let source = track(&repo, area.id.as_str(), "Source").await;
        report(&repo, target.id.as_str(), target_payload()).await;
        // Control characters expand to six bytes in JSON, so this crosses the real 64 KiB cap
        // with a much smaller CRDT fixture than a quarter-megabyte run of plain ASCII.
        let large_label = "\u{0001}".repeat(32);
        let body = (0..MAX_BACKLINK_ENTRIES)
            .map(|index| link_to(&format!("{index}-{large_label}"), &target.id))
            .collect::<Vec<_>>()
            .join("\n");
        report(&repo, source.id.as_str(), v1(body)).await;

        let found = backlinks_for_track(&repo, target.id.as_str())
            .await
            .unwrap();
        assert!(found.truncated);
        assert!(found.backlinks.len() < MAX_BACKLINK_ENTRIES);
        assert!(
            rest_wire_bytes(&found).unwrap() <= MAX_BACKLINK_BYTES,
            "serialized REST envelope exceeds byte cap"
        );
    }

    #[test]
    fn quote_is_present_on_rest_dto_and_absent_from_the_tool_row() {
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
        assert_eq!(rest["backlinks"][0]["updated_at"], 1, "REST keeps unix ms");
        let row = backlink.mcp_row();
        assert!(row.get("quote").is_none());
        assert_eq!(row["updated_at"], crate::time_format::at(1));

        // A one-byte-short cap rejects; the exact larger length admits the same prefix as full serialization.
        let mut prefix = Vec::new();
        let mut budget = WireBudget::new(7).unwrap();
        for _ in 0..3 {
            let exact_cap = budget.next_length(&backlink).unwrap();
            let mut short = budget;
            assert!(!short.push_if_fits(&backlink, exact_cap - 1));
            assert!(budget.push_if_fits(&backlink, exact_cap));
            prefix.push(backlink.clone());
            let page = BacklinkPage {
                backlinks: prefix.clone(),
                truncated: false,
                skipped_sources: 7,
            };
            assert_eq!(exact_cap, rest_wire_bytes(&page).unwrap());
            let shorter = BacklinkPage {
                backlinks: prefix.clone(),
                truncated: true,
                skipped_sources: 7,
            };
            assert!(rest_wire_bytes(&shorter).unwrap() <= exact_cap);
        }
    }

    /// `neige_link_ls` pages by row count and by bytes, resuming inside one source's links and
    /// across sources, and returns every link exactly once in scan order.
    #[tokio::test]
    async fn tool_pages_return_every_link_once_across_sources() {
        let repo = fresh_repo().await;
        let area = area(&repo, "one").await;
        let target = track(&repo, area.id.as_str(), "Target").await;
        report(&repo, target.id.as_str(), target_payload()).await;
        let mut expected = Vec::new();
        for (name, links) in [("Left", 7), ("Right", 5)] {
            let source = track(&repo, area.id.as_str(), name).await;
            let body = (0..links)
                .map(|index| link_to(&format!("{name}{index}"), &target.id))
                .collect::<Vec<_>>()
                .join("\n");
            report(&repo, source.id.as_str(), v1(body)).await;
            expected
                .extend((0..links).map(|index| (source.id.to_string(), format!("{name}{index}"))));
        }
        expected.sort();
        let row_bytes = serde_json::to_vec(
            &backlinks_for_track(&repo, target.id.as_str())
                .await
                .unwrap()
                .backlinks[0]
                .mcp_row(),
        )
        .unwrap()
        .len();
        for (page_rows, page_bytes) in [(4, usize::MAX), (100, 3 * row_bytes + 3)] {
            let (mut seen, mut cursor, mut pages) = (Vec::new(), None::<String>, 0);
            loop {
                let after = cursor.as_deref().map(|c| parse_cursor(c).unwrap());
                let page = mcp_page(&repo, target.id.as_str(), after, page_rows, page_bytes)
                    .await
                    .unwrap()
                    .expect("a minted cursor names a link");
                pages += 1;
                for row in page["backlinks"].as_array().unwrap() {
                    let track = row["src_track_id"].as_str().unwrap().to_string();
                    seen.push((track, row["label"].as_str().unwrap().to_string()));
                }
                match page["next_cursor"].as_str() {
                    Some(next) => cursor = Some(next.to_string()),
                    None => break,
                }
            }
            assert_eq!(
                seen, expected,
                "{page_rows}/{page_bytes}: every link once, in order"
            );
            if page_rows == 4 {
                assert_eq!(pages, 3, "the row count ends a page");
            } else {
                assert!(pages >= 4, "the byte budget ends a page: {pages}");
            }
        }
        let left = &expected[0].0;
        for foreign in [format!("{left}:7"), "zzzz:0".to_string()] {
            let after = parse_cursor(&foreign);
            let page = mcp_page(&repo, target.id.as_str(), after, 4, usize::MAX).await;
            assert!(page.unwrap().is_none(), "{foreign} names no link");
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
            .map(|index| link_to(&index.to_string(), &target.id))
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
