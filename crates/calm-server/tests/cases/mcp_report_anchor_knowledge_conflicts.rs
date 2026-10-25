//! Structured refusals are advice, never read certificates.
#![cfg(unix)]
use crate::mcp_report_sections::{SECTIONS, commit, section_blocks, seed};
use crate::mcp_track_report::{boot, planner_identity};
use serde_json::json;

#[tokio::test]
async fn never_read_has_typed_scope_and_reread_retry() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    let id = section_blocks(&b, "# 概要").await[0].0.clone();
    let before = state(&b).await;
    let err = commit(
        &b,
        planner_identity(&b),
        json!({"ops":[{"op":"delete","id":id}]}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32602);
    let data = err.data.expect("Report rejection must carry typed advice");
    assert_eq!(data["reason_code"], "never_read");
    assert_eq!(
        data["target"],
        json!({"report_card_id":b.report_card_id,"block_ids":[id],"sections":[]})
    );
    assert_eq!(
        data["next_step"],
        json!({"action":"read","blocks":[id],"sections":[]})
    );
    assert_eq!(state(&b).await, before);
    let again = commit(
        &b,
        planner_identity(&b),
        json!({"ops":[{"op":"delete","id":id}]}),
    )
    .await
    .unwrap_err();
    assert_eq!(again.code, -32602);
    assert_eq!(state(&b).await, before);
    reread(&b, &data).await;
    commit(
        &b,
        planner_identity(&b),
        json!({"ops":[{"op":"delete","id":id}]}),
    )
    .await
    .unwrap();
}

use crate::mcp_report_sections::{payload, read_full, read_sections, replace, supersede_planner};
use crate::mcp_track_report::{Boot, assistant_identity, call_tool};
use calm_server::mcp_server::tools::{
    track_report::TOOL_REPORT_READ, track_report_blocks::TOOL_REPORT_WRITE,
};
use serde_json::Value;

async fn state(b: &Boot) -> Value {
    let db = b.repo.sqlite_db().unwrap();
    let history: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM track_vcs_commits), \
         (SELECT count(*) FROM track_vcs_objects), \
         (SELECT count(*) FROM events)",
    )
    .fetch_one(db.read_pool())
    .await
    .unwrap();
    let head = calm_truth::track_vcs::head(db.read_pool(), &b.track_id)
        .await
        .unwrap();
    let card = b
        .repo
        .card_get(b.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let ledger = calm_truth::db::sqlite::report_knowledge::load(
        b.repo.as_ref(),
        &planner_identity(b).session_id,
        b.report_card_id.as_str(),
    )
    .await
    .unwrap();
    json!({"history":history,"head":head,"card":card,"ledger":ledger})
}

async fn reread(b: &Boot, data: &Value) {
    let next = &data["next_step"];
    assert_eq!(next["action"], "read");
    let args = if next["mode"] == "full" {
        json!({})
    } else if next["sections"].as_array().is_some_and(|s| !s.is_empty()) {
        json!({"sections":next["sections"]})
    } else {
        json!({"blocks":next["blocks"]})
    };
    call_tool(b, TOOL_REPORT_READ, planner_identity(b), args)
        .await
        .unwrap();
}

