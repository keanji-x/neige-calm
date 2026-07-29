//! Lossless markdown slicing and best-effort report-block identity reuse.

use crate::wave_report::ReportBlock;
use serde_json::json;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockSlice {
    pub raw: String,
}

/// Split at line-start ATX H1/H2 headings, except while inside a fenced
/// code block. Every input byte belongs to exactly one returned slice.
pub fn split_body(body: &str) -> Vec<BlockSlice> {
    let mut starts = Vec::new();
    let mut offset = 0;
    let mut fence: Option<(u8, usize)> = None;

    for line_with_ending in body.split_inclusive('\n') {
        let line = line_with_ending
            .strip_suffix('\n')
            .unwrap_or(line_with_ending);
        let line = line.strip_suffix('\r').unwrap_or(line);

        if let Some((marker, minimum)) = fence {
            let fence_line = strip_fence_indent(line);
            if fence_run(fence_line, marker)
                .is_some_and(|length| length >= minimum && fence_tail_is_blank(fence_line, length))
            {
                fence = None;
            }
        } else if let Some((marker, length)) = opening_fence(line) {
            fence = Some((marker, length));
        } else if is_h1_or_h2(line) {
            starts.push(offset);
        }
        offset += line_with_ending.len();
    }

    if body.is_empty() {
        return vec![BlockSlice { raw: String::new() }];
    }

    if starts.first() != Some(&0) {
        starts.insert(0, 0);
    }
    starts
        .iter()
        .enumerate()
        .map(|(index, start)| BlockSlice {
            raw: body[*start..starts.get(index + 1).copied().unwrap_or(body.len())].to_string(),
        })
        .collect()
}

pub fn flatten(blocks: &[BlockSlice]) -> String {
    blocks.iter().map(|block| block.raw.as_str()).collect()
}

/// Reuse ids through exact LCS anchors, then similarity-align edited slices
/// within each unmatched gap. Remaining slices receive a new `b_ffff` id.
///
/// Rev semantics (#960 PR2):
///   * matched block, byte-identical content → `rev` unchanged;
///   * matched block, content changed → `rev = old.rev + 1`;
///   * unmatched (brand-new) slice → `rev = 1`.
///
/// Matched blocks keep their old `kind`. A matched non-prose block also
/// keeps its old `payload` verbatim — its payload is not `{ markdown }`
/// and must not be clobbered by the prose slice text.
// TODO(#960 PR3): non-prose blocks should be compared against their
// deterministic flat (fenced) representation instead of an optional
// `payload.markdown`; today only prose slices reach this function.
pub fn reassign_ids(old_blocks: &[ReportBlock], new_slices: &[BlockSlice]) -> Vec<ReportBlock> {
    let mut reusable_ids = HashSet::new();
    let old_text: Vec<Option<&str>> = old_blocks
        .iter()
        .map(|block| {
            reusable_ids
                .insert(block.id.as_str())
                .then(|| block_markdown(block))
                .flatten()
        })
        .collect();
    let anchors = lcs_matches(&old_text, new_slices);
    let mut assignments = vec![None; new_slices.len()];
    for &(old, new) in &anchors {
        assignments[new] = Some(old);
    }

    let mut previous_old = 0;
    let mut previous_new = 0;
    for &(anchor_old, anchor_new) in anchors
        .iter()
        .chain(std::iter::once(&(old_blocks.len(), new_slices.len())))
    {
        similarity_matches(
            &old_text[previous_old..anchor_old],
            &new_slices[previous_new..anchor_new],
            previous_old,
            previous_new,
            &mut assignments,
        );
        previous_old = anchor_old.saturating_add(1);
        previous_new = anchor_new.saturating_add(1);
    }

    let mut used: HashSet<String> = old_blocks.iter().map(|block| block.id.clone()).collect();
    new_slices
        .iter()
        .enumerate()
        .map(|(index, slice)| match assignments[index] {
            Some(old_index) => {
                let old = &old_blocks[old_index];
                let unchanged = block_markdown(old) == Some(slice.raw.as_str());
                ReportBlock {
                    id: old.id.clone(),
                    kind: old.kind.clone(),
                    rev: if unchanged { old.rev } else { old.rev + 1 },
                    // A matched non-prose payload is preserved verbatim —
                    // it is not `{ markdown }` and must not be clobbered
                    // by the prose slice text.
                    payload: if old.kind == "prose" {
                        json!({ "markdown": slice.raw })
                    } else {
                        old.payload.clone()
                    },
                }
            }
            None => ReportBlock {
                id: mint_id(&slice.raw, index, &mut used),
                kind: "prose".to_string(),
                rev: 1,
                payload: json!({ "markdown": slice.raw }),
            },
        })
        .collect()
}

