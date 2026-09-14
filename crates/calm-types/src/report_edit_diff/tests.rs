use super::*;
use crate::report_blocks::render_fence;
use serde_json::json;

fn task(key: &str, ready: bool, goal: &str) -> String {
    render_fence(
        KIND_TASK,
        &json!({
            "key": key,
            "kind": "codex",
            "ready": ready,
            "declared_by": "user",
            "goal": goal,
        }),
    )
}

/// A1 — one block of each class, each named by heading with its own
/// verb, and the modified one carries `-N/+M` plus the changed lines.
#[test]
fn diff_names_added_removed_and_modified_blocks() {
    let before = "# Title\n\nintro\n\n## Thesis\n\nold claim\nshared line\n\n## Draft\n\nscrap\n";
    let after =
        "# Title\n\nintro\n\n## Thesis\n\nnew claim\nshared line\n\n## Risks\n\nfx exposure\n";
    let out = render_report_diff(before, after);
    assert!(
        out.starts_with("Blocks: 1 added, 1 removed, 1 modified (1 unchanged)."),
        "{out}"
    );
    assert!(
        out.contains("## modified: `## Thesis` (-1/+1 lines)"),
        "{out}"
    );
    assert!(out.contains("\n-old claim\n+new claim\n"), "{out}");
    assert!(
        !out.contains("shared line"),
        "unchanged lines are not excerpted: {out}"
    );
    assert!(out.contains("## added: `## Risks` (+3 lines)"), "{out}");
    assert!(out.contains("\n+fx exposure\n"), "{out}");
    assert!(out.contains("## removed: `## Draft` (-3 lines)"), "{out}");
    assert!(out.contains("\n-scrap\n"), "{out}");
    assert!(
        !out.contains("# Title"),
        "unchanged block must not appear: {out}"
    );
}

/// A1 — a task fence reports `ready` old -> new by name and never
/// pastes its payload (the goal text stays out of the turn input).
#[test]
fn task_fence_names_ready_and_key_without_payload_text() {
    let before = format!("## Plan\n\n{}", task("build", false, "secret goal prose"));
    let after = format!("## Plan\n\n{}", task("build", true, "secret goal prose"));
    let out = render_report_diff(&before, &after);
    assert!(
        out.contains("## modified: `task` block key = \"build\""),
        "{out}"
    );
    assert!(out.contains("\nfields changed: ready\n"), "{out}");
    assert!(out.contains("\nready: false -> true\n"), "{out}");
    assert!(out.contains("\nkey: \"build\" -> \"build\"\n"), "{out}");
    assert!(
        !out.contains("secret goal prose"),
        "payload text leaked: {out}"
    );
    assert!(!out.contains("```"), "fence text leaked: {out}");
}

/// A1 — a renamed task key is one modification (leftover tasks pair by
/// order), with the rename spelled out.
#[test]
fn task_key_rename_is_one_modification() {
    let before = task("old-key", true, "g");
    let after = task("new-key", true, "g");
    let out = render_report_diff(&before, &after);
    assert!(
        out.starts_with("Blocks: 0 added, 0 removed, 1 modified (0 unchanged)."),
        "{out}"
    );
    assert!(out.contains("\nkey: \"old-key\" -> \"new-key\"\n"), "{out}");
}

/// A1 — a non-task data block: kind plus changed top-level keys, no
/// payload lines.
#[test]
fn data_fence_reports_kind_and_changed_keys_only() {
    let before = render_fence("table", &json!({"caption": "a", "rows": [[1, 2]]}));
    let after = render_fence("table", &json!({"caption": "b", "rows": [[1, 2]]}));
    let out = render_report_diff(&before, &after);
    assert!(out.contains("## modified: `table` block\n"), "{out}");
    assert!(out.contains("\nfields changed: caption\n"), "{out}");
    assert!(!out.contains("rows"), "unchanged key named: {out}");
    assert!(
        !out.contains("\"a\"") && !out.contains("\"b\""),
        "payload leaked: {out}"
    );
    assert!(!out.contains("ready:"), "task-only lines on a table: {out}");
}

