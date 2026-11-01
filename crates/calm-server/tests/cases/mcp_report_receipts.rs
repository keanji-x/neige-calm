//! Bounded report mutation receipts and immutable, non-authorizing evidence reads.
use crate::mcp_report_sections::{SECTIONS, commit, read_sections, replace, seed};
use crate::mcp_track_report::{
    Boot, assistant_identity, boot, call_tool, planner_identity, read_then_write_markdown,
};
use serde_json::{Value, json};

async fn index_page(b: &Boot, args: Value) -> Value {
    call_tool(b, "neige_report_read", planner_identity(b), args)
        .await
        .unwrap()
}

#[tokio::test]
async fn large_section_receipt_has_bounded_preview_and_immutable_complete_evidence() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要"]).await;
    let markdown = format!(
        "# 概要\n\nchanged\n{}",
        (0..110)
            .map(|n| format!("\n## part {n}\n\nnew {n}\n"))
            .collect::<String>()
    );
    let receipt = commit(&b, planner_identity(&b), replace("概要", &markdown))
        .await
        .unwrap();
    assert!(receipt["changes_total"].as_u64().unwrap() > 100);
    assert_eq!(receipt["changes_complete"], false);
    assert_eq!(receipt["changes"].as_array().unwrap().len(), 32);
    assert!(receipt["changes"].to_string().len() <= 8 * 1024);
    let known_before = calm_truth::db::sqlite::report_knowledge::load(
        b.repo.as_ref(),
        &planner_identity(&b).session_id,
        b.report_card_id.as_str(),
    )
    .await
    .unwrap();
    let mut args = receipt["full_index"]["arguments"].clone();
    let first = index_page(&b, args.clone()).await;
    assert_eq!(first["knowledge_granted"], false);
    assert_eq!(first["doc_rev"], receipt["doc_rev"]);
    assert!(!first["complete"].as_bool().unwrap());
    let mut all = first["blocks"].as_array().unwrap().clone();
    let pinned = first["commit"].clone();
    // Another writer changes live content between pages; the index must stay pinned.
    read_sections(&b, assistant_identity(&b), &["概要"]).await;
    commit(
        &b,
        assistant_identity(&b),
        replace("概要", "# 概要\n\nforeign\n"),
    )
    .await
    .unwrap();
    let mut next = first["next_cursor"].clone();
    while !next.is_null() {
        args["cursor"] = next;
        let page = index_page(&b, args.clone()).await;
        assert_eq!(page["commit"], pinned);
        assert_eq!(page["doc_rev"], receipt["doc_rev"]);
        assert!(page["blocks"].to_string().len() <= 8 * 1024);
        all.extend(page["blocks"].as_array().unwrap().clone());
        next = page["next_cursor"].clone();
    }
    assert_eq!(all.len(), first["blocks_total"].as_u64().unwrap() as usize);
    let ids = all
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(ids.len(), all.len(), "every block exactly once");
    assert!(all.len() > 100);
    assert_eq!(
        calm_truth::db::sqlite::report_knowledge::load(
            b.repo.as_ref(),
            &planner_identity(&b).session_id,
            b.report_card_id.as_str()
        )
        .await
        .unwrap(),
        known_before
    );
    let before = index_page(
        &b,
        json!({"detail":"index","commit":receipt["history"]["before"]}),
    )
    .await;
    assert!(before["blocks_total"].as_u64().unwrap() < 10);
    let mut wrong = receipt["full_index"]["arguments"].clone();
    wrong["cursor"] = json!(format!("wrong:{}", all[0]["id"].as_str().unwrap()));
    assert_eq!(
        call_tool(&b, "neige_report_read", planner_identity(&b), wrong)
            .await
            .unwrap_err()
            .code,
        -32602
    );
}

