//! Report-owned knowledge rules. Storage lives in calm-truth; no process-local authority.
//! The pre-existing same-id/same-rev ABA gap is tracked separately (#3185).
use crate::report_sections::sections;
use calm_types::report_blocks::in_sections;
pub use calm_types::report_knowledge::{HistoryRef, HistoryUnavailable, LastRead, ReadBlock};
use calm_types::track_report::ReportBlock;
use std::collections::HashMap;

pub fn record_read(
    old: Option<LastRead>,
    doc_rev: u64,
    blocks: &[ReportBlock],
    rendered: &[String],
) -> LastRead {
    let mut read = old.unwrap_or_else(|| LastRead {
        doc_rev,
        whole: false,
        blocks: HashMap::new(),
        sections: HashMap::new(),
    });
    let seen = |block: &ReportBlock| rendered.contains(&block.id);
    let all_seen = crate::track_report_read::listed_blocks(blocks).all(seen);
    if all_seen {
        read.blocks.retain(|id, rev| {
            blocks
                .iter()
                .any(|b| &b.id == id && b.rev == rev.rev && !in_sections(&b.kind))
        });
        read.sections.clear();
    }
    read.whole = all_seen || (read.whole && read.doc_rev == doc_rev);
    read.doc_rev = doc_rev;
    for block in blocks.iter().filter(|b| seen(b)) {
        read.blocks.insert(
            block.id.clone(),
            ReadBlock {
                rev: block.rev,
                history_ref: HistoryRef::Unavailable(HistoryUnavailable::NotLocated),
            },
        );
    }
    read.sections.extend(certified_sections(blocks, seen));
    read
}

pub fn anchor(read: Option<&LastRead>, current_rev: u64) -> serde_json::Value {
    read.map_or(serde_json::Value::Null, |r| {
        serde_json::json!({
            "doc_rev": r.doc_rev, "whole": r.whole && r.doc_rev == current_rev,
            "known_blocks": r.blocks.len(),
        })
    })
}

pub fn anchor_line(anchor: &serde_json::Value) -> String {
    if anchor.is_null() {
        return "anchor: unread · read Report text before writing (including after upgrade)".into();
    }
    format!(
        "anchor: doc_rev {} · whole={} · {} blocks known",
        anchor["doc_rev"],
        if anchor["whole"] == true { "yes" } else { "no" },
        anchor["known_blocks"]
    )
}

/// The shared section rule: a unique title and evidence for every final member.
/// Callers supply both content and (for commits) structural-placement evidence.
fn certified_sections(
    blocks: &[ReportBlock],
    witnessed: impl Fn(&ReportBlock) -> bool,
) -> HashMap<String, Vec<(String, u32)>> {
    let all = sections(blocks);
    all.iter()
        .filter(|section| {
            all.iter()
                .filter(|other| other.title == section.title)
                .count()
                == 1
                && section
                    .member_blocks(blocks)
                    .iter()
                    .all(|block| witnessed(block))
        })
        .map(|section| {
            (
                section.title.to_string(),
                section
                    .member_blocks(blocks)
                    .iter()
                    .map(|block| (block.id.clone(), block.rev))
                    .collect(),
            )
        })
        .collect()
}

/// Project only witnessed unchanged content and content authored by this commit. Section evidence
/// must cover every pre-operation structural source contributing to a final section, even when all individual
/// blocks were known: knowing content alone does not certify the section's membership.
pub(crate) fn project_commit(
    old: &LastRead,
    before_rev: u64,
    before: &[ReportBlock],
    after_rev: u64,
    after: &[ReportBlock],
    written: &[String],
    section_output_ids: &[String],
) -> LastRead {
    let anchored = old.doc_rev == before_rev;
    let mut read = LastRead {
        doc_rev: if anchored { after_rev } else { old.doc_rev },
        whole: old.whole && anchored,
        blocks: old.blocks.clone(),
        sections: HashMap::new(),
    };
    // Remove only this operation's deletions. Externally removed ids remain stale evidence.
    for block in before
        .iter()
        .filter(|b| !after.iter().any(|a| a.id == b.id))
    {
        read.blocks.remove(&block.id);
    }
    for block in after.iter().filter(|b| written.contains(&b.id)) {
        read.blocks.insert(
            block.id.clone(),
            ReadBlock {
                rev: block.rev,
                history_ref: HistoryRef::Unavailable(HistoryUnavailable::NotLocated),
            },
        );
    }
    let sources = sections(before);
    let valid_sources = certified_sections(before, |_| true);
    read.sections = certified_sections(after, |block| {
        let known = read.blocks.get(&block.id).map(|b| b.rev) == Some(block.rev);
        let placement = section_output_ids.contains(&block.id)
            || if let Some(source) = sources.iter().find(|source| {
                source
                    .member_blocks(before)
                    .iter()
                    .any(|prior| prior.id == block.id)
            }) {
                valid_sources
                    .get(source.title)
                    .is_some_and(|members| old.sections.get(source.title) == Some(members))
            } else if before.iter().any(|prior| prior.id == block.id) {
                // Outside every H1: content knowledge alone does not witness placement.
                old.whole && anchored
            } else {
                written.contains(&block.id)
            };
        known && placement
    });
    for (title, members) in &old.sections {
        if valid_sources.get(title) != Some(members) {
            read.sections
                .entry(title.clone())
                .or_insert_with(|| members.clone());
        }
    }
    read
}

#[cfg(test)]
mod tests;