fn is_h1_or_h2(line: &str) -> bool {
    line.starts_with("# ") || line.starts_with("## ")
}

fn fence_run(line: &str, marker: u8) -> Option<usize> {
    let length = line.bytes().take_while(|byte| *byte == marker).count();
    (length >= 3).then_some(length)
}

fn opening_fence(line: &str) -> Option<(u8, usize)> {
    let line = strip_fence_indent(line);
    b"`~".iter().copied().find_map(|marker| {
        fence_run(line, marker)
            .filter(|&length| marker != b'`' || !line[length..].contains('`'))
            .map(|length| (marker, length))
    })
}

fn strip_fence_indent(line: &str) -> &str {
    let indent = line
        .bytes()
        .take_while(|byte| *byte == b' ')
        .take(4)
        .count();
    if indent <= 3 { &line[indent..] } else { line }
}

fn fence_tail_is_blank(line: &str, run: usize) -> bool {
    line[run..].bytes().all(|byte| matches!(byte, b' ' | b'\t'))
}

fn block_markdown(block: &ReportBlock) -> Option<&str> {
    block
        .payload
        .get("markdown")
        .and_then(serde_json::Value::as_str)
}

fn lcs_matches(old: &[Option<&str>], new: &[BlockSlice]) -> Vec<(usize, usize)> {
    let mut lengths = vec![vec![0usize; new.len() + 1]; old.len() + 1];
    for old_index in (0..old.len()).rev() {
        for new_index in (0..new.len()).rev() {
            lengths[old_index][new_index] = if old[old_index] == Some(&new[new_index].raw) {
                lengths[old_index + 1][new_index + 1] + 1
            } else {
                lengths[old_index + 1][new_index].max(lengths[old_index][new_index + 1])
            };
        }
    }
    let (mut old_index, mut new_index) = (0, 0);
    let mut matches = Vec::new();
    while old_index < old.len() && new_index < new.len() {
        if old[old_index] == Some(&new[new_index].raw) {
            matches.push((old_index, new_index));
            old_index += 1;
            new_index += 1;
        } else if lengths[old_index + 1][new_index] >= lengths[old_index][new_index + 1] {
            old_index += 1;
        } else {
            new_index += 1;
        }
    }
    matches
}

fn similarity_matches(
    old: &[Option<&str>],
    new: &[BlockSlice],
    old_offset: usize,
    new_offset: usize,
    assignments: &mut [Option<usize>],
) {
    let mut scores = Vec::new();
    for (old_index, old_text) in old.iter().enumerate() {
        let Some(old_text) = old_text else {
            continue;
        };
        for (new_index, new_slice) in new.iter().enumerate() {
            let score = similarity(old_text, &new_slice.raw);
            if score >= 0.5 {
                scores.push((score, old_index, new_index));
            }
        }
    }
    scores.sort_by(|left, right| {
        right
            .0
            .total_cmp(&left.0)
            .then_with(|| left.1.abs_diff(left.2).cmp(&right.1.abs_diff(right.2)))
    });
    let mut old_used = vec![false; old.len()];
    let mut new_used = vec![false; new.len()];
    for (_, old_index, new_index) in scores {
        if !old_used[old_index] && !new_used[new_index] {
            assignments[new_offset + new_index] = Some(old_offset + old_index);
            old_used[old_index] = true;
            new_used[new_index] = true;
        }
    }
}

