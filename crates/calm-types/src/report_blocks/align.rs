//! Best-effort report-block identity reuse across wholesale rewrites
//! (design §3.4) — LCS anchors + gap similarity + explicit marker
//! hints.
//!
//! #960 PR3: alignment is fence-aware. Every slice and every old
//! block is compared through its **canonical flat text** — prose is
//! the markdown verbatim, a non-prose block is its deterministic
//! `neige-block` fence ([`super::fence::render_fence`]) — so a fence
//! re-serialized with different JSON formatting still matches its
//! block exactly, and a byte-preserved fence keeps id, kind, payload
//! and rev.

use super::fence::{NonProseFence, parse_fence, render_fence};
use super::kinds::KIND_PROSE;
use super::{BlockSlice, flat_text};
use crate::track_report::ReportBlock;
use serde_json::json;
use std::collections::{HashMap, HashSet};

/// Reuse ids through exact LCS anchors, then similarity-align edited
/// slices within each unmatched gap. Remaining slices receive a new
/// `b_ffff` id.
///
/// Rev semantics (#960 PR2):
///   * matched block, canonical-identical content → `rev` unchanged;
///   * matched block, content changed → `rev = old.rev + 1`;
///   * unmatched (brand-new) slice → `rev = 1`.
///
/// Kind/payload come from the slice's own nature: a fence slice
/// yields its parsed kind + payload, a prose slice yields
/// `prose`/`{ markdown }`. (Server write paths guarantee a non-prose
/// block is never stomped by a prose slice — the `Replace` guard in
/// calm-server refuses such writes before alignment lands.)
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
    // Per-slice nature: canonical comparison text + parsed fence.
    let fences: Vec<Option<NonProseFence>> = new_slices
        .iter()
        .map(|slice| parse_fence(&slice.raw))
        .collect();
    let canonical: Vec<String> = new_slices
        .iter()
        .zip(&fences)
        .map(|(slice, fence)| match fence {
            Some(fence) => render_fence(&fence.kind, &fence.payload),
            None => slice.raw.clone(),
        })
        .collect();

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
    let old_flat: Vec<Option<String>> = old_blocks
        .iter()
        .enumerate()
        .map(|(index, block)| {
            (unique[index] && !taken[index])
                .then(|| comparable_flat(block))
                .flatten()
        })
        .collect();
    let mut old_text: Vec<Option<&str>> = old_flat.iter().map(Option::as_deref).collect();
    let mut new_text: Vec<Option<&str>> = canonical
        .iter()
        .enumerate()
        .map(|(index, text)| assignments[index].is_none().then_some(text.as_str()))
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
        // Same-kind non-prose pre-pass (#960 PR3 review round 1): a
        // heavily re-parameterized data block (e.g. a chart whose
        // candles were all replaced) falls below the 0.5 similarity
        // threshold on its full fence text and would mint a new id.
        // Within this gap, when the old side has exactly ONE eligible
        // non-prose block of kind K and the new side exactly ONE
        // fence slice of kind K, pair them directly — identity is
        // unambiguous regardless of content distance. More than one
        // of the same kind falls back to the similarity logic.
        same_kind_pairs(
            old_blocks,
            &fences,
            (previous_old, anchor_old),
            (previous_new, anchor_new),
            &mut old_text,
            &mut new_text,
            &mut assignments,
        );
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
        .map(|(index, slice)| {
            let (id, rev) = match assignments[index] {
                Some(old_index) => {
                    let old = &old_blocks[old_index];
                    let unchanged = comparable_flat(old).as_deref() == Some(&canonical[index]);
                    let rev = if unchanged {
                        old.rev
                    } else {
                        old.rev.saturating_add(1)
                    };
                    (old.id.clone(), rev)
                }
                None => (mint_id(&slice.raw, index, &mut used), 1),
            };
            match &fences[index] {
                Some(fence) => ReportBlock {
                    id,
                    kind: fence.kind.clone(),
                    rev,
                    payload: fence.payload.clone(),
                },
                None => ReportBlock {
                    id,
                    kind: KIND_PROSE.to_string(),
                    rev,
                    payload: json!({ "markdown": slice.raw }),
                },
            }
        })
        .collect()
}

/// The comparison text of an existing block: prose markdown verbatim
/// (`None` for a malformed prose payload — never matchable, same rule
/// as before), or the canonical fence for a non-prose block.
fn comparable_flat(block: &ReportBlock) -> Option<String> {
    if block.kind == KIND_PROSE {
        block
            .payload
            .get("markdown")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    } else {
        Some(flat_text(block))
    }
}

