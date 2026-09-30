use super::*;

#[test]
fn observation_summary_is_one_line_without_screen_text() {
    let state = json!({"terminal_id":"t-1","observation_id":"o-1","observation_revision":3,
        "role":"owner","cols":80,"rows":24,"cursor":{"row":1,"column":2},
        "wait":{"outcome":"elapsed"},"text":["SECRET_SCREEN"]});
    let wire = serde_json::to_value(observation_result(state.clone())).unwrap();
    assert_eq!(wire["structuredContent"], state);
    assert_eq!(
        wire["content"],
        json!([{"type":"text","text":"terminal t-1 observation o-1 revision 3 owner 80x24 cursor 1,2 wait elapsed; \
            full state in structuredContent"}])
    );
}

#[test]
fn terminal_open_failure_is_a_one_line_summary_with_structured_detail() {
    let receipt = json!({"operation_id":"op-7","outcome":"unavailable","detail":"Failed { error: \"spawn refused\" }"});
    let wire = serde_json::to_value(open_failure_result(receipt.clone())).unwrap();
    assert_eq!(wire["structuredContent"], receipt);
    let content = wire["content"].as_array().unwrap();
    assert_eq!(content.len(), 1);
    assert_eq!(
        content[0]["text"],
        "terminal open unavailable operation op-7; details in structuredContent"
    );
    assert!(
        !content[0]["text"]
            .as_str()
            .unwrap()
            .contains("spawn refused"),
        "the detail must live only in structuredContent"
    );
}
