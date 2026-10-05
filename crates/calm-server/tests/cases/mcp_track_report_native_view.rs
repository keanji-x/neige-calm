use crate::mcp_track_report::{
    assistant_identity, boot, call_tool, planner_identity, upsert_block,
};
use serde_json::{Value, json};

#[tokio::test]
async fn native_view_upsert_read_roundtrip_and_cas_are_one_truth() {
    let boot = boot().await;
    let fixture: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test-data/native-view-v1.json"
    )))
    .unwrap();
    let created = upsert_block(
        &boot,
        planner_identity(&boot),
        json!({"kind":"view","payload":fixture["valid"]}),
    )
    .await
    .unwrap();
    let result = call_tool(
        &boot,
        "neige_report_read",
        planner_identity(&boot),
        json!({}),
    )
    .await
    .unwrap();
    let parsed = calm_types::report_blocks::split_body(result["text"].as_str().unwrap())
        .iter()
        .filter_map(|slice| calm_types::report_blocks::parse_fence(&slice.raw))
        .find(|b| b.kind == "view")
        .unwrap();
    assert_eq!(
        parsed.payload, fixture["valid"],
        "Planner and renderer receive the persisted components, not separate summaries"
    );
    // A different session changes this exact block after the Planner's read.
    let mut changed = fixture["valid"].clone();
    changed["title"] = json!("Changed by another session");
    let updated = upsert_block(
        &boot,
        assistant_identity(&boot),
        json!({"id":created["id"],"kind":"view","payload":changed}),
    )
    .await
    .unwrap();
    let error = call_tool(
        &boot,
        "neige_report_commit",
        planner_identity(&boot),
        json!({"message":"stale replacement", "ops":[{"op":"upsert","id":created["id"],"kind":"view","payload":fixture["valid"]}]}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32001);
    let after = call_tool(
        &boot,
        "neige_report_read",
        planner_identity(&boot),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(after["docRev"], updated["docRev"]);
    assert!(
        after["text"]
            .as_str()
            .unwrap()
            .contains("Changed by another session")
    );
    let kinds = call_tool(
        &boot,
        "neige_report_describe",
        planner_identity(&boot),
        json!({}),
    )
    .await
    .unwrap();
    let descriptor = kinds["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["kind"] == "view")
        .unwrap();
    assert_eq!(descriptor["schema"]["additionalProperties"], false);
}

#[tokio::test]
async fn native_view_template_of_live_slots_is_admitted_with_its_snapshot_rule() {
    let boot = boot().await;
    let fixture: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test-data/native-view-v1.json"
    )))
    .unwrap();
    upsert_block(
        &boot,
        planner_identity(&boot),
        json!({"kind":"view","payload":fixture["valid_slots"]}),
    )
    .await
    .expect("a template of live slots is a valid view");
    let mut unsourced = fixture["valid"].clone();
    unsourced["snapshot"] = Value::Null;
    let error = upsert_block(
        &boot,
        planner_identity(&boot),
        json!({"kind":"view","payload":unsourced}),
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("view.snapshot"), "{}", error.message);
}
