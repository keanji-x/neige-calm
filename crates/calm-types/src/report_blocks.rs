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
            if fence_run(line, marker)
                .is_some_and(|length| length >= minimum && fence_tail_is_blank(line, length))
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
pub fn reassign_ids(old_blocks: &[ReportBlock], new_slices: &[BlockSlice]) -> Vec<ReportBlock> {
    let old_text: Vec<&str> = old_blocks.iter().map(block_markdown).collect();
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
        .map(|(index, slice)| {
            let id = assignments[index]
                .map(|old| old_blocks[old].id.clone())
                .unwrap_or_else(|| mint_id(&slice.raw, index, &mut used));
            ReportBlock {
                id,
                kind: "prose".to_string(),
                rev: assignments[index].map_or(1, |old| old_blocks[old].rev),
                payload: json!({ "markdown": slice.raw }),
            }
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
    [b'`', b'~']
        .into_iter()
        .find_map(|marker| fence_run(line, marker).map(|length| (marker, length)))
}

fn fence_tail_is_blank(line: &str, run: usize) -> bool {
    line[run..].trim().is_empty()
}

fn block_markdown(block: &ReportBlock) -> &str {
    block
        .payload
        .get("markdown")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
}

fn lcs_matches(old: &[&str], new: &[BlockSlice]) -> Vec<(usize, usize)> {
    let mut lengths = vec![vec![0usize; new.len() + 1]; old.len() + 1];
    for old_index in (0..old.len()).rev() {
        for new_index in (0..new.len()).rev() {
            lengths[old_index][new_index] = if old[old_index] == new[new_index].raw {
                lengths[old_index + 1][new_index + 1] + 1
            } else {
                lengths[old_index + 1][new_index].max(lengths[old_index][new_index + 1])
            };
        }
    }
    let (mut old_index, mut new_index) = (0, 0);
    let mut matches = Vec::new();
    while old_index < old.len() && new_index < new.len() {
        if old[old_index] == new[new_index].raw {
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
    old: &[&str],
    new: &[BlockSlice],
    old_offset: usize,
    new_offset: usize,
    assignments: &mut [Option<usize>],
) {
    let mut scores = Vec::new();
    for (old_index, old_text) in old.iter().enumerate() {
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

fn mint_id(raw: &str, index: usize, used: &mut HashSet<String>) -> String {
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
    fn edited_block_inherits_id_after_an_insertion() {
        let old_slices = split_body("# A\nalpha\n# B\nbeta\n");
        let old = reassign_ids(&[], &old_slices);
        let new_slices = split_body("# X\nnew\n# A\nalpha edited\n# B\nbeta\n");
        let new = reassign_ids(&old, &new_slices);
        assert_eq!(new[1].id, old[0].id);
        assert_eq!(new[2].id, old[1].id);
    }
}
