//! Durable knowledge and in-batch binding through the production MCP entry points.
use crate::mcp_report_sections::{
    SECTIONS, commit, payload, read_sections, replace, section_blocks, seed,
};
use crate::mcp_track_report::{assistant_identity, boot, call_tool, planner_identity};
use calm_server::db::sqlite::report_knowledge;
use calm_server::mcp_server::tools::track_report::TOOL_REPORT_READ;
use serde_json::json;

#[tokio::test]
async fn own_marker_replace_then_delete_and_repeated_writes_bind_latest() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要"]).await;
    let id = section_blocks(&b, "# 概要").await[1].0.clone();
    commit(&b, planner_identity(&b), json!({"ops":[
        {"op":"replace","section":"概要","markdown":format!("# 概要\n\nfirst\n\n<!-- neige:{id} -->\n## detail\n\nnew\n")},
        {"op":"upsert","id":id,"kind":"prose","markdown":"## detail\n\nnewer\n"},
        {"op":"upsert","id":id,"kind":"prose","markdown":"## detail\n\nnewest\n"},
        {"op":"delete","id":id}
    ]})).await.unwrap();
    assert!(
        !payload(&b)
            .await
            .blocks
            .unwrap()
            .iter()
            .any(|block| block.id == id)
    );
    let read = report_knowledge::load(
        b.repo.as_ref(),
        &planner_identity(&b).session_id,
        b.report_card_id.as_str(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!read.blocks.contains_key(&id));
    commit(
        &b,
        planner_identity(&b),
        replace("概要", "# 概要\n\nfinal\n"),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn unrelated_own_write_retains_foreign_deleted_knowledge_and_history_ref() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要", "决策"]).await;
    read_sections(&b, assistant_identity(&b), &["概要"]).await;
    let id = section_blocks(&b, "# 概要").await[1].0.clone();
    let session = planner_identity(&b).session_id;
    let before = report_knowledge::load(b.repo.as_ref(), &session, b.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    commit(
        &b,
        assistant_identity(&b),
        json!({"ops":[{"op":"delete","id":id}]}),
    )
    .await
    .unwrap();
    commit(&b, planner_identity(&b), replace("决策", "# 决策\n\nown\n"))
        .await
        .unwrap();
    let after = report_knowledge::load(b.repo.as_ref(), &session, b.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.blocks.get(&id), before.blocks.get(&id));
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
async fn index_is_not_a_read_and_receipts_freeze_the_anchor() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    let out = call_tool(
        &b,
        TOOL_REPORT_READ,
        planner_identity(&b),
        json!({"detail":"index"}),
    )
    .await
    .unwrap();
    assert!(out["anchor"].is_null());
    assert!(
        out["anchor_status"]
            .as_str()
            .unwrap()
            .contains("index does not add knowledge")
    );
    assert_eq!(
        commit(&b, planner_identity(&b), replace("概要", "# 概要\n\nno\n"))
            .await
            .unwrap_err()
            .code,
        -32602
    );
    let read = call_tool(
        &b,
        TOOL_REPORT_READ,
        planner_identity(&b),
        json!({"sections":["概要"]}),
    )
    .await
    .unwrap();
    assert_eq!(read["anchor"]["whole"], false);
    let out = commit(&b, planner_identity(&b), replace("概要", "# 概要\n\nown\n"))
        .await
        .unwrap();
    assert_eq!(out["anchor"]["doc_rev"], out["doc_rev"]);
    assert_eq!(out["anchor"]["whole"], false);
    assert_eq!(out["anchor_status"].as_str().unwrap().lines().count(), 1);
    assert_ne!(read["anchor"]["doc_rev"], out["anchor"]["doc_rev"]);
}

#[tokio::test]
async fn supersede_both_doors_roll_back_with_session_and_compensation_requires_reread() {
    use calm_server::db::sqlite::{
        begin_immediate_tx, session_mark_superseded_runtime_tx,
        session_restore_from_superseded_runtime_tx, session_supersede_active_tx,
    };
    use calm_types::worker::WorkerSessionState;
    for tolerant in [false, true] {
        let b = boot().await;
        seed(&b, SECTIONS).await;
        read_sections(&b, planner_identity(&b), &["概要"]).await;
        let session = planner_identity(&b).session_id;
        let pool = b.repo.sqlite_db().unwrap();
        let before = report_knowledge::load(b.repo.as_ref(), &session, b.report_card_id.as_str())
            .await
            .unwrap();
        let mut tx = begin_immediate_tx(&pool).await.unwrap();
        if tolerant {
            session_mark_superseded_runtime_tx(&mut tx, &session)
                .await
                .unwrap();
        } else {
            session_supersede_active_tx(&mut tx, &session, 10)
                .await
                .unwrap();
        }
        assert!(
            report_knowledge::load_tx(&mut tx, &session, b.report_card_id.as_str())
                .await
                .unwrap()
                .is_none()
        );
        tx.rollback().await.unwrap();
        assert_eq!(
            report_knowledge::load(b.repo.as_ref(), &session, b.report_card_id.as_str())
                .await
                .unwrap(),
            before
        );
        let mut tx = begin_immediate_tx(&pool).await.unwrap();
        if tolerant {
            session_mark_superseded_runtime_tx(&mut tx, &session)
                .await
                .unwrap();
        } else {
            session_supersede_active_tx(&mut tx, &session, 20)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
        assert!(
            call_tool(&b, TOOL_REPORT_READ, planner_identity(&b), json!({}))
                .await
                .is_err(),
            "superseded identity cannot read to regain authority"
        );
        let mut tx = begin_immediate_tx(&pool).await.unwrap();
        session_restore_from_superseded_runtime_tx(&mut tx, &session, WorkerSessionState::Starting)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            commit(&b, planner_identity(&b), replace("概要", "# 概要\n\nno\n"))
                .await
                .unwrap_err()
                .code,
            -32602
        );
        read_sections(&b, planner_identity(&b), &["概要"]).await;
        commit(&b, planner_identity(&b), replace("概要", "# 概要\n\nyes\n"))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn transactional_failure_rolls_back_report_tasks_events_and_history() {
    for failure in ["ledger", "event", "history"] {
        let b = boot().await;
        seed(&b, SECTIONS).await;
        read_sections(&b, planner_identity(&b), &["概要"]).await;
        let pool = b.repo.sqlite_db().unwrap();
        let session = planner_identity(&b).session_id;
        let before = payload(&b).await;
        let known = report_knowledge::load(b.repo.as_ref(), &session, b.report_card_id.as_str())
            .await
            .unwrap();
        let head: Option<String> =
            sqlx::query_scalar("SELECT head_hash FROM track_vcs_refs WHERE track_id=?")
                .bind(b.track_id.as_str())
                .fetch_optional(pool.read_pool())
                .await
                .unwrap();
        let event_count: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
            .fetch_one(pool.read_pool())
            .await
            .unwrap();
        let tasks: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks")
            .fetch_one(pool.read_pool())
            .await
            .unwrap();
        let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
            .await
            .unwrap();
        let trigger = match failure {
            "ledger" => {
                "CREATE TRIGGER reject_write BEFORE UPDATE ON report_read_ledger BEGIN SELECT RAISE(ABORT, 'ledger failure'); END"
            }
            "event" => {
                "CREATE TRIGGER reject_write BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'event failure'); END"
            }
            _ => {
                "CREATE TRIGGER reject_write BEFORE INSERT ON track_vcs_commits BEGIN SELECT RAISE(ABORT, 'history failure'); END"
            }
        };
        sqlx::query(trigger).execute(&mut **tx).await.unwrap();
        tx.commit().await.unwrap();
        assert!(
        commit(
            &b,
            planner_identity(&b),
            json!({"ops":[
                {"op":"replace","section":"概要","markdown":"# 概要\n\nrolled back\n"},
                {"op":"upsert","kind":"task","payload":{"key":"ledger-rollback","kind":"terminal","command":"true","ready":false,"declared_by":calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR}}
            ]})
        )
        .await
        .is_err()
    );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT head_hash FROM track_vcs_refs WHERE track_id=?"
            )
            .bind(b.track_id.as_str())
            .fetch_optional(pool.read_pool())
            .await
            .unwrap(),
            head
        );
        assert_eq!(payload(&b).await.body, before.body);
        assert_eq!(payload(&b).await.doc_rev, before.doc_rev);
        assert_eq!(
            report_knowledge::load(b.repo.as_ref(), &session, b.report_card_id.as_str())
                .await
                .unwrap(),
            known
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM events")
                .fetch_one(pool.read_pool())
                .await
                .unwrap(),
            event_count
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tasks")
                .fetch_one(pool.read_pool())
                .await
                .unwrap(),
            tasks
        );
    }
}

#[tokio::test]
async fn deletion_cascades_for_session_report_track_and_area() {
    for target in ["session", "report", "track", "area"] {
        let b = boot().await;
        seed(&b, SECTIONS).await;
        // A live root session has an independent NO ACTION FK from tracks.root_session_id.
        // An Assistant exercises direct session deletion without changing that existing fence.
        let who = if target == "session" {
            assistant_identity(&b)
        } else {
            planner_identity(&b)
        };
        read_sections(&b, who.clone(), &["概要"]).await;
        let session = who.session_id;
        let pool = b.repo.sqlite_db().unwrap();
        let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
            .await
            .unwrap();
        match target {
            "session" => {
                sqlx::query("DELETE FROM worker_sessions WHERE id=?")
                    .bind(&session)
                    .execute(&mut **tx)
                    .await
                    .unwrap();
            }
            "report" => {
                sqlx::query("DELETE FROM cards WHERE id=?")
                    .bind(b.report_card_id.as_str())
                    .execute(&mut **tx)
                    .await
                    .unwrap();
            }
            // Domain deletions retain their existing required-session/root-reference ordering.
            "track" => calm_server::db::sqlite::track_delete_tx(
                &mut tx,
                b.track_id.as_str(),
                &calm_server::track_area_cache::TrackAreaCache::new(),
            )
            .await
            .unwrap(),
            _ => calm_server::db::sqlite::area_delete_tx(&mut tx, b.area_id.as_str())
                .await
                .unwrap(),
        }
        assert!(
            report_knowledge::load_tx(&mut tx, &session, b.report_card_id.as_str())
                .await
                .unwrap()
                .is_none()
        );
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM report_read_ledger_blocks WHERE worker_session_id=? AND report_card_id=?")
            .bind(&session).bind(b.report_card_id.as_str()).fetch_one(&mut **tx).await.unwrap();
        assert_eq!(count, 0, "{target}: block evidence cascades");
        tx.commit().await.unwrap();
    }
}

#[tokio::test]
async fn concurrent_reads_merge_without_lost_knowledge() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    let a = section_blocks(&b, "# 概要").await[0].0.clone();
    let c = section_blocks(&b, "# 决策").await[0].0.clone();
    let (left, right) = tokio::join!(
        call_tool(
            &b,
            TOOL_REPORT_READ,
            planner_identity(&b),
            json!({"blocks":[a]})
        ),
        call_tool(
            &b,
            TOOL_REPORT_READ,
            planner_identity(&b),
            json!({"blocks":[c]})
        ),
    );
    left.unwrap();
    right.unwrap();
    let read = report_knowledge::load(
        b.repo.as_ref(),
        &planner_identity(&b).session_id,
        b.report_card_id.as_str(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(read.blocks.contains_key(&a) && read.blocks.contains_key(&c));
    assert!(read.blocks.values().all(|r| matches!(
        r.history_ref,
        calm_types::report_knowledge::HistoryRef::Unavailable(
            calm_types::report_knowledge::HistoryUnavailable::NotLocated
        )
    )));
    assert!(!read.whole);
}

#[tokio::test]
async fn refreshed_block_does_not_refresh_independent_section_certificate() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要"]).await;
    read_sections(&b, assistant_identity(&b), &["概要"]).await;
    let id = section_blocks(&b, "# 概要").await[1].0.clone();
    commit(
        &b,
        assistant_identity(&b),
        json!({"ops":[{"op":"upsert","id":id,"kind":"prose","markdown":"## detail\n\nforeign\n"}]}),
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
    assert_eq!(
        commit(&b, planner_identity(&b), replace("概要", "# 概要\n\nno\n"))
            .await
            .unwrap_err()
            .code,
        -32409
    );
    // The refreshed block itself remains writable.
    commit(
        &b,
        planner_identity(&b),
        json!({"ops":[{"op":"upsert","id":id,"kind":"prose","markdown":"## detail\n\nown\n"}]}),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn read_save_failure_does_not_return_success_or_replace_knowledge() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要"]).await;
    let session = planner_identity(&b).session_id;
    let known = report_knowledge::load(b.repo.as_ref(), &session, b.report_card_id.as_str())
        .await
        .unwrap();
    let pool = b.repo.sqlite_db().unwrap();
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER reject_read BEFORE INSERT ON report_read_ledger_blocks
         BEGIN SELECT RAISE(ABORT, 'read ledger failure'); END",
    )
    .execute(&mut **tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(
        call_tool(
            &b,
            TOOL_REPORT_READ,
            planner_identity(&b),
            json!({"sections":["决策"]})
        )
        .await
        .is_err()
    );
    assert_eq!(
        report_knowledge::load(b.repo.as_ref(), &session, b.report_card_id.as_str())
            .await
            .unwrap(),
        known
    );
}

#[tokio::test]
async fn corrupt_history_ref_is_an_error_instead_of_never_read() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要"]).await;
    let pool = b.repo.sqlite_db().unwrap();
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE report_read_ledger_blocks SET history_ref='{}' WHERE worker_session_id=?")
        .bind(&planner_identity(&b).session_id)
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let err = call_tool(
        &b,
        TOOL_REPORT_READ,
        planner_identity(&b),
        json!({"detail":"index"}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32603);
}

#[tokio::test]
async fn concurrent_own_commits_bind_serialized_knowledge_and_freeze_each_receipt() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要"]).await;
    let before = payload(&b).await.doc_rev;
    let (left, right) = tokio::join!(
        commit(
            &b,
            planner_identity(&b),
            replace("概要", "# 概要\n\nleft\n")
        ),
        commit(
            &b,
            planner_identity(&b),
            replace("概要", "# 概要\n\nright\n")
        ),
    );
    let left = left.unwrap();
    let right = right.unwrap();
    let mut revisions = vec![
        left["doc_rev"].as_u64().unwrap(),
        right["doc_rev"].as_u64().unwrap(),
    ];
    revisions.sort();
    assert_eq!(revisions, vec![before + 1, before + 2]);
    for receipt in [&left, &right] {
        assert_eq!(receipt["anchor"]["doc_rev"], receipt["doc_rev"]);
    }
    let read = report_knowledge::load(
        b.repo.as_ref(),
        &planner_identity(&b).session_id,
        b.report_card_id.as_str(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(read.doc_rev, before + 2);
    assert_eq!(
        read.sections.get("概要"),
        Some(
            &section_blocks(&b, "# 概要")
                .await
                .into_iter()
                .map(|(id, rev)| (id, rev as u32))
                .collect()
        )
    );
}

#[tokio::test]
async fn repeated_moves_in_one_batch_never_grant_unread_content() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["已完成"]).await;
    let id = section_blocks(&b, "# 概要").await[1].0.clone();
    commit(
        &b,
        planner_identity(&b),
        json!({"ops":[
            {"op":"move","id":id,"to_index":3},
            {"op":"move","id":id,"to_index":3}
        ]}),
    )
    .await
    .unwrap();
    let read = report_knowledge::load(
        b.repo.as_ref(),
        &planner_identity(&b).session_id,
        b.report_card_id.as_str(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!read.blocks.contains_key(&id));
    assert_eq!(
        commit(
            &b,
            planner_identity(&b),
            json!({"ops":[{"op":"delete","id":id}]})
        )
        .await
        .unwrap_err()
        .code,
        -32602
    );
}
