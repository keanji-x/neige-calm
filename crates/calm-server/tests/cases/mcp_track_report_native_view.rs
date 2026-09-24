use crate::mcp_track_report::{boot, call_tool, planner_identity};
use serde_json::{Value, json};

#[tokio::test]
async fn native_view_upsert_read_roundtrip_and_cas_are_one_truth() {
    let boot = boot().await;
    let read = call_tool(
        &boot,
        "calm.report.read",
        planner_identity(&boot),
        json!({}),
    )
    .await
    .unwrap();
    let fixture: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test-data/native-view-v1.json"
    )))
    .unwrap();
    let created = call_tool(
        &boot,
        "calm.report.blocks.upsert",
        planner_identity(&boot),
        json!({"kind":"view","payload":fixture["valid"],"if_doc_rev":read["docRev"]}),
    )
    .await
    .unwrap();
    let result = call_tool(
        &boot,
        "calm.report.read",
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
    let error = call_tool(
        &boot,
        "calm.report.blocks.upsert",
        planner_identity(&boot),
        json!({"id":created["id"],"kind":"view","payload":fixture["valid"],"if_rev":999}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32001);
    let after = call_tool(
        &boot,
        "calm.report.read",
        planner_identity(&boot),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(after["docRev"], result["docRev"]);
    let kinds = call_tool(
        &boot,
        "calm.report.blocks.kinds",
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