/// Pair the unique same-kind non-prose (old block, new fence slice)
/// per kind within one LCS gap. Paired entries are assigned and
/// masked out of both sides so the similarity pass skips them.
fn same_kind_pairs(
    old_blocks: &[ReportBlock],
    fences: &[Option<NonProseFence>],
    (old_start, old_end): (usize, usize),
    (new_start, new_end): (usize, usize),
    old_text: &mut [Option<&str>],
    new_text: &mut [Option<&str>],
    assignments: &mut [Option<usize>],
) {
    let mut old_by_kind: HashMap<&str, Vec<usize>> = HashMap::new();
    for index in old_start..old_end {
        if old_text[index].is_some() && old_blocks[index].kind != KIND_PROSE {
            old_by_kind
                .entry(old_blocks[index].kind.as_str())
                .or_default()
                .push(index);
        }
    }
    let mut new_by_kind: HashMap<&str, Vec<usize>> = HashMap::new();
    for index in new_start..new_end {
        if new_text[index].is_some()
            && let Some(fence) = &fences[index]
        {
            new_by_kind
                .entry(fence.kind.as_str())
                .or_default()
                .push(index);
        }
    }
    for (kind, old_indexes) in old_by_kind {
        if let ([old_index], Some([new_index])) = (
            old_indexes.as_slice(),
            new_by_kind.get(kind).map(Vec::as_slice),
        ) {
            assignments[*new_index] = Some(*old_index);
            old_text[*old_index] = None;
            new_text[*new_index] = None;
        }
    }
}