#[tokio::test]
async fn receipt_reports_equal_writes_explicit_moves_deletions_and_created_ids() {
    let b = boot().await;
    read_then_write_markdown(
        &b,
        planner_identity(&b),
        json!({"body":"# A\n\nalpha\n\n# B\n\nbeta\n\n# C\n\ngamma\n"}),
    )
    .await
    .unwrap();
    let read = index_page(&b, json!({})).await;
    let id = read["blocks"][1]["id"].clone();
    let equal = commit(
        &b,
        planner_identity(&b),
        json!({"ops":[{"op":"upsert","id":id,"kind":"prose","markdown":"# B\n\nbeta\n"}]}),
    )
    .await
    .unwrap();
    assert_eq!(equal["changes_total"], 1);
    assert_eq!(
        equal["changes"][0]["rev"],
        equal["changes"][0]["previous_rev"]
    );
    let moved = commit(
        &b,
        planner_identity(&b),
        json!({"ops":[{"op":"move","id":id,"to_index":0}]}),
    )
    .await
    .unwrap();
    assert_eq!(
        moved["changes_total"], 1,
        "shifted siblings are not authored results"
    );
    assert_eq!(moved["changes"][0]["id"], id);
    assert_eq!(moved["changes"][0]["moved"], true);
    assert_eq!(moved["changes"][0]["position"], 0);
    let removed = commit(
        &b,
        planner_identity(&b),
        json!({"ops":[{"op":"delete","id":id}]}),
    )
    .await
    .unwrap();
    assert_eq!(removed["changes_total"], 1);
    assert_eq!(removed["changes"][0]["id"], id);
    assert!(removed["changes"][0]["rev"].is_null());
    assert_eq!(removed["changes"][0]["previous_rev"], 1);
    let created = commit(
        &b,
        planner_identity(&b),
        json!({"ops":[{"op":"upsert","kind":"prose","markdown":"# new\n\nx\n","position":0}]}),
    )
    .await
    .unwrap();
    assert_eq!(created["changes_total"], 1);
    assert!(created["changes"][0]["previous_rev"].is_null());
    let fresh = created["changes"][0]["id"].clone();
    commit(
        &b,
        planner_identity(&b),
        json!({"ops":[{"op":"upsert","id":fresh,"kind":"prose","markdown":"# new\n\ny\n"}]}),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn historical_indexes_grant_no_knowledge_and_reject_other_tracks_or_missing_history() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    let db = b.repo.sqlite_db().unwrap();
    let head = calm_truth::track_vcs::head(db.read_pool(), &b.track_id)
        .await
        .unwrap()
        .unwrap();
    let args = json!({"detail":"index","commit":head});
    let page = index_page(&b, args.clone()).await;
    assert_eq!(page["knowledge_granted"], false);
    assert!(
        calm_truth::db::sqlite::report_knowledge::load(
            b.repo.as_ref(),
            &planner_identity(&b).session_id,
            b.report_card_id.as_str()
        )
        .await
        .unwrap()
        .is_none()
    );
    let id = page["blocks"][0]["id"].clone();
    let refused = commit(
        &b,
        planner_identity(&b),
        json!({"ops":[{"op":"delete","id":id}]}),
    )
    .await
    .unwrap_err();
    assert_eq!(refused.data.unwrap()["reason_code"], "never_read");
    for invalid in [
        json!({"commit":head}),
        json!({"detail":"index","commit":head,"blocks":[id]}),
        json!({"detail":"index","cursor":"x"}),
    ] {
        assert_eq!(
            call_tool(&b, "neige_report_read", planner_identity(&b), invalid)
                .await
                .unwrap_err()
                .code,
            -32602
        );
    }
    let other = b
        .repo
        .track_create(calm_server::model::NewTrack {
            template_input: None,
            area_id: b.area_id.clone(),
            title: "other".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::model::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let foreign_track = other.id.to_string();
    let source = head.clone();
    // A foreign commit containing the SAME payload path defeats path-only authorization.
    calm_server::db::write_in_tx_typed(b.repo.as_ref(), move |tx| {
        Box::pin(async move {
            sqlx::query(concat!(
                "INSERT INTO track_vcs_commits(hash,track_id,parent_hash,tree_hash,",
                "manifest_schema_version,event_id,created_at) ",
                "SELECT 'foreign-receipt',?,NULL,tree_hash,manifest_schema_version,NULL,",
                "created_at FROM track_vcs_commits WHERE hash=?",
            ))
            .bind(foreign_track)
            .bind(source)
            .execute(&mut **tx)
            .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    for commit in ["foreign-receipt", "missing-receipt"] {
        let error = call_tool(
            &b,
            "neige_report_read",
            planner_identity(&b),
            json!({"detail":"index","commit":commit}),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, -32404);
    }
    let old = planner_identity(&b);
    crate::mcp_report_sections::supersede_planner(&b, &old.session_id, "receipt-successor").await;
    assert_eq!(
        call_tool(&b, "neige_report_read", old, args)
            .await
            .unwrap_err()
            .code,
        -32403
    );
}

#[tokio::test]
async fn mutation_receipt_growth_depends_on_changes_not_report_size() {
    use crate::mcp_track_report::call_tool_raw;
    let mut sizes = Vec::new();
    for count in [10, 300] {
        let b = boot().await;
        let body = (0..count)
            .map(|n| format!("# Section {n}\n\ntext {n}\n"))
            .collect::<String>();
        read_then_write_markdown(&b, planner_identity(&b), json!({"body":body}))
            .await
            .unwrap();
        let read = index_page(&b, json!({})).await;
        let id = read["blocks"][0]["id"].clone();
        let wire = call_tool_raw(&b, "neige_report_commit", planner_identity(&b), json!({
            "message":"edit one block", "ops":[{"op":"upsert", "id":id, "kind":"prose", "markdown":"# Section 0\n\nupdated\n"}]
        })).await.unwrap();
        let receipt = &wire["structuredContent"];
        assert!(
            receipt.get("blocks").is_none(),
            "a mutation is not a full index"
        );
        assert_eq!(receipt["changes_total"], 1);
        assert_eq!(receipt["changes_complete"], true);
        assert_eq!(receipt["changes"][0]["id"], id);
        assert_eq!(receipt["changes"][0]["rev"], 2);
        assert_eq!(receipt["changes"][0]["previous_rev"], 1);
        let text = wire["content"][0]["text"].as_str().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(text).unwrap(),
            *receipt,
            "content-only clients must receive the complete receipt"
        );
        eprintln!(
            "report_blocks={count} receipt_payload_bytes={} serialized_result_bytes={} content_text_bytes={}",
            receipt.to_string().len(),
            wire.to_string().len(),
            text.len()
        );
        sizes.push(text.len());
    }
    assert!(sizes.iter().all(|size| *size < 2048), "{sizes:?}");
    assert!(sizes[1].abs_diff(sizes[0]) < 128, "{sizes:?}");
}