fn similarity(left: &str, right: &str) -> f64 {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let maximum = left.len().max(right.len());
    if maximum == 0 {
        return 1.0;
    }
    1.0 - levenshtein(&left, &right) as f64 / maximum as f64
}

fn levenshtein(left: &[char], right: &[char]) -> usize {
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (left_index, left_char) in left.iter().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_char) in right.iter().enumerate() {
            current[right_index + 1] = if left_char == right_char {
                previous[right_index]
            } else {
                1 + previous[right_index]
                    .min(previous[right_index + 1])
                    .min(current[right_index])
            };
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

/// Mint a deterministic `b_xxxx` block id from the slice content +
/// position, probing until it misses every id in `used` (which the
/// caller must pre-seed with all live ids and which this function
/// extends with the returned id). Public so the CRDT layer
/// (`calm-server::wave_report_doc`) mints ids in the same style.
pub fn mint_id(raw: &str, index: usize, used: &mut HashSet<String>) -> String {
    let mut hash = 0x811c9dc5u32;
    for byte in raw.bytes().chain(index.to_le_bytes()) {
        hash = (hash ^ u32::from(byte)).wrapping_mul(0x01000193);
    }
    for candidate in 0..=u16::MAX {
        let id = format!("b_{:04x}", (hash as u16).wrapping_add(candidate));
        if used.insert(id.clone()) {
            return id;
        }
    }
    unreachable!("a report cannot contain more than 65,536 distinct block ids")
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn split_and_flatten_is_byte_exact(body in any::<String>()) {
            prop_assert_eq!(flatten(&split_body(&body)), body);
        }

        #[test]
        fn markdown_state_machine_is_byte_exact(
            fragments in prop::collection::vec(
                prop::sample::select(vec![
                    "# A", "## B", "```", "~~~", "   ```", "    ```", "```x`y",
                    "text", "\n", "\r\n", "", "    indented", "\t", "\u{00a0}",
                ]),
                0..40,
            ),
        ) {
            let body = fragments.concat();
            prop_assert_eq!(flatten(&split_body(&body)), body);
        }
    }

    #[test]
    fn tricky_markdown_stays_lossless_and_only_real_headings_split() {
        let body = "preamble\r\n\r\n# A\r\n~~~md\r\n# fenced\r\n~~~\r\n    # indented\r\n> # quoted\r\n## B";
        let blocks = split_body(body);
        assert_eq!(flatten(&blocks), body);
        assert_eq!(blocks.len(), 3);
        assert!(blocks[1].raw.contains("# fenced"));
        assert!(blocks[1].raw.contains("# indented"));
        assert!(blocks[1].raw.contains("# quoted"));
    }

    #[test]
    fn empty_and_heading_only_documents_are_lossless() {
        for body in ["", "# A", "# A\n", "\r\n\r\n", "# A\r\n\r\n## B"] {
            assert_eq!(flatten(&split_body(body)), body);
        }
    }

    #[test]
    fn commonmark_fence_edges_split_only_real_headings() {
        let cases = [
            ("```\r\n# code\r\n```\r\n# real", 2),
            ("```\n# code\n```\n# real\n", 2),
            ("   ```rust\n# code\n   ```\n# real\n", 2),
            ("~~~rust\n# code\n~~~~~~\n# real\n", 2),
            ("   ~~~\n# code\n   ~~~\n# real\n", 2),
            ("    ```\n# real\n", 2),
            ("```foo`bar\n# A\n```\n# B\n", 2),
            ("```\n```\u{00a0}\n# still-code\n", 1),
            ("```\n# code\n# still-code", 1),
        ];

        for (body, expected_blocks) in cases {
            let blocks = split_body(body);
            assert_eq!(flatten(&blocks), body, "{body:?}");
            assert_eq!(blocks.len(), expected_blocks, "{body:?}");
        }

        assert!(
            split_body("```foo`bar\n# A\n```\n# B\n")[1]
                .raw
                .starts_with("# A\n")
        );
    }

    #[test]
    fn edited_block_inherits_id_after_an_insertion() {
        let old_slices = split_body("# A\nalpha\n# B\nbeta\n");
        let old = reassign_ids(&[], &old_slices);
        let new_slices = split_body("# X\nnew\n# A\nalpha edited\n# B\nbeta\n");
        let new = reassign_ids(&old, &new_slices);
        assert_eq!(new[1].id, old[0].id);
        assert_eq!(new[2].id, old[1].id);
    }

    #[test]
    fn rev_increments_on_change_and_holds_on_identical_content() {
        let first = reassign_ids(&[], &split_body("# A\nalpha\n# B\nbeta\n"));
        assert!(
            first.iter().all(|block| block.rev == 1),
            "new blocks: rev=1"
        );

        // Byte-identical rewrite: ids and revs both stay put.
        let same = reassign_ids(&first, &split_body("# A\nalpha\n# B\nbeta\n"));
        for (before, after) in first.iter().zip(&same) {
            assert_eq!(after.id, before.id);
            assert_eq!(after.rev, before.rev);
        }

        // Editing one block bumps only that block's rev.
        let edited = reassign_ids(&same, &split_body("# A\nalpha edited\n# B\nbeta\n"));
        assert_eq!(edited[0].id, first[0].id);
        assert_eq!(edited[0].rev, 2, "edited block: rev+1");
        assert_eq!(edited[1].id, first[1].id);
        assert_eq!(edited[1].rev, 1, "untouched block: rev unchanged");

        // A brand-new block starts at rev=1; survivors keep theirs.
        let grown = reassign_ids(
            &edited,
            &split_body("# A\nalpha edited\n# B\nbeta\n# C\nnew\n"),
        );
        assert_eq!(grown[0].rev, 2);
        assert_eq!(grown[1].rev, 1);
        assert_eq!(grown[2].rev, 1);
        assert!(grown[2].id != grown[0].id && grown[2].id != grown[1].id);
    }

    #[test]
    fn matched_non_prose_block_keeps_kind_payload_and_rev() {
        let payload = json!({ "symbol": "0700.HK", "markdown": "# Chart\nflat repr\n" });
        let old = vec![ReportBlock {
            id: "b_ch01".to_string(),
            kind: "chart.candles".to_string(),
            rev: 3,
            payload: payload.clone(),
        }];
        let reassigned = reassign_ids(&old, &split_body("# Chart\nflat repr\n"));
        assert_eq!(reassigned.len(), 1);
        assert_eq!(reassigned[0].id, "b_ch01");
        assert_eq!(reassigned[0].kind, "chart.candles");
        assert_eq!(reassigned[0].rev, 3, "identical content: rev unchanged");
        assert_eq!(
            reassigned[0].payload, payload,
            "non-prose payload preserved verbatim"
        );
    }

    #[test]
    fn duplicate_ids_and_invalid_payloads_are_not_reused() {
        let old = vec![
            ReportBlock {
                id: "dup".to_string(),
                kind: "prose".to_string(),
                rev: 1,
                payload: json!({ "markdown": "# A\n" }),
            },
            ReportBlock {
                id: "dup".to_string(),
                kind: "prose".to_string(),
                rev: 1,
                payload: json!({ "markdown": "# B\n" }),
            },
            ReportBlock {
                id: "broken".to_string(),
                kind: "prose".to_string(),
                rev: 1,
                payload: json!({}),
            },
        ];
        let reassigned = reassign_ids(&old, &split_body("# A\n# B\n"));

        assert_eq!(reassigned[0].id, "dup");
        assert_ne!(reassigned[1].id, "dup");
        assert_ne!(reassigned[1].id, "broken");
        assert_eq!(
            reassigned
                .iter()
                .map(|block| block.id.as_str())
                .collect::<HashSet<_>>()
                .len(),
            reassigned.len()
        );
    }
}