fn lcs_matches(old: &[Option<&str>], new: &[Option<&str>]) -> Vec<(usize, usize)> {
    // `None` on either side means "not eligible for matching" (dup /
    // malformed old, or hint-pinned entries) — two `None`s must never
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
/// (`calm-server::track_report_doc`) mints ids in the same style.
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
    use super::super::{split_body, strip_markers_and_split};
    use super::*;

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
    fn byte_preserved_fence_keeps_id_kind_payload_and_rev() {
        // #960 PR3: a non-prose block's flat form IS its canonical
        // fence. A wholesale rewrite that carries the fence through
        // verbatim must preserve everything.
        let payload = serde_json::json!({ "src": "/apps/x", "title": "工具" });
        let old = vec![ReportBlock {
            id: "b_ap01".to_string(),
            kind: "app".to_string(),
            rev: 3,
            payload: payload.clone(),
        }];
        let flat = flat_text(&old[0]);
        let body = format!("# Intro\nprose\n{flat}");
        let reassigned = reassign_ids(&old, &split_body(&body));
        assert_eq!(reassigned.len(), 2);
        assert_eq!(reassigned[1].id, "b_ap01");
        assert_eq!(reassigned[1].kind, "app");
        assert_eq!(reassigned[1].rev, 3, "identical fence: rev unchanged");
        assert_eq!(reassigned[1].payload, payload, "payload preserved");
    }

    #[test]
    fn reformatted_fence_json_still_matches_and_changed_params_bump_rev() {
        let old = vec![ReportBlock {
            id: "b_ap01".to_string(),
            kind: "app".to_string(),
            rev: 5,
            payload: serde_json::json!({ "src": "/x", "height": 300 }),
        }];
        // Same payload, hostile formatting (compact, different key
        // order): canonical comparison must still match exactly.
        let body = "```neige-block app\n{\"height\":300,\"src\":\"/x\"}\n```\n";
        let same = reassign_ids(&old, &split_body(body));
        assert_eq!(same.len(), 1);
        assert_eq!(same[0].id, "b_ap01");
        assert_eq!(same[0].rev, 5, "reformatted-but-equal payload: rev holds");
        assert_eq!(
            same[0].payload,
            serde_json::json!({ "src": "/x", "height": 300 })
        );

        // Changed parameter: id reused (similar), rev bumps.
        let body = "```neige-block app\n{\"height\":301,\"src\":\"/x\"}\n```\n";
        let changed = reassign_ids(&old, &split_body(body));
        assert_eq!(changed[0].id, "b_ap01");
        assert_eq!(changed[0].rev, 6, "changed payload: rev+1");
        assert_eq!(
            changed[0].payload,
            serde_json::json!({ "src": "/x", "height": 301 })
        );
    }

    #[test]
    fn fence_block_alignment_survives_insertions() {
        // Non-prose blocks participate in LCS like any other slice.
        let chart = ReportBlock {
            id: "b_ch01".to_string(),
            kind: "chart.candles".to_string(),
            rev: 2,
            payload: serde_json::json!({
                "symbol": "0700.HK",
                "candles": [[1, 1, 1, 1, 1], [2, 2, 2, 2, 2]],
            }),
        };
        let prose_slices = split_body("# A\nalpha\n");
        let mut old = reassign_ids(&[], &prose_slices);
        old.push(chart.clone());
        let body = format!("# New\nintro\n# A\nalpha\n{}", flat_text(&chart));
        let out = reassign_ids(&old, &split_body(&body));
        assert_eq!(out.len(), 3);
        assert_eq!(out[1].id, old[0].id, "prose keeps id across insertion");
        assert_eq!(out[2].id, "b_ch01", "fence keeps id across insertion");
        assert_eq!(out[2].rev, 2);
        assert_eq!(out[2].payload, chart.payload);
    }

    #[test]
    fn heavily_reparameterized_data_block_keeps_id_via_same_kind_pairing() {
        // #960 PR3 review round 1: a large parameter edit drops fence
        // similarity below 0.5 — the unique same-kind pre-pass must
        // still pair the blocks (id inherited, rev+1).
        //
        // app: src "/x" → long path.
        let old = vec![ReportBlock {
            id: "b_ap01".to_string(),
            kind: "app".to_string(),
            rev: 2,
            payload: serde_json::json!({ "src": "/x" }),
        }];
        let long_src = format!("/apps/deeply/nested/{}", "z".repeat(120));
        let body = format!(
            "```neige-block app\n{{\"src\": \"{long_src}\", \"title\": \"tooling\"}}\n```\n"
        );
        let out = reassign_ids(&old, &split_body(&body));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "b_ap01", "id survives the large edit");
        assert_eq!(out[0].rev, 3, "changed payload: rev+1");
        assert_eq!(out[0].payload["src"], serde_json::json!(long_src));

        // chart: most candles replaced, surrounded by prose that also
        // changed — still pairs inside the gap.
        let chart = ReportBlock {
            id: "b_ch01".to_string(),
            kind: "chart.candles".to_string(),
            rev: 4,
            payload: serde_json::json!({
                "symbol": "0700.HK",
                "candles": [[1, 1, 1, 1, 1], [2, 2, 2, 2, 2]],
            }),
        };
        let mut old = reassign_ids(&[], &split_body("# A\nalpha\n"));
        old.push(chart);
        let new_candles: Vec<serde_json::Value> = (100..160)
            .map(|i| serde_json::json!([i, i * 7, i * 9, i * 3, i * 5]))
            .collect();
        let new_fence = flat_text(&ReportBlock {
            id: String::new(),
            kind: "chart.candles".to_string(),
            rev: 0,
            payload: serde_json::json!({ "symbol": "9988.HK", "candles": new_candles }),
        });
        let out = reassign_ids(&old, &split_body(&format!("# A\nalpha\n{new_fence}")));
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].id, "b_ch01", "chart id survives wholesale re-data");
        assert_eq!(out[1].rev, 5, "rev+1");

        // Two same-kind blocks in one gap: ambiguity falls back to
        // similarity — the byte-identical one is LCS-anchored, the
        // remaining one-on-one pair still resolves via the pre-pass.
        let a = ReportBlock {
            id: "b_a".to_string(),
            kind: "app".to_string(),
            rev: 1,
            payload: serde_json::json!({ "src": "/keep" }),
        };
        let b = ReportBlock {
            id: "b_b".to_string(),
            kind: "app".to_string(),
            rev: 1,
            payload: serde_json::json!({ "src": "/rewrite" }),
        };
        let body = format!(
            "{}```neige-block app\n{{\"src\": \"/totally/different/{}\"}}\n```\n",
            flat_text(&a),
            "w".repeat(80)
        );
        let out = reassign_ids(&[a, b], &split_body(&body));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "b_a", "identical fence LCS-anchored");
        assert_eq!(out[0].rev, 1);
        assert_eq!(out[1].id, "b_b", "leftover same-kind pair inherits");
        assert_eq!(out[1].rev, 2);
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
                payload: serde_json::json!({ "markdown": "# A\nsame\n" }),
            },
            ReportBlock {
                id: "b_bbbb".to_string(),
                kind: "prose".to_string(),
                rev: 7,
                payload: serde_json::json!({ "markdown": "# A\nsame\n" }),
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
                payload: serde_json::json!({ "markdown": "# A\n" }),
            },
            ReportBlock {
                id: "dup".to_string(),
                kind: "prose".to_string(),
                rev: 1,
                payload: serde_json::json!({ "markdown": "# B\n" }),
            },
            ReportBlock {
                id: "broken".to_string(),
                kind: "prose".to_string(),
                rev: 1,
                payload: serde_json::json!({}),
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
