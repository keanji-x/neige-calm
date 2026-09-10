use super::*;

#[test]
fn summary_preserves_failures_unknowns_and_c2_vs_c1() {
    let source = json!({"key":"repair","attempt_id":"repair-2","generation":2,"status":"done","blocking_reason":null,
        "file_delivery":{"publication":{"state":"failed","failure":"出".repeat(300)},
            "candidate":{"state":"sealed","publication_operation_id":"C2","snapshot":"S2"},
            "verification":{"state":"succeeded","passed":null,"failure":"checks unavailable","policy":{"steps":[{"cmd":"private"}]}},
            "review":{"passed":true,"state":"passed","operation":{"state":"failed","failure":"settlement failed"},"blocking_findings":["private"]},
            "input":{"publication_operation_id":"C1","purpose":"candidate-repair-input"},
            "repair":{"snapshot":"S1","repair_key":"repair","blocking_findings":["private"]},
            "qualification":{"qualified":false,"reason":"withdrawn"}}});
    let out = summary(&source);
    assert_eq!(
        out["file_delivery"]["candidate"]["publication_operation_id"],
        "C2"
    );
    assert_eq!(
        out["file_delivery"]["input"]["publication_operation_id"],
        "C1"
    );
    assert_eq!(out["file_delivery"]["repair"]["snapshot"], "S1");
    assert_eq!(
        out["file_delivery"]["publication"]["failure"],
        "出".repeat(256)
    );
    assert_eq!(
        out["truncated_fields"],
        json!(["/file_delivery/publication/failure"])
    );
    assert_eq!(
        out["file_delivery"]["review"]["operation"],
        source["file_delivery"]["review"]["operation"]
    );
    assert_eq!(out["file_delivery"]["review"]["passed"], true);
    assert!(out["file_delivery"]["verification"]["passed"].is_null());
    assert_eq!(
        out["file_delivery"]["verification"]["failure"],
        "checks unavailable"
    );
    assert_eq!(
        out["file_delivery"]["qualification"],
        source["file_delivery"]["qualification"]
    );
    assert!(!out.to_string().contains("private"));
    assert!(
        out["omitted_fields"]
            .as_array()
            .unwrap()
            .contains(&json!("/file_delivery/review/blocking_findings"))
    );
}

#[test]
fn summary_history_growth_is_omitted_and_bounded() {
    let mut source = json!({"key":"a","kind":"codex","attempt_id":"a-1","generation":1,"status":"done","blocking_reason":null,
        "activity":{"coverage":"partial","collector_health":"unknown","recent":[{"detail":{"kind":"agent_message_recorded","summary":"bulky"},"row_id":1}]},
        "file_delivery":{"review":{"state":"passed","passed":true,"blocking_findings":[],"finding_responses":[{"finding_index":0,"status":"resolved","evidence":"bulky"}]}}});
    let small = summary(&source);
    source["activity"]["recent"][0]["detail"]["summary"] = json!("private".repeat(90));
    source["file_delivery"]["review"]["finding_responses"][0]["evidence"] =
        json!("private".repeat(200));
    let large = summary(&source);
    assert_eq!(small, large);
    assert!(large.to_string().len() <= 12 * 1024);
    assert!(!large.to_string().contains("private"));
}
