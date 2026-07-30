//! Lossless markdown slicing and best-effort report-block identity reuse.

use crate::wave_report::ReportBlock;
use serde_json::json;
use std::collections::{HashMap, HashSet};

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
    reassign_ids_with_hints(old_blocks, new_slices, &[])
}

/// [`reassign_ids`] with explicit per-slice id hints (#960 PR2's
/// `write_markdown` marker channel). `hints[i] = Some(id)` pins slice
/// `i` to the old block carrying `id` **before** the LCS/similarity
/// passes run; hinted pairs are excluded from both passes, so markers
/// make otherwise-undecidable inputs (duplicate content) decidable.
///
/// A hint is ignored (slice falls back to normal alignment) when the
/// id does not exist among the old blocks, is duplicated there, or was
/// already claimed by an earlier hint. `hints` may be shorter than
/// `new_slices` (missing entries mean "no hint").
pub fn reassign_ids_with_hints(
    old_blocks: &[ReportBlock],
    new_slices: &[BlockSlice],
    hints: &[Option<String>],
) -> Vec<ReportBlock> {
    // First-occurrence-unique old ids are the only reusable ones (a
    // duplicated id is unattributable — same rule as before hints).
    let mut reusable_ids = HashSet::new();
    let unique: Vec<bool> = old_blocks
        .iter()
        .map(|block| reusable_ids.insert(block.id.as_str()))
        .collect();
    let index_by_id: HashMap<&str, usize> = old_blocks
        .iter()
        .enumerate()
        .filter(|(index, _)| unique[*index])
        .map(|(index, block)| (block.id.as_str(), index))
        .collect();

    // Hint pass: pin slices to their marked old blocks, first-wins.
    let mut assignments: Vec<Option<usize>> = vec![None; new_slices.len()];
    let mut taken = vec![false; old_blocks.len()];
    for (index, hint) in hints.iter().enumerate().take(new_slices.len()) {
        if let Some(id) = hint
            && let Some(&old_index) = index_by_id.get(id.as_str())
            && !taken[old_index]
        {
            assignments[index] = Some(old_index);
            taken[old_index] = true;
        }
    }

    // LCS + similarity over the *unpinned* remainder: pinned old
    // blocks and pinned slices are masked out with `None`.
    let old_text: Vec<Option<&str>> = old_blocks
        .iter()
        .enumerate()
        .map(|(index, block)| {
            (unique[index] && !taken[index])
                .then(|| block_markdown(block))
                .flatten()
        })
        .collect();
    let new_text: Vec<Option<&str>> = new_slices
        .iter()
        .enumerate()
        .map(|(index, slice)| assignments[index].is_none().then_some(slice.raw.as_str()))
        .collect();
    let anchors = lcs_matches(&old_text, &new_text);
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
            &new_text[previous_new..anchor_new],
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
                    rev: if unchanged {
                        old.rev
                    } else {
                        old.rev.saturating_add(1)
                    },
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

fn lcs_matches(old: &[Option<&str>], new: &[Option<&str>]) -> Vec<(usize, usize)> {
    // `None` on either side means "not eligible for matching" (dup /
    // non-prose old, or hint-pinned entries) — two `None`s must never
    // compare equal, hence the explicit `is_some` guard.
    let eq = |old_index: usize, new_index: usize| {
        old[old_index].is_some() && old[old_index] == new[new_index]
    };
    let mut lengths = vec![vec![0usize; new.len() + 1]; old.len() + 1];
    for old_index in (0..old.len()).rev() {
        for new_index in (0..new.len()).rev() {
            lengths[old_index][new_index] = if eq(old_index, new_index) {
                lengths[old_index + 1][new_index + 1] + 1
            } else {
                lengths[old_index + 1][new_index].max(lengths[old_index][new_index + 1])
            };
        }
    }
    let (mut old_index, mut new_index) = (0, 0);
    let mut matches = Vec::new();
    while old_index < old.len() && new_index < new.len() {
        if eq(old_index, new_index) {
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
    new: &[Option<&str>],
    old_offset: usize,
    new_offset: usize,
    assignments: &mut [Option<usize>],
) {
    let mut scores = Vec::new();
    for (old_index, old_text) in old.iter().enumerate() {
        let Some(old_text) = old_text else {
            continue;
        };
        for (new_index, new_text) in new.iter().enumerate() {
            let Some(new_text) = new_text else {
                continue;
            };
            let score = similarity(old_text, new_text);
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

/// A marker-stripped `write_markdown` body: the cleaned flat markdown
/// (guaranteed marker-free), its slices, and the per-slice id hints
/// recovered from the stripped marker lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkedBody {
    pub cleaned: String,
    pub slices: Vec<BlockSlice>,
    pub hints: Vec<Option<String>>,
}

/// Strip every standalone `<!-- neige:b_xxxx -->` marker line out of
/// `body` **unconditionally** (markers must never reach storage), then
/// split the cleaned text and bind each stripped id to the slice the
/// marker preceded. `hints` is index-aligned with `slices`; a slice
/// with no marker gets `None`, extra/trailing/duplicate markers are
/// dropped (first marker per slice wins). Feed the result to
/// [`reassign_ids_with_hints`].
pub fn strip_markers_and_split(body: &str) -> MarkedBody {
    let mut cleaned = String::with_capacity(body.len());
    let mut markers: Vec<(usize, String)> = Vec::new();
    for line in body.split_inclusive('\n') {
        match marker_line_id(line) {
            Some(id) => markers.push((cleaned.len(), id.to_string())),
            None => cleaned.push_str(line),
        }
    }
    let slices = split_body(&cleaned);
    let mut starts = Vec::with_capacity(slices.len());
    let mut offset = 0;
    for slice in &slices {
        starts.push(offset);
        offset += slice.raw.len();
    }
    let mut hints = vec![None; slices.len()];
    for (offset, id) in markers {
        if offset >= cleaned.len() && !cleaned.is_empty() {
            // Trailing marker with nothing after it: no block follows.
            continue;
        }
        // The slice whose byte range contains the position the marker
        // occupied — i.e. the block whose content directly follows it.
        let index = match starts.binary_search(&offset) {
            Ok(index) => index,
            Err(0) => 0,
            Err(insert) => insert - 1,
        };
        if hints[index].is_none() {
            hints[index] = Some(id);
        }
    }
    MarkedBody {
        cleaned,
        slices,
        hints,
    }
}

/// The exact marker line [`strip_markers_and_split`] strips and
/// `calm.report.read { with_markers: true }` injects.
pub fn marker_line(id: &str) -> String {
    format!("<!-- neige:{id} -->\n")
}

/// `Some(id)` iff `line` (one `split_inclusive('\n')` item) is a
/// standalone marker line: optional surrounding whitespace around
/// `<!-- neige:b_hhhh -->` with lowercase-hex `h`, nothing else.
fn marker_line_id(line: &str) -> Option<&str> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    let id = line
        .trim_matches([' ', '\t'])
        .strip_prefix("<!-- neige:")?
        .strip_suffix(" -->")?;
    (id.len() == 6
        && id.starts_with("b_")
        && id[2..]
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')))
    .then_some(id)
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
    fn strip_markers_binds_ids_and_cleans_unconditionally() {
        let body = "<!-- neige:b_00aa -->\n# A\nalpha\n<!-- neige:b_00bb -->\n# B\nbeta\n";
        let marked = strip_markers_and_split(body);
        assert_eq!(marked.cleaned, "# A\nalpha\n# B\nbeta\n");
        assert!(!marked.cleaned.contains("<!-- neige:"));
        assert_eq!(marked.slices.len(), 2);
        assert_eq!(
            marked.hints,
            vec![Some("b_00aa".to_string()), Some("b_00bb".to_string())]
        );

        // Unknown / trailing / duplicate / CRLF / indented markers are
        // still stripped; binding is best-effort first-wins.
        let messy =
            "  <!-- neige:b_0001 -->  \r\n# A\n<!-- neige:b_0002 -->\nmid\n<!-- neige:b_0003 -->\n";
        let marked = strip_markers_and_split(messy);
        assert_eq!(marked.cleaned, "# A\nmid\n");
        assert_eq!(marked.slices.len(), 1);
        assert_eq!(marked.hints, vec![Some("b_0001".to_string())]);

        // Non-marker lookalikes stay in the body.
        for keep in [
            "x <!-- neige:b_0001 -->\n", // not standalone
            "<!-- neige:b_00G1 -->\n",   // non-hex
            "<!-- neige:b_00aaa -->\n",  // wrong length
            "<!-- neige:c_00aa -->\n",   // wrong prefix
            "<!--neige:b_00aa -->\n",    // malformed comment
            "<!-- neige:b_00AA -->\n",   // uppercase hex
        ] {
            let marked = strip_markers_and_split(keep);
            assert_eq!(marked.cleaned, keep, "{keep:?} must survive");
        }
    }

    #[test]
    fn marker_hints_make_duplicate_content_decidable() {
        // Two byte-identical blocks: without markers this input is
        // undecidable (design §3.4); with markers it must be exact.
        let old = vec![
            ReportBlock {
                id: "b_aaaa".to_string(),
                kind: "prose".to_string(),
                rev: 4,
                payload: json!({ "markdown": "# A\nsame\n" }),
            },
            ReportBlock {
                id: "b_bbbb".to_string(),
                kind: "prose".to_string(),
                rev: 7,
                payload: json!({ "markdown": "# A\nsame\n" }),
            },
        ];
        // Swap the two blocks via markers; edit the second one.
        let body = "<!-- neige:b_bbbb -->\n# A\nsame\n<!-- neige:b_aaaa -->\n# A\nsame edited\n";
        let marked = strip_markers_and_split(body);
        let out = reassign_ids_with_hints(&old, &marked.slices, &marked.hints);
        assert_eq!(out[0].id, "b_bbbb");
        assert_eq!(out[0].rev, 7, "identical content: rev holds");
        assert_eq!(out[1].id, "b_aaaa");
        assert_eq!(out[1].rev, 5, "edited content: rev+1");
    }

    #[test]
    fn unhinted_slices_fall_back_to_lcs_alignment() {
        let old = reassign_ids(&[], &split_body("# A\nalpha\n# B\nbeta\n"));
        // Marker only on A; B is edited and must still inherit via LCS
        // gap similarity; X is brand new.
        let body = format!(
            "# X\nnew\n<!-- neige:{} -->\n# A\nalpha\n# B\nbeta edited\n",
            old[0].id
        );
        let marked = strip_markers_and_split(&body);
        let out = reassign_ids_with_hints(&old, &marked.slices, &marked.hints);
        assert_eq!(out.len(), 3);
        assert_ne!(out[0].id, old[0].id);
        assert_ne!(out[0].id, old[1].id);
        assert_eq!(out[1].id, old[0].id, "hinted block keeps its id");
        assert_eq!(
            out[2].id, old[1].id,
            "unhinted edit falls back to alignment"
        );
        assert_eq!(out[2].rev, old[1].rev + 1);
    }

    #[test]
    fn hint_to_unknown_or_duplicate_id_is_ignored() {
        let old = reassign_ids(&[], &split_body("# A\nalpha\n"));
        let body = "<!-- neige:b_dead -->\n# A\nalpha\n";
        let marked = strip_markers_and_split(body);
        assert_eq!(marked.hints, vec![Some("b_dead".to_string())]);
        let out = reassign_ids_with_hints(&old, &marked.slices, &marked.hints);
        // Unknown hint id: LCS still reuses the old id (exact match).
        assert_eq!(out[0].id, old[0].id);
        assert_eq!(out[0].rev, old[0].rev);
    }

    #[test]
    fn duplicate_hint_ids_only_first_slice_claims_the_block() {
        // Two slices hinting the same id: first-wins, the second falls
        // back to normal alignment / a fresh mint — output ids stay
        // unique (fix for #960 PR2 review failure-major 3).
        let old = reassign_ids(&[], &split_body("# A\nalpha\n"));
        let hints = vec![Some(old[0].id.clone()), Some(old[0].id.clone())];
        let slices = split_body("# A\nalpha\n# Z\nzeta\n");
        let out = reassign_ids_with_hints(&old, &slices, &hints);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, old[0].id, "first hint claims the block");
        assert_ne!(out[1].id, old[0].id, "duplicate hint is ignored");
        assert_eq!(
            out.iter()
                .map(|block| block.id.as_str())
                .collect::<HashSet<_>>()
                .len(),
            out.len(),
            "output ids are unique"
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
