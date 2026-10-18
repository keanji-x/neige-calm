//! Committed snapshots refresh only the caller's report knowledge.
#![cfg(unix)]
use crate::mcp_report_sections::{
    SECTIONS, commit, payload, read_sections, replace, section_blocks, seed,
};
use crate::mcp_track_report::{boot, planner_identity};
use serde_json::json;

#[tokio::test]
async fn own_created_block_keeps_section_anchor() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要"]).await;
    commit(
        &b,
        planner_identity(&b),
        json!({"ops":[{"op":"upsert","kind":"prose","markdown":"## new\n\nx\n","position":3}]}),
    )
    .await
    .unwrap();
    commit(
        &b,
        planner_identity(&b),
        replace("概要", "# 概要\n\nfinal\n"),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn own_moves_rebuild_section_anchors() {
    let b = boot().await;
    seed(
        &b,
        &SECTIONS.replace("a2\n\n# 已完成", "a2\n\n## 另\n\nextra\n\n# 已完成"),
    )
    .await;
    read_sections(&b, planner_identity(&b), &["概要", "已完成"]).await;
    let id = section_blocks(&b, "# 概要").await[1].0.clone();
    for to in [3, 4, 2] {
        commit(
            &b,
            planner_identity(&b),
            json!({"ops":[{"op":"move","id":id,"to_index":to}]}),
        )
        .await
        .unwrap();
        let read = b
            .ctx
            .read_ledger
            .last_read(&planner_identity(&b).session_id, b.report_card_id.as_str())
            .unwrap();
        for (title, heading) in [("概要", "# 概要"), ("已完成", "# 已完成")] {
            let expected: Vec<_> = section_blocks(&b, heading)
                .await
                .into_iter()
                .map(|(id, rev)| (id, rev as u32))
                .collect();
            assert_eq!(read.sections.get(title), Some(&expected));
        }
    }
}

#[tokio::test]
async fn section_replace_then_delete_section_uses_final_anchor() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要"]).await;
    let id = section_blocks(&b, "# 概要").await[1].0.clone();
    let markdown = format!("# 概要\n\nx\n\n<!-- neige:{id} -->\n## 细节\n\na2\n");
    commit(
        &b,
        planner_identity(&b),
        json!({"ops":[
            {"op":"replace","section":"概要","markdown":markdown},
            {"op":"move","id":id,"to_index":1}
        ]}),
    )
    .await
    .unwrap();
    commit(
        &b,
        planner_identity(&b),
        json!({"ops":[{"op":"delete","section":"概要"}]}),
    )
    .await
    .unwrap();
    assert!(!payload(&b).await.body.contains("# 概要"));
}

