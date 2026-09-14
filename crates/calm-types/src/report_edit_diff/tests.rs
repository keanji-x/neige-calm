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
