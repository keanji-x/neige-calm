//! Upgrade and empty-block boundary regressions from #1501 independent review.
use super::*;

async fn edit_preupgrade_report(missing_cache: bool, boundary_match: bool) {
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
    let (old, new) = if boundary_match {
        ("local prose\n# Second", "updated prose\n# Second")
    } else {
        ("original decision", "revised decision")
    };
    let edited = call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        planner_identity(&boot),
        json!({"old_string": old, "new_string": new, "if_doc_rev": snapshot["docRev"],
            "message": "edit pre-upgrade report"}),
    )
    .await
    .expect("edit must use the projected read body");
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
    assert!(after.body.contains(new));
    assert_eq!(edited["docRev"].as_u64(), Some(after.doc_rev));
    let stale = call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        planner_identity(&boot),
        json!({"old_string": old, "new_string": "stale", "if_doc_rev": snapshot["docRev"],
            "message": "stale snapshot"}),
    )
    .await
    .expect_err("stale revision must refuse even if old text is absent");
    assert_eq!(stale.code, RPC_REV_CONFLICT);
    assert_eq!(current_payload(&boot).await, after);
    call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        planner_identity(&boot),
        json!({"old_string": new, "new_string": new, "if_doc_rev": edited["docRev"],
            "message": "reuse returned revision"}),
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
            call_tool(
                &boot,
                TOOL_REPORT_BLOCKS_UPSERT,
                planner_identity(&boot),
                json!({"kind": "prose", "markdown": text,
                    "if_doc_rev": read(&boot, json!({})).await["docRev"]}),
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
        call_tool(
            &boot,
            TOOL_REPORT_WRITE_MARKDOWN,
            planner_identity(&boot),
            json!({"body": marked["text"], "if_doc_rev": marked["docRev"]}),
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
