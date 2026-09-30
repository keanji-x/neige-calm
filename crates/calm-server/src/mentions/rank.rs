//! Fuzzy matching and ranking of the `@` candidates, in memory over one fresh read of the area.
//!
//! A non-empty query is matched with `nucleo-matcher` (fuzzy: the query's characters in order, gaps
//! allowed; whitespace separates terms that must all match) against the tag, the track title, and the
//! block label. Order within each group, ties broken left to right:
//!
//! * tags: match score, reports carrying it, the newest of those reports' updates, tag;
//! * tracks: match score, report update time (newest first), path;
//! * blocks: match score, the `track` parameter's blocks first, report update time (newest first),
//!   path, document order.
//!
//! An empty query recommends: the most-used tags, the most recently updated reports, and the
//! `track` parameter's blocks in document order. Without a `track`, or when it names no report in the
//! area, the blocks of the most recently updated reports instead: newest report first, document order
//! within a report. Each group keeps [`MAX_PER_GROUP`].

use std::collections::BTreeMap;

use calm_types::ids::TrackId;
use calm_types::mentions::{BlockMention, MentionCandidates, TagMention, TrackMention};
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use super::mention_insert;
use crate::area_reports::ReportOutline;
use crate::mcp_server::tools::report_links::block_heading;

/// Most candidates one group returns.
pub(crate) const MAX_PER_GROUP: usize = 8;

/// Scores text against the query; `None` is no match. An empty query matches everything equally.
struct Scorer {
    pattern: Option<Pattern>,
    matcher: Matcher,
    buf: Vec<char>,
}

impl Scorer {
    fn new(query: &str) -> Self {
        let pattern = (!query.trim().is_empty()).then(|| {
            Pattern::new(
                query,
                CaseMatching::Ignore,
                Normalization::Smart,
                AtomKind::Fuzzy,
            )
        });
        Self {
            pattern,
            matcher: Matcher::new(Config::DEFAULT),
            buf: Vec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.pattern.is_none()
    }

    fn score(&mut self, text: &str) -> Option<u32> {
        match &self.pattern {
            None => Some(0),
            Some(pattern) => pattern.score(Utf32Str::new(text, &mut self.buf), &mut self.matcher),
        }
    }
}

pub(super) fn candidates(
    reports: &[ReportOutline],
    query: &str,
    track: Option<&str>,
) -> MentionCandidates {
    let mut scorer = Scorer::new(query);
    MentionCandidates {
        tags: tags(reports, &mut scorer),
        tracks: tracks(reports, &mut scorer),
        blocks: blocks(reports, &mut scorer, track),
    }
}

fn tags(reports: &[ReportOutline], scorer: &mut Scorer) -> Vec<TagMention> {
    // tag -> (reports carrying it, newest of their updates)
    let mut usage: BTreeMap<&str, (u32, i64)> = BTreeMap::new();
    for report in reports {
        for tag in &report.tags {
            let entry = usage.entry(tag).or_insert((0, i64::MIN));
            entry.0 += 1;
            entry.1 = entry.1.max(report.updated_at);
        }
    }
    let mut ranked: Vec<(u32, &str, u32, i64)> = usage
        .into_iter()
        .filter_map(|(tag, (count, newest))| {
            scorer.score(tag).map(|score| (score, tag, count, newest))
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.2.cmp(&a.2))
            .then(b.3.cmp(&a.3))
            .then(a.1.cmp(b.1))
    });
    ranked
        .into_iter()
        .take(MAX_PER_GROUP)
        .map(|(_, tag, track_count, _)| TagMention {
            label: tag.to_string(),
            track_count,
            insert: mention_insert(&format!("tag:{tag}")),
        })
        .collect()
}

fn tracks(reports: &[ReportOutline], scorer: &mut Scorer) -> Vec<TrackMention> {
    let mut ranked: Vec<(u32, &ReportOutline)> = reports
        .iter()
        .filter_map(|report| scorer.score(&report.title).map(|score| (score, report)))
        .collect();
    ranked.sort_by(|(a_score, a), (b_score, b)| {
        b_score
            .cmp(a_score)
            .then(b.updated_at.cmp(&a.updated_at))
            .then(a.path.cmp(&b.path))
    });
    ranked
        .into_iter()
        .take(MAX_PER_GROUP)
        .map(|(_, report)| TrackMention {
            label: report.title.clone(),
            track_id: TrackId::from(report.track_id.as_str()),
            insert: mention_insert(&report.path),
        })
        .collect()
}

struct RankedBlock<'a> {
    score: u32,
    in_track: bool,
    report: &'a ReportOutline,
    index: usize,
    label: String,
}

fn blocks(
    reports: &[ReportOutline],
    scorer: &mut Scorer,
    track: Option<&str>,
) -> Vec<BlockMention> {
    // An empty query recommends the `track` parameter's blocks alone when it names a report here.
    let track_only = scorer.is_empty()
        && track.is_some_and(|track| reports.iter().any(|report| report.track_id == track));
    let mut ranked = Vec::new();
    for report in reports {
        let in_track = track == Some(report.track_id.as_str());
        if track_only && !in_track {
            continue;
        }
        for (index, block) in report.blocks.iter().enumerate() {
            // A block with no heading (a templated report's hidden contract is HTML comments only)
            // has nothing to show a reader, so it is not offered.
            let label = block_heading(block);
            if label.is_empty() {
                continue;
            }
            if let Some(score) = scorer.score(&label) {
                ranked.push(RankedBlock {
                    score,
                    in_track,
                    report,
                    index,
                    label,
                });
            }
        }
    }
    ranked.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(b.in_track.cmp(&a.in_track))
            .then(b.report.updated_at.cmp(&a.report.updated_at))
            .then(a.report.path.cmp(&b.report.path))
            .then(a.index.cmp(&b.index))
    });
    ranked
        .into_iter()
        .take(MAX_PER_GROUP)
        .map(|ranked| {
            let block_id = ranked.report.blocks[ranked.index].id.clone();
            BlockMention {
                insert: mention_insert(&format!("{}#{block_id}", ranked.report.path)),
                label: ranked.label,
                block_id,
                track_title: ranked.report.title.clone(),
                track_id: TrackId::from(ranked.report.track_id.as_str()),
            }
        })
        .collect()
}
