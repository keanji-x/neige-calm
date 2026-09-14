//! #1669 §2.3 — receipt warnings: `neige://source/…` links in the prose
//! blocks a write touched that point at a source or anchor this track does
//! not have. Computed after the persist transaction committed, from the
//! final snapshot plus one `report_sources` read; the write itself is never
//! blocked (§7 default 1).

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use sqlx::SqlitePool;

use calm_types::report_blocks::KIND_PROSE;
use calm_types::report_source_links;
use calm_types::track_report::ReportBlock;

use crate::error::CalmError;

pub const UNRESOLVED_SOURCE_LINK: &str = "unresolved_source_link";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceLinkWarning {
    pub kind: &'static str,
    pub block_id: String,
    pub destination: String,
}

/// Pure half: the links in `written_prose_block_ids` (looked up in
/// `blocks`, prose only) that `index` cannot resolve. One warning per
/// distinct `(block, destination)`, in document order.
pub fn unresolved_links(
    blocks: &[ReportBlock],
    written_prose_block_ids: &[String],
    index: &HashMap<String, Vec<String>>,
) -> Vec<SourceLinkWarning> {
    let written: HashSet<&str> = written_prose_block_ids.iter().map(String::as_str).collect();
    let mut seen = HashSet::new();
    let mut warnings = Vec::new();
    for block in blocks {
        if block.kind != KIND_PROSE || !written.contains(block.id.as_str()) {
            continue;
        }
        let Some(markdown) = block.payload.get("markdown").and_then(|m| m.as_str()) else {
            continue;
        };
        for link in report_source_links::scan(markdown) {
            let resolved = match (index.get(&link.source_id), &link.quote_id) {
                (None, _) => false,
                (Some(_), None) => true,
                (Some(anchors), Some(quote_id)) => anchors.iter().any(|a| a == quote_id),
            };
            if resolved {
                continue;
            }
            if seen.insert((block.id.clone(), link.destination.clone())) {
                warnings.push(SourceLinkWarning {
                    kind: UNRESOLVED_SOURCE_LINK,
                    block_id: block.id.clone(),
                    destination: link.destination,
                });
            }
        }
    }
    warnings
}

/// The receipt's `warnings` for one committed write. No touched prose →
/// no read, empty list.
pub async fn for_write(
    pool: &SqlitePool,
    track_id: &str,
    blocks: &[ReportBlock],
    written_prose_block_ids: &[String],
) -> Result<Vec<SourceLinkWarning>, CalmError> {
    if written_prose_block_ids.is_empty() {
        return Ok(Vec::new());
    }
    let index = super::store::anchor_index(pool, track_id).await?;
    Ok(unresolved_links(blocks, written_prose_block_ids, &index))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn prose(id: &str, markdown: &str) -> ReportBlock {
        ReportBlock {
            id: id.into(),
            kind: KIND_PROSE.into(),
            rev: 1,
            payload: json!({ "markdown": markdown }),
        }
    }

    fn ids(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    fn index(entries: &[(&str, &[&str])]) -> HashMap<String, Vec<String>> {
        entries
            .iter()
            .map(|(source, anchors)| ((*source).to_string(), ids(anchors)))
            .collect()
    }

    #[test]
    fn dangling_source_and_dangling_anchor_are_warned_resolved_is_not() {
        let blocks = vec![
            prose("b_0001", "[a](neige://source/src_00000001#q1)"),
            prose(
                "b_0002",
                "[b](neige://source/src_00000001#q2) [c](neige://source/src_00000002)",
            ),
        ];
        let index = index(&[("src_00000001", &["q1"])]);
        let warnings = unresolved_links(&blocks, &ids(&["b_0001", "b_0002"]), &index);
        assert_eq!(
            warnings,
            vec![
                SourceLinkWarning {
                    kind: UNRESOLVED_SOURCE_LINK,
                    block_id: "b_0002".into(),
                    destination: "neige://source/src_00000001#q2".into(),
                },
                SourceLinkWarning {
                    kind: UNRESOLVED_SOURCE_LINK,
                    block_id: "b_0002".into(),
                    destination: "neige://source/src_00000002".into(),
                },
            ]
        );
    }

    #[test]
    fn only_written_prose_blocks_are_scanned() {
        let blocks = vec![
            prose("b_0001", "[a](neige://source/src_00000009)"),
            prose("b_0002", "[a](neige://source/src_00000009)"),
            ReportBlock {
                id: "b_0003".into(),
                kind: "task".into(),
                rev: 1,
                payload: json!({ "goal": "[a](neige://source/src_00000009)" }),
            },
        ];
        let warnings = unresolved_links(&blocks, &ids(&["b_0002", "b_0003"]), &HashMap::new());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].block_id, "b_0002");
        assert!(unresolved_links(&blocks, &[], &HashMap::new()).is_empty());
    }

    #[test]
    fn duplicate_destinations_in_one_block_warn_once() {
        let blocks = vec![prose(
            "b_0001",
            "[a](neige://source/src_00000009) and [b](neige://source/src_00000009)",
        )];
        let warnings = unresolved_links(&blocks, &ids(&["b_0001"]), &HashMap::new());
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn warning_serializes_with_its_kind() {
        let warning = SourceLinkWarning {
            kind: UNRESOLVED_SOURCE_LINK,
            block_id: "b_0001".into(),
            destination: "neige://source/src_00000009".into(),
        };
        assert_eq!(
            serde_json::to_value(&warning).unwrap(),
            json!({
                "kind": "unresolved_source_link",
                "block_id": "b_0001",
                "destination": "neige://source/src_00000009",
            })
        );
    }
}
