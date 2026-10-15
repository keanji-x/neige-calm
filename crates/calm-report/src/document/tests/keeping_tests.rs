//! Where [`ReportDoc::update_with_hints_keeping`] puts the blocks a whole write does not own (#2617).

use super::*;
use calm_types::report_blocks::BlockSlice;

fn prose(id: &str, markdown: &str) -> ReportBlock {
    ReportBlock {
        id: id.into(),
        kind: KIND_PROSE.into(),
        rev: 3,
        payload: json!({ "markdown": markdown }),
    }
}

fn kept(id: &str) -> ReportBlock {
    ReportBlock {
        id: id.into(),
        kind: "app".into(),
        rev: 5,
        payload: json!({ "src": format!("/{id}") }),
    }
}

/// Rewrite the prose of `blocks` to `slices` (with `hints`); every other block is kept.
fn rewrite(blocks: &[ReportBlock], slices: &[&str], hints: &[Option<&str>]) -> Vec<ReportBlock> {
    let mut doc = ReportDoc::from_blocks_exact("s", blocks).unwrap();
    let slices: Vec<BlockSlice> = slices
        .iter()
        .map(|raw| BlockSlice {
            raw: raw.to_string(),
        })
        .collect();
    let hints: Vec<Option<String>> = hints.iter().map(|hint| hint.map(str::to_string)).collect();
    doc.update_with_hints_keeping("s", &slices, &hints, |block| block.kind == KIND_PROSE)
        .unwrap();
    doc.blocks_snapshot().unwrap()
}

fn ids(blocks: &[ReportBlock]) -> Vec<&str> {
    blocks.iter().map(|block| block.id.as_str()).collect()
}

#[test]
fn a_kept_block_follows_the_nearest_earlier_survivor_when_its_own_anchor_is_deleted() {
    let blocks = [
        prose("b_0001", "# A\nalpha\n"),
        prose("b_0002", "# B\nbeta\n"),
        kept("b_00aa"),
        prose("b_0003", "# C\ngamma\n"),
    ];
    let after = rewrite(
        &blocks,
        &["# A\nalpha\n", "# C\ngamma\n"],
        &[Some("b_0001"), Some("b_0003")],
    );
    assert_eq!(ids(&after), ["b_0001", "b_00aa", "b_0003"]);
    assert_eq!(after[1], blocks[2], "the kept block is untouched");
}

#[test]
fn a_kept_block_with_no_surviving_anchor_goes_after_the_first_written_block() {
    let blocks = [
        prose("b_0001", "# A\nalpha\n"),
        kept("b_00aa"),
        prose("b_0002", "# B\nbeta\n"),
    ];
    let after = rewrite(
        &blocks,
        &[
            "# Totally new\nfirst line of text\n",
            "# Other\nsecond part entirely\n",
        ],
        &[None, None],
    );
    assert!(
        !ids(&after).contains(&"b_0001") && !ids(&after).contains(&"b_0002"),
        "premise: no rewritten block survives: {after:?}"
    );
    assert_eq!(after.len(), 3);
    assert_eq!(after[1], blocks[1]);
    assert!(flat_text(&after[0]).starts_with("# Totally new"));
}

#[test]
fn kept_blocks_before_every_rewritten_block_stay_in_front() {
    let blocks = [
        kept("b_00aa"),
        kept("b_00bb"),
        prose("b_0001", "# A\nalpha\n"),
        kept("b_00cc"),
    ];
    let after = rewrite(&blocks, &["# A\nalpha edited\n"], &[Some("b_0001")]);
    assert_eq!(ids(&after), ["b_00aa", "b_00bb", "b_0001", "b_00cc"]);
    assert_eq!(after[2].rev, 4, "the edited block's rev moves");
    assert_eq!(
        [&after[0], &after[1], &after[3]],
        [&blocks[0], &blocks[1], &blocks[3]]
    );
}
