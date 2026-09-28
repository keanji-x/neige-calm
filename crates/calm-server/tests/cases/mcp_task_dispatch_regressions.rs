//! Review regressions against the actual MCP dispatch and report writer.
use crate::mcp_task_dispatch::{args, boot, counts, dispatch, payload, policy};
use crate::mcp_track_report::{Boot, call_tool, planner_identity};
use serde_json::json;

async fn claim_dispatch(b: &Boot, key: &str) {
    let task = crate::task_recovery::current(b, key).await;
    let monitor = calm_server::task_context::TaskContextMonitor::new(
        b.repo.clone(),
        b.ctx.events.clone(),
        b.ctx.write.clone(),
    );
    let closure = monitor
        .resolve_task_closure(b.track_id.as_str(), key)
        .await
        .unwrap();
    let pool = b.repo.sqlite_pool().unwrap();
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
        .await
        .unwrap();
    assert_eq!(
        calm_server::db::sqlite::task_claim_pending_tx(
            &mut tx,
            &task.id,
            10,
            &closure.refs,
            closure.closure_truncated
        )
        .await
        .unwrap(),
        1
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn dispatch_acceptance_drift_remains_visible_after_claim_and_ready_changes_do_not_drift() {
    let b = boot().await;
    policy(&b, "declare-and-wait").await;
    let original = dispatch(&b, args()).await.unwrap();
    let block = payload(&b)
        .await
        .blocks
        .unwrap()
        .into_iter()
        .find(|x| x.kind == "task")
        .unwrap();
    let mut changed = block.payload.clone();
    changed["acceptance"] = json!("A different acceptance contract");
    call_tool(
        &b,
        "calm.report.blocks.upsert",
        planner_identity(&b),
        json!({"id":block.id,"kind":"task","payload":changed,"if_rev":block.rev}),
    )
    .await
    .unwrap();
    let saved = counts(&b).await;
    let replay = dispatch(&b, args()).await.unwrap();
    assert_eq!(
        replay["current"]["contract_status"],
        "differs_from_dispatch"
    );
    assert_eq!(replay["receipt"], original["receipt"]);
    assert_eq!(counts(&b).await, saved);
    policy(&b, "auto-declare").await;
    // Reproject the edited declaration through its real authoring entry, then
    // freeze it with the same production claim seam used by the scheduler.
    let block = payload(&b)
        .await
        .blocks
        .unwrap()
        .into_iter()
        .find(|x| x.kind == "task")
        .unwrap();
    call_tool(
        &b,
        "calm.report.blocks.upsert",
        planner_identity(&b),
        json!({"id":block.id,"kind":"task","payload":block.payload,"if_rev":block.rev}),
    )
    .await
    .unwrap();
    let key = original["receipt"]["task_key"].as_str().unwrap();
    claim_dispatch(&b, key).await;
    let frozen = crate::task_recovery::current(&b, key).await;
    assert_eq!(
        frozen.acceptance_criteria.as_deref(),
        Some("A different acceptance contract")
    );
    let saved = counts(&b).await;
    let replay = dispatch(&b, args()).await.unwrap();
    assert_eq!(
        replay["current"]["contract_status"],
        "differs_from_dispatch"
    );
    assert_eq!(replay["current"]["task"]["status"], "dispatched");
    assert_eq!(replay["receipt"], original["receipt"]);
    assert_eq!(counts(&b).await, saved);

    let b = boot().await;
    policy(&b, "declare-and-wait").await;
    let original = dispatch(&b, args()).await.unwrap();
    for ready in [false, true] {
        let block = payload(&b)
            .await
            .blocks
            .unwrap()
            .into_iter()
            .find(|x| x.kind == "task")
            .unwrap();
        let mut changed = block.payload;
        changed["ready"] = json!(ready);
        call_tool(
            &b,
            "calm.report.blocks.upsert",
            planner_identity(&b),
            json!({"id":block.id,"kind":"task","payload":changed,"if_rev":block.rev}),
        )
        .await
        .unwrap();
        let replay = dispatch(&b, args()).await.unwrap();
        assert_eq!(replay["current"]["contract_status"], "matches_dispatch");
        assert_eq!(replay["receipt"], original["receipt"]);
    }
    for released in [true, false] {
        let block = payload(&b)
            .await
            .blocks
            .unwrap()
            .into_iter()
            .find(|x| x.kind == "task")
            .unwrap();
        let mut changed = block.payload;
        changed["released_by_user"] = json!(released);
        crate::task_projection_acceptance::user_upsert(&b, &block.id, block.rev as u64, changed)
            .await;
        let saved = counts(&b).await;
        let replay = dispatch(&b, args()).await.unwrap();
        assert_eq!(replay["current"]["contract_status"], "matches_dispatch");
        assert_eq!(replay["receipt"], original["receipt"]);
        assert_eq!(counts(&b).await, saved);
    }
}

#[tokio::test]
async fn dispatch_invalid_current_declaration_never_claims_contract_match() {
    let b = boot().await;
    policy(&b, "declare-and-wait").await;
    let original = dispatch(&b, args()).await.unwrap();
    let p = payload(&b).await;
    let block = p
        .blocks
        .as_ref()
        .unwrap()
        .iter()
        .find(|x| x.kind == "task")
        .unwrap();
    let mut invalid = block.payload.clone();
    invalid["priority"] = json!("invalid historical value");
    // Emulate a malformed persisted report from an older writer. The fields in
    // the execution-root partition are unchanged; schema invalidity must still
    // prevent a matching claim. The read exercises the production MCP snapshot.
    let mut doc = calm_server::track_report_doc::ReportDoc::from_payload(&p);
    doc.ensure_blocks_layout(p.blocks.as_deref()).unwrap();
    doc.upsert_block(
        Some(&block.id),
        "task",
        &calm_types::report_blocks::render_fence("task", &invalid),
    )
    .unwrap();
    sqlx::query(
        "UPDATE cards SET body_crdt=?1,payload=json_set(payload,'$.body',?2,'$.blocks',json(?3)) \
         WHERE id=?4",
    )
    .bind(doc.to_bytes())
    .bind(doc.project().unwrap().1)
    .bind(serde_json::to_string(&doc.blocks_snapshot().unwrap()).unwrap())
    .bind(b.report_card_id.as_str())
    .execute(&b.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    let saved = counts(&b).await;
    let replay = dispatch(&b, args()).await.unwrap();
    assert_eq!(replay["current"]["contract_status"], "unavailable");
    assert_eq!(replay["receipt"], original["receipt"]);
    assert_eq!(counts(&b).await, saved);
}
