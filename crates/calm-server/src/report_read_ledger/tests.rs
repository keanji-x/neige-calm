use super::*;
use calm_types::report_blocks::{KIND_TASK, reassign_ids, render_fence, split_body};

fn ids(blocks: &[ReportBlock], indices: &[usize]) -> Vec<String> {
    indices.iter().map(|i| blocks[*i].id.clone()).collect()
}

#[test]
fn records_rendered_blocks_and_only_whole_sections() {
    let blocks = reassign_ids(&[], &split_body("intro\n# A\na\n## A1\nx\n# B\nb\n"));
    let read = record_read(None, 3, &blocks, &ids(&blocks, &[1, 3]));
    assert_eq!(read.doc_rev, 3);
    assert!(!read.whole, "a partial read is not a whole read");
    assert_eq!(read.blocks.len(), 2);
    assert_eq!(read.sections.keys().collect::<Vec<_>>(), vec!["B"]);
}

#[test]
fn section_anchors_exclude_task_blocks() {
    let task = render_fence(KIND_TASK, &serde_json::json!({"key":"k"}));
    let blocks = reassign_ids(&[], &split_body(&format!("# A\na\n{task}## A1\nx\n")));
    assert_eq!(blocks[1].kind, KIND_TASK);
    let read = record_read(None, 1, &blocks, &ids(&blocks, &[0, 2]));
    assert_eq!(
        read.sections.get("A"),
        Some(&vec![
            (blocks[0].id.clone(), blocks[0].rev),
            (blocks[2].id.clone(), blocks[2].rev),
        ])
    );
    assert!(!read.blocks.contains_key(&blocks[1].id));
}

#[test]
fn whole_is_a_read_of_every_non_task_block_at_the_recorded_doc_rev() {
    let blocks = reassign_ids(&[], &split_body("# A\na\n# B\nb\n"));
    let read = record_read(None, 1, &blocks, &ids(&blocks, &[0]));
    assert!(!read.whole);
    let read = record_read(Some(read), 1, &blocks, &ids(&blocks, &[0, 1]));
    assert!(read.whole);
    let read = record_read(Some(read), 1, &blocks, &ids(&blocks, &[1]));
    assert!(read.whole);
    let read = project_commit(&read, 1, &blocks, 2, &blocks, &[], &[]);
    assert!(read.whole);
    assert_eq!(read.doc_rev, 2);
    let read = record_read(Some(read), 3, &blocks, &ids(&blocks, &[1]));
    assert!(!read.whole);
}
