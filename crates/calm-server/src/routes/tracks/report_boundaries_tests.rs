use super::{ReportBlock, prepare_fork_report};
use serde_json::json;

#[test]
fn fork_import_preserves_unterminated_raw_blocks_and_projects_boundaries() {
    let blocks = vec![
        ReportBlock {
            id: "b_0001".into(),
            kind: "prose".into(),
            rev: 7,
            payload: json!({"markdown": "# A\nalpha"}),
        },
        ReportBlock {
            id: "b_0002".into(),
            kind: "prose".into(),
            rev: 3,
            payload: json!({"markdown": "# B\nbeta"}),
        },
        ReportBlock {
            id: "b_0003".into(),
            kind: "app".into(),
            rev: 2,
            payload: json!({"src": "/example"}),
        },
    ];
    let mut prepared =
        prepare_fork_report("summary".into(), blocks.clone(), "source", "target").unwrap();
    assert_eq!(prepared.payload.blocks.as_ref(), Some(&blocks));
    assert_eq!(prepared.doc.blocks_snapshot().unwrap(), blocks);
    assert!(
        prepared
            .payload
            .body
            .starts_with("# A\nalpha\n# B\nbeta\n```neige-block app")
    );
    let reloaded =
        crate::track_report_doc::ReportDoc::from_bytes(&prepared.doc.to_bytes()).unwrap();
    assert_eq!(reloaded.blocks_snapshot().unwrap(), blocks);
    assert_eq!(reloaded.project().unwrap().1, prepared.payload.body);
}