#[tokio::test]
async fn foreign_content_is_not_granted_by_an_unrelated_commit() {
    use crate::mcp_report_sections::read_full;
    use crate::mcp_track_report::assistant_identity;
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_full(&b).await;
    read_sections(&b, assistant_identity(&b), &["概要"]).await;
    commit(
        &b,
        assistant_identity(&b),
        replace("概要", "# 概要\n\nforeign\n"),
    )
    .await
    .unwrap();
    commit(&b, planner_identity(&b), replace("决策", "# 决策\n\nown\n"))
        .await
        .unwrap();
    let read = b
        .ctx
        .read_ledger
        .last_read(&planner_identity(&b).session_id, b.report_card_id.as_str())
        .unwrap();
    let foreign = section_blocks(&b, "# 概要").await[0].clone();
    assert_ne!(read.blocks.get(&foreign.0), Some(&(foreign.1 as u32)));
    assert!(!read.sections.contains_key("概要"));
    assert!(!read.whole);
    assert!(
        commit(
            &b,
            planner_identity(&b),
            replace("概要", "# 概要\n\nstolen\n")
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn foreign_move_of_known_blocks_does_not_grant_section() {
    use crate::mcp_report_sections::read_full;
    use crate::mcp_track_report::assistant_identity;
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_full(&b).await;
    read_sections(&b, assistant_identity(&b), &["概要", "已完成"]).await;
    let id = section_blocks(&b, "# 概要").await[1].0.clone();
    commit(
        &b,
        assistant_identity(&b),
        json!({"ops":[{"op":"move","id":id,"to_index":3}]}),
    )
    .await
    .unwrap();
    commit(&b, planner_identity(&b), replace("决策", "# 决策\n\nown\n"))
        .await
        .unwrap();
    let read = b
        .ctx
        .read_ledger
        .last_read(&planner_identity(&b).session_id, b.report_card_id.as_str())
        .unwrap();
    assert!(!read.sections.contains_key("概要"));
    assert!(!read.sections.contains_key("已完成"));
    assert!(!read.whole);
}

#[tokio::test]
async fn partial_read_commit_never_grants_whole() {
    use crate::mcp_track_report::call_tool;
    use calm_server::mcp_server::tools::track_report_blocks::TOOL_REPORT_WRITE;
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要"]).await;
    commit(&b, planner_identity(&b), replace("概要", "# 概要\n\nown\n"))
        .await
        .unwrap();
    let read = b
        .ctx
        .read_ledger
        .last_read(&planner_identity(&b).session_id, b.report_card_id.as_str())
        .unwrap();
    assert!(!read.whole);
    assert!(
        call_tool(
            &b,
            TOOL_REPORT_WRITE,
            planner_identity(&b),
            json!({"body":payload(&b).await.body})
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn failed_transaction_leaves_knowledge_unchanged_and_new_session_empty() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要", "已完成"]).await;
    let who = planner_identity(&b);
    let old = b
        .ctx
        .read_ledger
        .last_read(&who.session_id, b.report_card_id.as_str());
    let doc = payload(&b).await;
    assert!(
        commit(
            &b,
            who.clone(),
            json!({"ops":[
                {"op":"replace","section":"概要","markdown":"# 概要\n\nnot landed\n"},
                {"op":"replace","section":"已完成","markdown":"# wrong\n"}
            ]})
        )
        .await
        .is_err()
    );
    assert_eq!(
        b.ctx
            .read_ledger
            .last_read(&who.session_id, b.report_card_id.as_str()),
        old
    );
    assert_eq!(payload(&b).await.body, doc.body);
    assert_eq!(
        b.ctx
            .read_ledger
            .last_read("new-session", b.report_card_id.as_str()),
        None
    );
    assert_eq!(
        calm_server::report_read_ledger::ReadLedger::new()
            .last_read(&who.session_id, b.report_card_id.as_str()),
        None
    );
}

#[tokio::test]
async fn committed_write_refreshes_anchors_before_warning_lookup_failure() {
    let mut b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要"]).await;
    // The receipt pool is a production seam separate from the authoritative repository.
    let pool = calm_server::db::sqlite::SqliteDb::connect("sqlite::memory:")
        .await
        .unwrap();
    pool.close().await;
    let ctx = std::sync::Arc::get_mut(&mut b.ctx).expect("exclusive context");
    let original = ctx.sqlite_db.replace(pool);
    assert!(
        commit(
            &b,
            planner_identity(&b),
            replace("概要", "# 概要\n\ncommitted\n\n## final\n\nx\n")
        )
        .await
        .is_err()
    );
    assert!(payload(&b).await.body.contains("committed"));
    std::sync::Arc::get_mut(&mut b.ctx).unwrap().sqlite_db = original;
    commit(
        &b,
        planner_identity(&b),
        json!({"ops":[{"op":"delete","section":"概要"}]}),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn own_move_of_unread_content_grants_neither_block_nor_section() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["已完成"]).await;
    let id = section_blocks(&b, "# 概要").await[1].0.clone();
    commit(
        &b,
        planner_identity(&b),
        json!({"ops":[{"op":"move","id":id,"to_index":3}]}),
    )
    .await
    .unwrap();
    let read = b
        .ctx
        .read_ledger
        .last_read(&planner_identity(&b).session_id, b.report_card_id.as_str())
        .unwrap();
    assert!(!read.blocks.contains_key(&id));
    assert!(!read.sections.contains_key("已完成"));
    assert!(!read.whole);
}

#[tokio::test]
async fn known_content_outside_sections_needs_structural_evidence() {
    use crate::mcp_track_report::call_tool;
    use calm_server::mcp_server::tools::track_report::TOOL_REPORT_READ;
    let b = boot().await;
    seed(&b, &format!("## intro\n\nx\n\n{SECTIONS}")).await;
    read_sections(&b, planner_identity(&b), &["概要"]).await;
    let blocks = payload(&b).await.blocks.unwrap();
    let id = blocks
        .iter()
        .find(|block| {
            block.payload["markdown"]
                .as_str()
                .is_some_and(|s| s.starts_with("## intro"))
        })
        .unwrap()
        .id
        .clone();
    call_tool(
        &b,
        TOOL_REPORT_READ,
        planner_identity(&b),
        json!({"blocks":[id]}),
    )
    .await
    .unwrap();
    commit(
        &b,
        planner_identity(&b),
        json!({"ops":[{"op":"move","id":id,"to_index":3}]}),
    )
    .await
    .unwrap();
    let read = b
        .ctx
        .read_ledger
        .last_read(&planner_identity(&b).session_id, b.report_card_id.as_str())
        .unwrap();
    assert!(read.blocks.contains_key(&id));
    assert!(!read.sections.contains_key("概要"));
    assert!(!read.whole);
}

#[tokio::test]
async fn newly_authored_section_has_its_final_anchor() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要"]).await;
    commit(
        &b,
        planner_identity(&b),
        json!({"ops":[{"op":"upsert","kind":"prose","markdown":"# 待你定\n\nown\n","position":3}]}),
    )
    .await
    .unwrap();
    commit(
        &b,
        planner_identity(&b),
        replace("待你定", "# 待你定\n\nfinal\n"),
    )
    .await
    .unwrap();
}

/// Whole-section output must not certify untouched members of its D0 source by title.
async fn section_output_does_not_certify_foreign_members(variant: &str) {
    use crate::mcp_track_report::assistant_identity;
    let b = boot().await;
    seed(
        &b,
        "# 概要\n\na\n\n# 已完成\n\nb\n\n## x\n\nx\n\n## y\n\ny\n",
    )
    .await;
    read_sections(&b, planner_identity(&b), &["概要", "已完成"]).await;
    let members = section_blocks(&b, "# 已完成").await;
    let x = &members[1].0;
    let y = &members[2].0;
    // Both moves preserve content and revisions but invalidate both D0 section certificates.
    commit(
        &b,
        assistant_identity(&b),
        json!({"ops":[
            {"op":"move","id":x,"to_index":2},
            {"op":"move","id":y,"to_index":3}
        ]}),
    )
    .await
    .unwrap();
    if variant == "merge_after_replace" {
        // Certify the unaffected merge donor, so only A's untouched members lack provenance.
        read_sections(&b, planner_identity(&b), &["已完成"]).await;
    }
    let mut ops = vec![
        json!({"op":"upsert","id":x,"kind":"prose","markdown":"# 待你定\n\nx\n"}),
        json!({"op":"replace","section":"概要","markdown":"# 概要\n\nown\n"}),
    ];
    let target = match variant {
        "repeated_replace" => {
            ops.push(json!({"op":"replace","section":"概要","markdown":"# 概要\n\nown again\n"}));
            "待你定"
        }
        "merge_after_replace" => {
            ops.push(
                json!({"op":"upsert","id":members[0].0,"kind":"prose","markdown":"## b\n\nb\n"}),
            );
            "待你定"
        }
        _ => "待你定",
    };
    let committed = commit(&b, planner_identity(&b), json!({"ops":ops})).await;
    if variant == "repeated_replace" {
        assert!(
            committed
                .unwrap_err()
                .message
                .contains("each section may appear at most once")
        );
        return;
    }
    committed.unwrap();
    let read = b
        .ctx
        .read_ledger
        .last_read(&planner_identity(&b).session_id, b.report_card_id.as_str())
        .unwrap();
    assert!(
        read.blocks.contains_key(x) && read.blocks.contains_key(y),
        "{variant}: content known"
    );
    assert!(
        !read.sections.contains_key(target),
        "{variant}: untouched y has no structural witness"
    );
    for op in [
        json!({"op":"delete","section":target}),
        json!({"op":"replace","section":target,"markdown":format!("# {target}\n\nstolen\n")}),
    ] {
        assert!(
            commit(&b, planner_identity(&b), json!({"ops":[op]}))
                .await
                .is_err(),
            "{variant}: requires a read"
        );
    }
    read_sections(&b, planner_identity(&b), &[target]).await;
    commit(
        &b,
        planner_identity(&b),
        json!({"ops":[{"op":"delete","section":target}]}),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn section_output_does_not_certify_split_foreign_members() {
    section_output_does_not_certify_foreign_members("split").await;
}

#[tokio::test]
async fn repeated_section_ops_are_rejected_before_provenance_projection() {
    section_output_does_not_certify_foreign_members("repeated_replace").await;
}

#[tokio::test]
async fn section_output_does_not_certify_merged_foreign_members() {
    section_output_does_not_certify_foreign_members("merge_after_replace").await;
}

#[tokio::test]
async fn section_output_keeps_provenance_after_heading_edit() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要"]).await;
    let id = section_blocks(&b, "# 概要").await[0].0.clone();
    let marked = format!("<!-- neige:{id} -->\n# 概要\n\nalpha\n\n");
    commit(
        &b,
        planner_identity(&b),
        json!({"ops":[
            {"op":"replace","section":"概要","markdown":marked},
            {"op":"upsert","id":id,"kind":"prose","markdown":"# 待你定\n\nrenamed\n"}
        ]}),
    )
    .await
    .unwrap();
    commit(
        &b,
        planner_identity(&b),
        json!({"ops":[{"op":"delete","section":"待你定"}]}),
    )
    .await
    .unwrap();
}
