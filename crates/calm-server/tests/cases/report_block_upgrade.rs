//! Upgrade and empty-block boundary regressions.
use super::*;

async fn edit_preupgrade_report(missing_cache: bool, boundary_match: bool) {
    let boot = boot().await;
    for markdown in ["# First\nlocal prose", "# Second\noriginal decision"] {
        upsert_block(
            &boot,
            planner_identity(&boot),
            json!({"kind": "prose", "markdown": markdown}),
        )
        .await
        .unwrap();
    }
    let (task_id, _) = seed_planner_task(&boot, "build").await;
    let stored = current_payload(&boot).await;
    let blocks = stored.blocks.as_ref().unwrap();
    let task_before = blocks.iter().find(|b| b.id == task_id).unwrap().clone();
    // Upsert stores exact independent block bytes in the real CRDT. Only the
    // derived body cache differs in the pre-upgrade writer; reproduce that
    // old cache without changing the authoritative blob or its revision.
    assert!(
        blocks
            .iter()
            .any(|b| b.payload["markdown"] == "# Second\noriginal decision")
    );
    let raw: String = blocks
        .iter()
        .map(calm_types::report_blocks::flat_text)
        .collect();
    assert!(raw.contains("original decision```neige-block task"));
    let mut cache = serde_json::to_value(&stored).unwrap();
    cache["body"] = json!(raw);
    if missing_cache {
        cache.as_object_mut().unwrap().remove("blocks");
    }
    overwrite_report_payload_cache(&boot, cache).await;
    let before = boot
        .repo
        .card_get_with_body_crdt(boot.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert!(
        before.1.is_some(),
        "upgrade fixture must retain a real persisted CRDT"
    );
    let snapshot = read(&boot, json!({})).await;
    assert!(
        snapshot["text"]
            .as_str()
            .unwrap()
            .contains("original decision\n```neige-block task")
    );
    let after_read = boot
        .repo
        .card_get_with_body_crdt(boot.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        after_read.1, before.1,
        "read cannot repair storage as a side effect"
    );
    assert_eq!(after_read.0.payload, before.0.payload);
    // The boundary variant edits the block that ends right before `# Second`; the other the one before the task fence.
    let (old, new, boundary) = if boundary_match {
        (
            "# First\nlocal prose",
            "# First\nupdated prose",
            "updated prose\n# Second",
        )
    } else {
        (
            "# Second\noriginal decision",
            "# Second\nrevised decision",
            "revised decision\n```neige-block task",
        )
    };
    let target = blocks
        .iter()
        .find(|b| b.payload["markdown"] == old)
        .expect("the edited prose block")
        .clone();
    let edited = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        json!({ "message": "edit pre-upgrade report",
            "ops": [{"op": "upsert", "id": target.id, "kind": "prose", "markdown": new}]}),
    )
    .await
    .expect("edit must land on the CRDT truth, not the obsolete body cache");
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
    assert!(after.body.contains(boundary), "{}", after.body);
    assert_eq!(edited["doc_rev"].as_u64(), Some(after.doc_rev));
    call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        json!({ "message": "continue from the own write",
            "summary": "reused"}),
    )
    .await
    .unwrap();
    assert_eq!(task_keys(&boot).await, ["build"]);
}

#[tokio::test]
async fn preupgrade_cached_body_prose_edit() {
    edit_preupgrade_report(false, false).await;
}
#[tokio::test]
async fn preupgrade_missing_cache_prose_edit() {
    edit_preupgrade_report(true, false).await;
}
#[tokio::test]
async fn preupgrade_cached_body_boundary_edit() {
    edit_preupgrade_report(false, true).await;
}
#[tokio::test]
async fn preupgrade_missing_cache_boundary_edit() {
    edit_preupgrade_report(true, true).await;
}

#[tokio::test]
async fn empty_blocks_have_equal_plain_and_stripped_marked_projections() {
    // A real trailing empty block gets a structural line boundary. This
    // intentionally supersedes R1's promise to ignore empties at EOF.
    for (parts, suffix) in [
        (vec!["a", ""], "a\n"),
        (vec!["a", "", ""], "a\n"),
        (vec!["", "a"], "a"),
        (vec!["a", "", "b"], "a\nb"),
        (vec!["", ""], ""),
    ] {
        let boot = boot().await;
        for text in &parts {
            upsert_block(
                &boot,
                planner_identity(&boot),
                json!({"kind": "prose", "markdown": text}),
            )
            .await
            .unwrap();
        }
        let before = current_payload(&boot).await;
        let plain = read(&boot, json!({})).await;
        assert_eq!(
            plain["text"].as_str().unwrap(),
            format!("{}{suffix}", seed_body())
        );
        let marked = read(&boot, json!({"with_markers": true})).await;
        let stripped =
            calm_types::report_blocks::strip_markers_and_split(marked["text"].as_str().unwrap());
        assert_eq!(
            stripped.cleaned,
            plain["text"].as_str().unwrap(),
            "parts={parts:?}"
        );
        assert_eq!(
            current_payload(&boot).await,
            before,
            "rendering must not mutate empty ids or stored content"
        );
        read_then_write_markdown(
            &boot,
            planner_identity(&boot),
            json!({"body": marked["text"]}),
        )
        .await
        .unwrap();
        assert_eq!(
            current_payload(&boot).await.body,
            plain["text"].as_str().unwrap(),
            "unchanged marked import must preserve projected body including EOF"
        );
    }
}