/// A1 — the per-block excerpt stops at `MAX_BLOCK_LINES` and says so.
#[test]
fn per_block_excerpt_is_capped_at_the_line_limit() {
    let before = "## Long\n\nx\n".to_string();
    let body: String = (0..100).map(|i| format!("line {i}\n")).collect();
    let after = format!("## Long\n\n{body}");
    let out = render_report_diff(&before, &after);
    let excerpt: Vec<&str> = out
        .lines()
        .filter(|l| l.starts_with('+') || l.starts_with('-'))
        .collect();
    assert_eq!(excerpt.len(), MAX_BLOCK_LINES, "{out}");
    assert!(out.contains(TRUNCATED_MARKER), "{out}");
    assert!(
        out.contains("(-1/+100 lines)"),
        "counts are of the whole block: {out}"
    );
}

/// A1 — the whole rendering is bounded by `MAX_TOTAL_BYTES` and ends
/// with the marker when cut; a single over-long line is cut at a char
/// boundary rather than panicking.
#[test]
fn total_rendering_is_capped_and_ends_with_the_marker() {
    let mut before = String::new();
    let mut after = String::new();
    for i in 0..60 {
        before.push_str(&format!("## Section {i}\n\nbefore {i}\n\n"));
        after.push_str(&format!(
            "## Section {i}\n\nafter {i} {}\n\n",
            "é".repeat(400)
        ));
    }
    let out = render_report_diff(&before, &after);
    assert!(out.len() <= MAX_TOTAL_BYTES, "{} bytes", out.len());
    assert!(out.ends_with(&format!("{TRUNCATED_MARKER}\n")), "{out}");
    assert!(out.contains("## modified: `## Section 0`"), "{out}");
    assert!(
        !out.contains("## modified: `## Section 59`"),
        "the tail was cut: {out}"
    );
}

#[test]
fn identical_bodies_say_so() {
    let out = render_report_diff("# A\n\nsame\n", "# A\n\nsame\n");
    assert_eq!(
        out,
        "No block-level changes (the body is byte-identical).\n"
    );
}

/// Same heading twice pairs by order of appearance.
#[test]
fn duplicate_headings_pair_by_order() {
    let before = "## Note\n\nfirst\n\n## Note\n\nsecond\n";
    let after = "## Note\n\nfirst\n\n## Note\n\nsecond changed\n";
    let out = render_report_diff(before, after);
    assert!(
        out.starts_with("Blocks: 0 added, 0 removed, 1 modified (1 unchanged)."),
        "{out}"
    );
    assert!(out.contains("\n-second\n+second changed\n"), "{out}");
}

/// Prose replaced by a data block under the same heading: the heading's
/// prose is modified (its line removed) and the fence is an addition —
/// and the fence's payload is still not pasted.
#[test]
fn prose_replaced_by_fence_is_a_modification_plus_an_addition() {
    let before = "## Plan\n\nprose plan\n";
    let after = format!("## Plan\n\n{}", task("k", true, "hidden"));
    let out = render_report_diff(before, &after);
    assert!(
        out.starts_with("Blocks: 1 added, 0 removed, 1 modified (0 unchanged)."),
        "{out}"
    );
    assert!(
        out.contains("## modified: `## Plan` (-1/+0 lines)"),
        "{out}"
    );
    assert!(
        out.contains("## added: `task` block key = \"k\" (+"),
        "{out}"
    );
    assert!(!out.contains("hidden"), "{out}");
}

fn prose(id: &str, rev: u32, markdown: &str) -> ReportBlock {
    ReportBlock {
        id: id.into(),
        kind: "prose".into(),
        rev,
        payload: json!({ "markdown": markdown }),
    }
}

fn refs(pairs: &[(&str, u32)]) -> Vec<ReportBlockRef> {
    pairs
        .iter()
        .map(|(id, rev)| ReportBlockRef {
            id: (*id).into(),
            rev: *rev,
        })
        .collect()
}

/// #1667 round-2 F1 — a snapshot that projects to exactly `after`, one
/// block per slice, aligns by position; the last block may be
/// unterminated and a terminated one may be followed by another.
#[test]
fn snapshot_that_projects_to_after_aligns_one_ref_per_slice() {
    let blocks = [
        prose("b_0001", 1, "# T\n\nintro\n"),
        prose("b_0002", 4, "## Thesis\n\nclaim\n"),
        prose("b_0003", 2, "## Next\n\nsteps"),
    ];
    let after = "# T\n\nintro\n## Thesis\n\nclaim\n## Next\n\nsteps";
    assert_eq!(
        align_block_refs(after, &blocks),
        Some(refs(&[("b_0001", 1), ("b_0002", 4), ("b_0003", 2)]))
    );
}

