use super::*;
#[test]
fn objective_fork_is_refused() {
    let block = ReportBlock {
        id: "b_1111".into(),
        kind: "objective".into(),
        rev: 0,
        payload: crate::track_report::objective_tests::payload(),
    };
    assert!(
        matches!(prepare_fork_report("s".into(),vec![block],"from","to"),Err(CalmError::BadRequest(reason)) if reason.contains("objective"))
    );
}
