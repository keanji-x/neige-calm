use super::*;

#[test]
fn summary_bounds_long_diagnostics_and_omits_private_evidence() {
    let source = json!({"key":"a","attempt_id":"a-1","generation":1,"status":"failed","blocking_reason":null,
        "status_detail":"出".repeat(300),
        "gate_result":{"status":"red","failing_step":"checks","exit_code":1,"log_tail":"private"}});
    let out = summary(&source);
    assert_eq!(out["status_detail"], "出".repeat(256));
    assert_eq!(out["truncated_fields"], json!(["/status_detail"]));
    assert_eq!(out["gate_result"]["failing_step"], "checks");
    assert!(!out.to_string().contains("private"));
    assert!(
        out["omitted_fields"]
            .as_array()
            .unwrap()
            .contains(&json!("/gate_result/log_tail"))
    );
}

#[test]
fn summary_history_growth_is_omitted_and_bounded() {
    let mut source = json!({"key":"a","kind":"codex","attempt_id":"a-1","generation":1,"status":"done","blocking_reason":null,
        "candidate":{"binding":"bound","workspace":{"path":"bulky"}}});
    let small = summary(&source);
    source["candidate"]["workspace"]["path"] = json!("private".repeat(90));
    let large = summary(&source);
    assert_eq!(small, large);
    assert!(large.to_string().len() <= 12 * 1024);
    assert!(!large.to_string().contains("private"));
}