/// F1 — the projection inserts a line break after an unterminated block;
/// that break belongs to the preceding slice and alignment still holds.
#[test]
fn unterminated_middle_block_still_aligns() {
    let blocks = [
        prose("b_0001", 1, "# T\n\nintro"),
        prose("b_0002", 1, "## B\n\nx\n"),
    ];
    let after = "# T\n\nintro\n## B\n\nx\n";
    assert_eq!(
        align_block_refs(after, &blocks),
        Some(refs(&[("b_0001", 1), ("b_0002", 1)]))
    );
}

/// F1 — a snapshot from AFTER a later write (same layout, different
/// text) is not the body being diffed: no refs, not shifted ones.
#[test]
fn snapshot_of_a_later_write_yields_no_refs() {
    let blocks = [
        prose("b_0001", 1, "# T\n\nintro\n"),
        prose("b_0002", 5, "## Thesis\n\nclaim rewritten later\n"),
    ];
    let after = "# T\n\nintro\n## Thesis\n\nclaim\n";
    assert_eq!(align_block_refs(after, &blocks), None);
}

/// F1 — a prose block that holds two headings is two slices; the id
/// would be ambiguous, so nothing is named.
#[test]
fn block_that_splits_into_two_slices_yields_no_refs() {
    let blocks = [prose("b_0001", 1, "# T\n\nintro\n## Inner\n\nx\n")];
    let after = "# T\n\nintro\n## Inner\n\nx\n";
    assert_eq!(align_block_refs(after, &blocks), None);
}

/// F1 — an empty block is no slice at all; the sequence cannot align.
#[test]
fn empty_block_yields_no_refs() {
    let blocks = [prose("b_0001", 1, "# T\n"), prose("b_0002", 1, "")];
    let after = "# T\n";
    assert_eq!(align_block_refs(after, &blocks), None);
}

/// F1 — with refs every added / modified line carries `id (rev N)`;
/// a removed block has no after-side identity and reads as before.
#[test]
fn refs_name_added_and_modified_blocks_by_id_and_rev() {
    let before = "# Title\n\nintro\n\n## Thesis\n\nold claim\n\n## Draft\n\nscrap\n";
    let after = "# Title\n\nintro\n\n## Thesis\n\nnew claim\n\n## Risks\n\nfx exposure\n";
    let refs = refs(&[("b_aaaa", 1), ("b_ffb8", 2), ("b_c3ae", 1)]);
    let out = render_report_diff_with_refs(before, after, Some(&refs));
    assert!(
        out.contains("## modified: b_ffb8 (rev 2) `## Thesis` (-1/+1 lines)"),
        "{out}"
    );
    assert!(
        out.contains("## added: b_c3ae (rev 1) `## Risks` (+3 lines)"),
        "{out}"
    );
    assert!(out.contains("## removed: `## Draft` (-3 lines)"), "{out}");
    assert!(!out.contains("b_aaaa"), "unchanged block named: {out}");
    assert_eq!(
        render_report_diff(before, after),
        render_report_diff_with_refs(before, after, None),
        "no refs renders exactly the ref-less form"
    );
}

/// F1 — a task fence line carries the ref too.
#[test]
fn refs_name_a_modified_fence() {
    let before = format!("## Plan\n\n{}", task("build", false, "g"));
    let after = format!("## Plan\n\n{}", task("build", true, "g"));
    let refs = refs(&[("b_0001", 1), ("b_0002", 7)]);
    let out = render_report_diff_with_refs(&before, &after, Some(&refs));
    assert!(
        out.contains("## modified: b_0002 (rev 7) `task` block key = \"build\""),
        "{out}"
    );
}

/// F1 — a refs sequence of the wrong length is not this body's and is
/// ignored whole rather than applied to a prefix.
#[test]
fn refs_of_the_wrong_length_are_ignored() {
    let before = "## A\n\nx\n";
    let after = "## A\n\ny\n\n## B\n\nz\n";
    let refs = refs(&[("b_0001", 1)]);
    assert_eq!(
        render_report_diff_with_refs(before, after, Some(&refs)),
        render_report_diff(before, after)
    );
}
