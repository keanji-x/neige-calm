//! Independent prose blocks remain editable beside task fences.
use super::*;

#[tokio::test]
async fn local_prose_edit_preserves_task_after_unterminated_block_upserts() {
    let boot = boot().await;
    for markdown in ["# First\nlocal prose", "# Second\noriginal decision"] {
        call_tool(
            &boot,
            TOOL_REPORT_BLOCKS_UPSERT,
            planner_identity(&boot),
            json!({"kind": "prose", "markdown": markdown,
                "if_doc_rev": read(&boot, json!({})).await["docRev"]}),
        )
        .await
        .expect("upsert independent prose without final newline");
    }
    let (task_id, _) = seed_planner_task(&boot, "build").await;
    let before = current_payload(&boot).await;
    let task_before = before
        .blocks
        .as_ref()
        .unwrap()
        .iter()
        .find(|b| b.id == task_id)
        .unwrap()
        .clone();
    let snapshot = read(&boot, json!({})).await;
    let marked = read(&boot, json!({"with_markers": true})).await;
    let stripped =
        calm_types::report_blocks::strip_markers_and_split(marked["text"].as_str().unwrap());
    assert_eq!(stripped.cleaned, snapshot["text"].as_str().unwrap());
    assert_eq!(
        stripped.hints.iter().filter(|id| id.is_some()).count(),
        before.blocks.as_ref().unwrap().len(),
        "all markers remain standalone"
    );
    let second = before
        .blocks
        .as_ref()
        .unwrap()
        .iter()
        .find(|b| b.payload["markdown"] == "# Second\noriginal decision")
        .expect("the second prose block")
        .clone();
    let edited = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        json!({"if_doc_rev": snapshot["docRev"], "message": "local prose update",
            "ops": [{"op": "upsert", "id": second.id, "if_rev": second.rev, "kind": "prose",
                "markdown": "# Second\nrevised decision"}]}),
    )
    .await
    .expect("local prose edit must preserve the adjacent task fence");
    let after = current_payload(&boot).await;
    assert_eq!(
        after
            .blocks
            .as_ref()
            .unwrap()
            .iter()
            .find(|b| b.id == task_id),
        Some(&task_before),
        "task identity, kind, payload and revision must remain intact"
    );
    assert_eq!(task_keys(&boot).await, ["build"]);
    assert!(after.body.contains("local prose\n# Second"));
    assert!(after.body.contains("revised decision\n```neige-block task"));
    assert_eq!(edited["docRev"].as_u64(), Some(after.doc_rev));
    assert!(after.doc_rev > before.doc_rev);
    let second_rev = edited["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == second.id.as_str())
        .expect("the receipt indexes the edited block")["rev"]
        .clone();
    call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        json!({"if_doc_rev": edited["docRev"], "message": "continue with returned revision",
            "ops": [{"op": "upsert", "id": second.id, "if_rev": second_rev, "kind": "prose",
                "markdown": "# Second\nfinal decision"}]}),
    )
    .await
    .expect("returned revisions are usable for the next edit");
    let preserved = current_payload(&boot).await;
    assert_eq!(
        preserved
            .blocks
            .as_ref()
            .unwrap()
            .iter()
            .find(|b| b.id == task_id),
        Some(&task_before)
    );
    // A prose op cannot carry a task change in: an embedded fence is refused and nothing lands.
    let final_second = preserved
        .blocks
        .as_ref()
        .unwrap()
        .iter()
        .find(|b| b.id == second.id)
        .unwrap()
        .clone();
    let smuggled = calm_types::report_blocks::flat_text(&task_before)
        .replace("build it", "silently changed task");
    let err = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        json!({"if_doc_rev": preserved.doc_rev, "message": "must refuse task mutation",
            "ops": [{"op": "upsert", "id": second.id, "if_rev": final_second.rev, "kind": "prose",
                "markdown": format!("# Second\nfinal decision\n{smuggled}")}]}),
    )
    .await
    .expect_err("prose edit must still refuse a task payload change");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert_eq!(current_payload(&boot).await, preserved);
}

#[tokio::test]
async fn marked_import_and_block_move_keep_unterminated_prose_separate() {
    let boot = boot().await;
    let (task_id, _) = seed_planner_task(&boot, "build").await;
    let prose = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        planner_identity(&boot),
        json!({"kind": "prose", "markdown": "# Decision\nfirst draft",
            "if_doc_rev": read(&boot, json!({})).await["docRev"]}),
    )
    .await
    .unwrap();
    let snapshot = read(&boot, json!({})).await;
    let task_index = snapshot["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .position(|block| block["id"] == task_id)
        .unwrap();
    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_MOVE,
        planner_identity(&boot),
        json!({"id": prose["id"], "if_doc_rev": snapshot["docRev"],
            "to_index": task_index}),
    )
    .await
    .unwrap();
    let replacement = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        planner_identity(&boot),
        json!({"id": prose["id"], "if_rev": prose["rev"], "kind": "prose",
            "markdown": "# Decision\nsecond draft"}),
    )
    .await
    .unwrap();
    let before = current_payload(&boot).await;
    let task_before = before
        .blocks
        .as_ref()
        .unwrap()
        .iter()
        .find(|b| b.id == task_id)
        .unwrap()
        .clone();
    let marked = read(&boot, json!({"with_markers": true})).await;
    let out = call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        planner_identity(&boot),
        json!({"body": marked["text"].as_str().unwrap().replace("second draft", "final draft"),
            "if_doc_rev": marked["docRev"]}),
    )
    .await
    .expect("marked import remains usable");
    let after = current_payload(&boot).await;
    assert_eq!(
        after
            .blocks
            .as_ref()
            .unwrap()
            .iter()
            .find(|b| b.id == task_id),
        Some(&task_before)
    );
    let updated = after
        .blocks
        .as_ref()
        .unwrap()
        .iter()
        .find(|b| b.id == prose["id"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        u64::from(updated.rev),
        replacement["rev"].as_u64().unwrap() + 1
    );
    assert!(after.body.contains("final draft\n```neige-block task"));
    assert_eq!(out["docRev"].as_u64(), Some(after.doc_rev));
}