#[tokio::test]
async fn section_changes_add_modify_delete_nulls_and_advice_does_not_certify() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要"]).await;
    let old = section_blocks(&b, "# 概要").await;
    // Preserve the heading, change a member, add another, and remove the old detail.
    commit(
        &b,
        assistant_identity(&b),
        json!({"ops":[
            {"op":"upsert","id":old[0].0,"kind":"prose","markdown":"# 概要\n\nchanged\n"},
            {"op":"delete","id":old[1].0},
            {"op":"upsert","kind":"prose","markdown":"## added\n\nnew\n","position":2}
        ]}),
    )
    .await
    .unwrap();
    let current = section_blocks(&b, "# 概要").await;
    let before = state(&b).await;
    for attempt in 0..2 {
        let err = commit(
            &b,
            planner_identity(&b),
            replace("概要", "# 概要\n\nours\n"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, -32409);
        let d = err.data.unwrap();
        assert_eq!(d["reason_code"], "changed_since");
        assert_eq!(d["doc_rev"], payload(&b).await.doc_rev);
        assert_eq!(
            d["target"],
            json!({"report_card_id":b.report_card_id,"block_ids":[],"sections":["概要"]})
        );
        assert_eq!(
            d["next_step"],
            json!({"action":"read","blocks":[],"sections":["概要"]})
        );
        assert_eq!(
            d["changed"],
            json!([
                {"id":old[0].0,"known_rev":old[0].1,"current_rev":current[0].1},
                {"id":current[1].0,"known_rev":null,"current_rev":current[1].1},
                {"id":old[1].0,"known_rev":old[1].1,"current_rev":null}
            ])
        );
        assert_eq!(state(&b).await, before);
        if attempt == 1 {
            reread(&b, &d).await;
        }
    }
    commit(
        &b,
        planner_identity(&b),
        replace("概要", "# 概要\n\nours\n"),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn structural_only_section_conflict_requires_section_read() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要"]).await;
    let id = section_blocks(&b, "# 概要").await[1].0.clone();
    commit(
        &b,
        assistant_identity(&b),
        json!({"ops":[{"op":"move","id":id,"to_index":1}]}),
    )
    .await
    .unwrap();
    let err = commit(
        &b,
        planner_identity(&b),
        replace("概要", "# 概要\n\nours\n"),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32409);
    let d = err.data.unwrap();
    assert_eq!(d["changed"], json!([]));
    assert_eq!(d["next_step"]["sections"], json!(["概要"]));
    reread(&b, &d).await;
    commit(
        &b,
        planner_identity(&b),
        replace("概要", "# 概要\n\nours\n"),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn whole_required_and_stale_whole_require_full_but_doc_only_needs_local_text() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要"]).await;
    let body = payload(&b).await.body;
    let err = call_tool(
        &b,
        TOOL_REPORT_WRITE,
        planner_identity(&b),
        json!({"body":body}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32602);
    let d = err.data.unwrap();
    assert_eq!(d["reason_code"], "whole_required");
    assert_eq!(d["next_step"]["mode"], "full");
    reread(&b, &d).await;
    commit(&b, assistant_identity(&b), json!({"summary":"foreign"}))
        .await
        .unwrap();
    let err = call_tool(
        &b,
        TOOL_REPORT_WRITE,
        planner_identity(&b),
        json!({"body":body}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32409);
    let d = err.data.unwrap();
    assert_eq!(d["reason_code"], "changed_since");
    assert_eq!(d["next_step"]["mode"], "full");
    assert_eq!(d["changed"], json!([]));
    let err = commit(&b, planner_identity(&b), json!({"summary":"ours"}))
        .await
        .unwrap_err();
    let d = err.data.unwrap();
    assert_eq!(d["next_step"]["blocks"].as_array().unwrap().len(), 1);
    assert!(d["next_step"]["mode"].is_null());
    reread(&b, &d).await;
    commit(&b, planner_identity(&b), json!({"summary":"ours"}))
        .await
        .unwrap();
    assert_eq!(
        call_tool(
            &b,
            TOOL_REPORT_WRITE,
            planner_identity(&b),
            json!({"body":body})
        )
        .await
        .unwrap_err()
        .code,
        -32602
    );
}

#[tokio::test]
async fn deleted_block_and_disappeared_section_give_recoverable_advice() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要"]).await;
    let old = section_blocks(&b, "# 概要").await;
    commit(
        &b,
        assistant_identity(&b),
        json!({"ops":[{"op":"delete","section":"概要"}]}),
    )
    .await
    .unwrap();
    let err = commit(
        &b,
        planner_identity(&b),
        json!({"ops":[{"op":"delete","id":old[1].0}]}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32409);
    let d = err.data.unwrap();
    assert_eq!(
        d["changed"],
        json!([{"id":old[1].0,"known_rev":old[1].1,"current_rev":null}])
    );
    assert_eq!(d["next_step"]["action"], "index/revise_target");
    let err = commit(&b, planner_identity(&b), replace("概要", "# 概要\n\nnew\n"))
        .await
        .unwrap_err();
    assert_eq!(err.code, -32409);
    let d = err.data.unwrap();
    assert_eq!(d["next_step"]["mode"], "full");
    reread(&b, &d).await;
    commit(&b, planner_identity(&b), replace("概要", "# 概要\n\nnew\n"))
        .await
        .unwrap();
}

#[tokio::test]
async fn superseded_admission_never_exposes_report_evidence() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_full(&b).await;
    let old = planner_identity(&b);
    let successor = supersede_planner(&b, &old.session_id, "s3-successor").await;
    let before = state(&b).await;
    let err = commit(&b, old, json!({"ops":[{"op":"delete","id":"secret-task"}]}))
        .await
        .unwrap_err();
    assert_eq!(err.code, -32403);
    assert_eq!(err.data, Some(json!({"reason_code":"superseded_session"})));
    assert_eq!(state(&b).await, before);
    assert_eq!(
        commit(&b, successor, replace("概要", "# 概要\n\nx\n"))
            .await
            .unwrap_err()
            .code,
        -32602
    );
}

#[tokio::test]
async fn unknown_target_is_unread_but_database_failure_is_internal() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    let err = commit(
        &b,
        planner_identity(&b),
        json!({"ops":[{"op":"delete","id":"unknown-task"}]}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32602);
    let d = err.data.unwrap();
    assert_eq!(d["reason_code"], "never_read");
    assert_eq!(d["changed"], json!([]));
    assert_eq!(d["next_step"]["action"], "index/revise_target");
    read_sections(&b, planner_identity(&b), &["概要"]).await;
    let db = b.repo.sqlite_db().unwrap();
    sqlx::query("DROP TABLE report_read_ledger_blocks")
        .execute(db.writer_pool())
        .await
        .unwrap();
    let err = commit(&b, planner_identity(&b), json!({"summary":"ours"}))
        .await
        .unwrap_err();
    assert_eq!(err.code, -32603);
    assert!(
        err.data.is_none(),
        "storage failure must not masquerade as never_read"
    );
}

#[tokio::test]
async fn rejected_batch_freezes_step_evidence_and_rolls_back_earlier_writes() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要", "决策"]).await;
    let changed = section_blocks(&b, "# 概要").await[0].clone();
    let own = section_blocks(&b, "# 决策").await[0].0.clone();
    commit(&b,assistant_identity(&b),json!({"ops":[{"op":"upsert","id":changed.0,"kind":"prose","markdown":"# 概要\n\nforeign\n"}]})).await.unwrap();
    let before = state(&b).await;
    let args = json!({"ops":[
        {"op":"upsert","id":own,"kind":"prose","markdown":"# 决策\n\ntemporary own\n"},
        {"op":"delete","id":changed.0}
    ]});
    let err = commit(&b, planner_identity(&b), args.clone())
        .await
        .unwrap_err();
    assert_eq!(err.code, -32409);
    let d = err.data.unwrap();
    assert_eq!(d["target"]["block_ids"], json!([changed.0]));
    assert_eq!(
        d["changed"],
        json!([{"id":changed.0,"known_rev":changed.1,"current_rev":changed.1+1}])
    );
    assert_eq!(state(&b).await, before);
    reread(&b, &d).await;
    commit(&b, planner_identity(&b), args).await.unwrap();
}

#[tokio::test]
async fn ambiguous_section_advises_index_without_certifying_members() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要"]).await;
    commit(
        &b,
        assistant_identity(&b),
        json!({"ops":[{"op":"upsert","kind":"prose","markdown":"# 概要\n\nduplicate\n"}]}),
    )
    .await
    .unwrap();
    let before = state(&b).await;
    let err = commit(
        &b,
        planner_identity(&b),
        replace("概要", "# 概要\n\nours\n"),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32409);
    let d = err.data.unwrap();
    assert_eq!(d["reason_code"], "changed_since");
    assert_eq!(d["next_step"]["action"], "index/revise_target");
    assert_eq!(state(&b).await, before);
}

#[tokio::test]
async fn whole_advice_hides_unread_task_revisions_and_never_grants_task_knowledge() {
    use crate::mcp_track_report::upsert_block;
    use crate::mcp_track_report_blocks::planner_task_payload;
    let b = boot().await;
    seed(&b, SECTIONS).await;
    let task=upsert_block(&b,planner_identity(&b),json!({"kind":"task","payload":planner_task_payload("private-task", "private goal never returned in conflict")})).await.unwrap();
    let who = supersede_planner(&b, &planner_identity(&b).session_id, "s3-private-successor").await;
    let body = payload(&b).await.body;
    let err = call_tool(&b, TOOL_REPORT_WRITE, who.clone(), json!({"body":body}))
        .await
        .unwrap_err();
    assert_eq!(err.code, -32602);
    let d = err.data.unwrap();
    assert_eq!(d["reason_code"], "whole_required");
    assert!(
        !d["changed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == task["id"]),
        "whole advice must exclude unread task revisions"
    );
    assert!(!d.to_string().contains("private goal"));
    call_tool(&b, TOOL_REPORT_READ, who.clone(), json!({}))
        .await
        .unwrap();
    call_tool(&b, TOOL_REPORT_READ, assistant_identity(&b), json!({}))
        .await
        .unwrap();
    commit(&b, assistant_identity(&b), json!({"summary":"foreign"}))
        .await
        .unwrap();
    let err = call_tool(&b, TOOL_REPORT_WRITE, who.clone(), json!({"body":body}))
        .await
        .unwrap_err();
    assert_eq!(err.code, -32409);
    let d = err.data.unwrap();
    assert_eq!(d["changed"], json!([]));
    assert_eq!(d["next_step"]["mode"], "full");
    let known = calm_truth::db::sqlite::report_knowledge::load(
        b.repo.as_ref(),
        &who.session_id,
        b.report_card_id.as_str(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!known.blocks.contains_key(task["id"].as_str().unwrap()));
    let err = commit(&b, who, json!({"ops":[{"op":"delete","id":task["id"]}]}))
        .await
        .unwrap_err();
    assert_eq!(err.code, -32602);
    assert_eq!(err.data.unwrap()["reason_code"], "never_read");
}

#[tokio::test]
async fn full_empty_text_refreshes_document_anchor_without_certifying_tasks() {
    use crate::mcp_track_report::upsert_block;
    use crate::mcp_track_report_blocks::planner_task_payload;
    let b = boot().await;
    seed(&b, SECTIONS).await;
    let _created=upsert_block(&b,planner_identity(&b),json!({"kind":"task","payload":planner_task_payload("private-empty-task", "private empty goal")})).await.unwrap();
    let who = supersede_planner(&b, &planner_identity(&b).session_id, "s3-empty-successor").await;
    // A legacy Report with only a task has no renderable full text. Use its real task block.
    let mut p = payload(&b).await;
    p.blocks.as_mut().unwrap().retain(|b| b.kind == "task");
    p.body =
        calm_types::report_blocks::render_fence("task", &p.blocks.as_ref().unwrap()[0].payload);
    let db = b.repo.sqlite_db().unwrap();
    sqlx::query("UPDATE cards SET payload=?, body_crdt=NULL WHERE id=?")
        .bind(serde_json::to_string(&p).unwrap())
        .bind(b.report_card_id.as_str())
        .execute(db.writer_pool())
        .await
        .unwrap();
    let read = call_tool(&b, TOOL_REPORT_READ, who.clone(), json!({}))
        .await
        .unwrap();
    assert_eq!(read["text"], "");
    assert_eq!(read["anchor"]["whole"], true);
    assert_eq!(read["anchor"]["known_blocks"], 0);
    let index = call_tool(&b, TOOL_REPORT_READ, who.clone(), json!({"detail":"index"}))
        .await
        .unwrap();
    let task = index["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["kind"] == "task")
        .unwrap();
    let err = commit(&b, who, json!({"ops":[{"op":"delete","id":task["id"]}]}))
        .await
        .unwrap_err();
    assert_eq!(err.code, -32602);
    assert_eq!(err.data.unwrap()["reason_code"], "never_read");
}

#[tokio::test]
async fn unread_and_absent_declared_sections_recover_with_local_text_reads() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    let err = commit(&b, planner_identity(&b), replace("概要", "# 概要\n\nown\n"))
        .await
        .unwrap_err();
    assert_eq!(err.code, -32602);
    let d = err.data.unwrap();
    assert_eq!(d["reason_code"], "never_read");
    assert_eq!(
        d["next_step"],
        json!({"action":"read","blocks":[],"sections":["概要"]})
    );
    reread(&b, &d).await;
    commit(&b, planner_identity(&b), replace("概要", "# 概要\n\nown\n"))
        .await
        .unwrap();
    call_tool(&b, TOOL_REPORT_READ, assistant_identity(&b), json!({}))
        .await
        .unwrap();
    commit(&b, assistant_identity(&b), json!({"summary":"foreign"}))
        .await
        .unwrap();
    let err = commit(
        &b,
        planner_identity(&b),
        replace("待你定", "# 待你定\n\ncreated\n"),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32409);
    let d = err.data.unwrap();
    assert_eq!(
        d["target"],
        json!({"report_card_id":b.report_card_id,"block_ids":[],"sections":["待你定"]})
    );
    assert_eq!(d["next_step"]["blocks"].as_array().unwrap().len(), 1);
    assert!(d["next_step"]["sections"].as_array().unwrap().is_empty());
    reread(&b, &d).await;
    let out = commit(
        &b,
        planner_identity(&b),
        replace("待你定", "# 待你定\n\ncreated\n"),
    )
    .await
    .unwrap();
    assert_eq!(out["anchor"]["whole"], false);
}

#[tokio::test]
async fn refreshed_block_text_does_not_certify_the_stale_section() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要"]).await;
    let id = section_blocks(&b, "# 概要").await[0].0.clone();
    commit(
        &b,
        assistant_identity(&b),
        json!({"ops":[{"op":"upsert","id":id,"kind":"prose","markdown":"# 概要\n\nforeign\n"}]}),
    )
    .await
    .unwrap();
    call_tool(
        &b,
        TOOL_REPORT_READ,
        planner_identity(&b),
        json!({"blocks":[id]}),
    )
    .await
    .unwrap();
    let before = state(&b).await;
    let err = commit(&b, planner_identity(&b), replace("概要", "# 概要\n\nown\n"))
        .await
        .unwrap_err();
    assert_eq!(err.code, -32409);
    let d = err.data.unwrap();
    assert_eq!(
        d["changed"],
        json!([]),
        "block revisions are known, section certificate remains stale"
    );
    assert_eq!(d["next_step"]["sections"], json!(["概要"]));
    assert_eq!(state(&b).await, before);
    reread(&b, &d).await;
    commit(&b, planner_identity(&b), replace("概要", "# 概要\n\nown\n"))
        .await
        .unwrap();
}
