//! #1595: AI edits use the production Report block write/read path.
use super::*;

#[tokio::test]
async fn layout_block_write_persists_edits_and_fences_stale_revisions() {
    let boot = boot().await;
    let payload = json!({"version":1,"columns":1,"gap":"normal","surface":"plain","items":[{
        "kind":"chart","title":"History","span":1,"data":{"source":"neige://plugin/market/history"},
        "chart":"line","x":"at","y":"value","height":240,"color":"#123456"
    }]});
    let before = read(&boot, json!({})).await;
    let created = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        planner_identity(&boot),
        json!({"kind":"layout","payload":payload,"if_doc_rev":before["docRev"]}),
    )
    .await
    .expect("create saved layout");
    let stored = current_payload(&boot).await;
    let block = stored
        .blocks
        .as_ref()
        .unwrap()
        .iter()
        .find(|b| b.kind == "layout")
        .unwrap();
    assert_eq!(block.payload, payload);
    let mut edited = payload.clone();
    edited["items"][0]["title"] = json!("Updated from chat");
    edited["surface"] = json!("muted");
    let args = json!({"id":block.id,"kind":"layout","payload":edited,"if_rev":block.rev});
    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        planner_identity(&boot),
        args.clone(),
    )
    .await
    .expect("AI updates configuration");
    let current = current_payload(&boot).await;
    assert_eq!(
        current
            .blocks
            .as_ref()
            .unwrap()
            .iter()
            .find(|b| b.kind == "layout")
            .unwrap()
            .payload,
        edited
    );
    assert!(current.doc_rev > created["docRev"].as_u64().unwrap());
    let error = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        planner_identity(&boot),
        args,
    )
    .await
    .expect_err("stale edit rejected");
    assert_eq!(error.code, RPC_REV_CONFLICT);
    let reread = read(&boot, json!({})).await;
    assert!(
        reread["text"]
            .as_str()
            .unwrap()
            .contains(&calm_types::report_blocks::render_fence("layout", &edited))
    );
    let mut invalid = edited;
    invalid["items"][0]["rogue"] = json!(true);
    let error = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        planner_identity(&boot),
        json!({"kind":"layout","payload":invalid,"if_doc_rev":reread["docRev"]}),
    )
    .await
    .expect_err("invalid shape rejected");
    assert_eq!(error.code, -32602);
    assert!(error.message.contains("rogue"));
    assert_eq!(
        read(&boot, json!({})).await["docRev"],
        reread["docRev"],
        "invalid write leaves saved document unchanged"
    );
}
