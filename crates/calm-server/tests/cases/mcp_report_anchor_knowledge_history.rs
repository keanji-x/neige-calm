//! Historical knowledge is located through production Report reads and commits.
use crate::mcp_report_sections::{SECTIONS, read_sections, section_blocks, seed};
use crate::mcp_track_report::{boot, planner_identity};
use calm_server::db::sqlite::report_knowledge;

#[tokio::test]
async fn read_locates_the_rendered_block_at_real_head() {
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要"]).await;
    let pool = b.repo.sqlite_db().unwrap();
    let head = calm_truth::track_vcs::head(pool.read_pool(), &b.track_id)
        .await
        .unwrap()
        .unwrap();
    let id = section_blocks(&b, "# 概要").await[0].0.clone();
    let read = report_knowledge::load(
        b.repo.as_ref(),
        &planner_identity(&b).session_id,
        b.report_card_id.as_str(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        serde_json::to_value(&read.blocks[&id].history_ref).unwrap(),
        serde_json::json!({"commit": head})
    );
}

async fn head(b: &crate::mcp_track_report::Boot) -> String {
    let pool = b.repo.sqlite_db().unwrap();
    calm_truth::track_vcs::head(pool.read_pool(), &b.track_id)
        .await
        .unwrap()
        .unwrap()
}

async fn known(b: &crate::mcp_track_report::Boot) -> calm_types::report_knowledge::LastRead {
    report_knowledge::load(
        b.repo.as_ref(),
        &planner_identity(b).session_id,
        b.report_card_id.as_str(),
    )
    .await
    .unwrap()
    .unwrap()
}

async fn counts(b: &crate::mcp_track_report::Boot) -> (i64, i64, i64) {
    let pool = b.repo.sqlite_db().unwrap();
    sqlx::query_as(
        "SELECT (SELECT count(*) FROM track_vcs_commits), \
         (SELECT count(*) FROM track_vcs_objects), (SELECT count(*) FROM events)",
    )
    .fetch_one(pool.read_pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn old_partial_knowledge_survives_history_prune() {
    use crate::mcp_report_sections::{commit, replace};
    use crate::mcp_track_report::assistant_identity;
    use calm_server::db::sqlite::{begin_immediate_tx, session_mark_superseded_runtime_tx};
    use calm_truth::track_vcs as vcs;
    for release in ["overwrite", "delete", "whole"] {
        let b = boot().await;
        seed(&b, SECTIONS).await;
        read_sections(&b, planner_identity(&b), &["概要"]).await;
        let pool = b.repo.sqlite_db().unwrap();
        let old = head(&b).await;
        assert!(
            known(&b)
                .await
                .blocks
                .values()
                .all(|block| block.history_ref
                    == calm_types::report_knowledge::HistoryRef::Commit(old.clone()))
        );
        let path = format!("cards/{}/.payload.json", b.report_card_id);
        let blob = vcs::tree_at(pool.read_pool(), &old)
            .await
            .unwrap()
            .unwrap()
            .entries[&path]
            .blob_hash
            .clone();
        let mut previous = old.clone();
        for n in 0..65 {
            read_sections(&b, assistant_identity(&b), &["决策"]).await;
            commit(
                &b,
                assistant_identity(&b),
                replace("决策", &format!("# 决策\n\nadvance {n}\n")),
            )
            .await
            .unwrap();
            let current = head(&b).await;
            let wake = vcs::since_last_turn_block(
                pool.read_pool(),
                &b.track_id,
                Some(&previous),
                Some(&current),
                Some(&b.planner_card_id),
                &planner_identity(&b).session_id.into(),
                vcs::ReportPatch::Omit,
            )
            .await
            .unwrap();
            assert_eq!(wake.current_head.as_deref(), Some(current.as_str()));
            // Advance both existing GC endpoints as wake does. The fixture uses the endpoint
            // frame already recognized by this collector, so skip-prune cannot mask retention.
            let mut tx = begin_immediate_tx(&pool).await.unwrap();
            sqlx::query("UPDATE worker_sessions SET handle_state_json=? WHERE track_id=?")
                .bind(serde_json::json!({"schema_version":1,"mode":"harness","last_seen_head":current,"issued_turn_head":current}).to_string())
                .bind(b.track_id.as_str()).execute(&mut **tx).await.unwrap();
            tx.commit().await.unwrap();
            previous = current;
        }
        // Release the seeding/writing assistant's knowledge through its actual lifecycle door.
        let mut tx = begin_immediate_tx(&pool).await.unwrap();
        session_mark_superseded_runtime_tx(&mut tx, &assistant_identity(&b).session_id)
            .await
            .unwrap();
        // Surviving knowledge must also protect an inactive session, outside active harnesses.
        sqlx::query("UPDATE worker_sessions SET state='exited' WHERE id=?")
            .bind(planner_identity(&b).session_id)
            .execute(&mut **tx)
            .await
            .unwrap();
        sqlx::query("UPDATE track_vcs_objects SET created_at=0")
            .execute(&mut **tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        vcs::prune_all_tracks_once(&pool, 50).await.unwrap();
        vcs::sweep_unreferenced_objects_once(&pool).await.unwrap();
        assert!(vcs::cat_at(pool.read_pool(), &old, &path).await.is_ok());
        let retained = counts(&b).await;
        assert!(retained.0 >= 66);
        let mut tx = begin_immediate_tx(&pool).await.unwrap();
        sqlx::query("UPDATE worker_sessions SET state='running' WHERE id=?")
            .bind(planner_identity(&b).session_id)
            .execute(&mut **tx)
            .await
            .unwrap();
        // Remove the other (fresh) harness endpoints from retention; only knowledge is under test.
        sqlx::query("UPDATE worker_sessions SET handle_state_json=NULL WHERE track_id=?")
            .bind(b.track_id.as_str())
            .execute(&mut **tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        match release {
            "overwrite" => {
                commit(
                    &b,
                    planner_identity(&b),
                    replace("概要", "# 概要\n\nreplacement\n"),
                )
                .await
                .unwrap();
            }
            "delete" => {
                commit(
                    &b,
                    planner_identity(&b),
                    serde_json::json!({"ops":[{"op":"delete","section":"概要"}]}),
                )
                .await
                .unwrap();
            }
            _ => {
                use crate::mcp_track_report::call_tool;
                call_tool(
                    &b,
                    calm_server::mcp_server::tools::track_report::TOOL_REPORT_READ,
                    planner_identity(&b),
                    serde_json::json!({}),
                )
                .await
                .unwrap();
                call_tool(&b, calm_server::mcp_server::tools::track_report_blocks::TOOL_REPORT_WRITE, planner_identity(&b), serde_json::json!({"body":"# fresh\n\nreplacement\n", "message":"whole replacement"})).await.unwrap();
            }
        }
        assert!(
            !known(&b)
                .await
                .blocks
                .values()
                .any(|block| block.history_ref
                    == calm_types::report_knowledge::HistoryRef::Commit(old.clone()))
        );
        assert!(vcs::prune_all_tracks_once(&pool, 1).await.unwrap() > 60);
        assert!(vcs::sweep_unreferenced_objects_once(&pool).await.unwrap() > 0);
        let released = counts(&b).await;
        eprintln!(
            "{release}: retained (commits, objects, events)={retained:?}; released={released:?}"
        );
        assert!(released.0 < retained.0 && released.1 < retained.1);
        assert!(vcs::cat_at(pool.read_pool(), &old, &path).await.is_err());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM track_vcs_objects WHERE hash=?")
                .bind(blob)
                .fetch_one(pool.read_pool())
                .await
                .unwrap(),
            0
        );
    }
}

/// Publish an intentionally divergent historical snapshot through real VCS, restoring live
/// truth before commit. This models a legacy/stale history projection without copying location.
async fn divergent_history(b: &crate::mcp_track_report::Boot, value: serde_json::Value) {
    use calm_truth::track_vcs as vcs;
    let pool = b.repo.sqlite_db().unwrap();
    let original = b
        .repo
        .card_get(b.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap()
        .payload;
    let parent = head(b).await;
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE cards SET payload=? WHERE id=?")
        .bind(value.to_string())
        .bind(b.report_card_id.as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    let tree = vcs::snapshot_tree(&mut tx, &b.track_id, vcs::MANIFEST_SCHEMA_VERSION)
        .await
        .unwrap();
    vcs::commit_tree(
        &mut tx,
        &b.track_id,
        Some(&parent),
        &tree,
        None,
        "legacy projection fixture",
        vcs::MANIFEST_SCHEMA_VERSION,
    )
    .await
    .unwrap();
    sqlx::query("UPDATE cards SET payload=? WHERE id=?")
        .bind(original.to_string())
        .bind(b.report_card_id.as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn read_history_requires_exact_id_rev_kind_payload_and_unique_id() {
    use calm_types::report_knowledge::{HistoryRef, HistoryUnavailable};
    for field in ["id", "rev", "kind", "payload", "duplicate", "legacy"] {
        let b = boot().await;
        seed(&b, SECTIONS).await;
        let id = section_blocks(&b, "# 概要").await[0].0.clone();
        let other = section_blocks(&b, "# 决策").await[0].0.clone();
        let mut value = b
            .repo
            .card_get(b.report_card_id.as_str())
            .await
            .unwrap()
            .unwrap()
            .payload;
        if field == "legacy" {
            value.as_object_mut().unwrap().remove("blocks");
        } else {
            let blocks = value["blocks"].as_array_mut().unwrap();
            let index = blocks.iter().position(|block| block["id"] == id).unwrap();
            match field {
                "id" => blocks[index]["id"] = serde_json::json!("different-id"),
                "rev" => blocks[index]["rev"] = serde_json::json!(777),
                "kind" => blocks[index]["kind"] = serde_json::json!("different-kind"),
                "payload" => {
                    blocks[index]["payload"] = serde_json::json!({"markdown":"different content"})
                }
                _ => blocks.push(blocks[index].clone()),
            }
        }
        divergent_history(&b, value).await;
        let text = read_sections(&b, planner_identity(&b), &["概要", "决策"]).await;
        assert!(text.contains("alpha"));
        let read = known(&b).await;
        assert_eq!(
            read.blocks[&id].history_ref,
            HistoryRef::Unavailable(HistoryUnavailable::NotLocated),
            "{field}"
        );
        // Mismatch is per block, not a reason to bind all blocks or reject the valid text.
        if field != "legacy" {
            assert_eq!(
                read.blocks[&other].history_ref,
                HistoryRef::Commit(head(&b).await)
            );
        }
    }
}

#[tokio::test]
async fn absent_history_preserves_read_authority_and_can_be_replaced_by_own_write() {
    use crate::mcp_report_sections::{commit, replace};
    use calm_types::report_knowledge::{HistoryRef, HistoryUnavailable};
    for missing in ["head", "tree", "blob"] {
        let b = boot().await;
        seed(&b, SECTIONS).await;
        let pool = b.repo.sqlite_db().unwrap();
        let previous = head(&b).await;
        let record = calm_truth::track_vcs::commit_record(pool.read_pool(), &previous)
            .await
            .unwrap()
            .unwrap();
        let manifest = calm_truth::track_vcs::tree_at(pool.read_pool(), &previous)
            .await
            .unwrap()
            .unwrap();
        let path = format!("cards/{}/.payload.json", b.report_card_id);
        let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
            .await
            .unwrap();
        match missing {
            "head" => {
                sqlx::query("DELETE FROM track_vcs_refs WHERE track_id=?")
                    .bind(b.track_id.as_str())
                    .execute(&mut **tx)
                    .await
                    .unwrap();
            }
            "tree" => {
                sqlx::query("DELETE FROM track_vcs_objects WHERE hash=?")
                    .bind(record.tree_hash)
                    .execute(&mut **tx)
                    .await
                    .unwrap();
            }
            _ => {
                sqlx::query("DELETE FROM track_vcs_objects WHERE hash=?")
                    .bind(&manifest.entries[&path].blob_hash)
                    .execute(&mut **tx)
                    .await
                    .unwrap();
            }
        }
        tx.commit().await.unwrap();
        let text = read_sections(&b, planner_identity(&b), &["概要"]).await;
        assert!(text.contains("alpha"));
        let read = known(&b).await;
        assert!(
            read.blocks.values().all(|block| block.history_ref
                == HistoryRef::Unavailable(HistoryUnavailable::NotLocated))
        );
        // A missing history is not unread. The no-HEAD case rebuilds through the normal writer.
        if missing == "head" {
            commit(&b, planner_identity(&b), replace("概要", "# 概要\n\nnew\n"))
                .await
                .unwrap();
            let current = head(&b).await;
            assert!(
                known(&b)
                    .await
                    .blocks
                    .values()
                    .all(|block| block.history_ref == HistoryRef::Commit(current.clone()))
            );
        }
    }
}

#[tokio::test]
async fn partial_reads_and_own_writes_keep_independent_commits_without_extra_history() {
    use crate::mcp_report_sections::{commit, replace};
    use crate::mcp_track_report::assistant_identity;
    use calm_types::report_knowledge::HistoryRef;
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要"]).await;
    let first = head(&b).await;
    let overview = section_blocks(&b, "# 概要").await[0].0.clone();
    let decision = section_blocks(&b, "# 决策").await[0].0.clone();
    read_sections(&b, assistant_identity(&b), &["概要"]).await;
    commit(
        &b,
        assistant_identity(&b),
        replace("概要", "# 概要\n\nforeign\n"),
    )
    .await
    .unwrap();
    let second = head(&b).await;
    let before = counts(&b).await;
    read_sections(&b, planner_identity(&b), &["决策"]).await;
    assert_eq!(counts(&b).await, before, "reads never create history");
    let read = known(&b).await;
    assert_eq!(
        read.blocks[&overview].history_ref,
        HistoryRef::Commit(first.clone())
    );
    assert_eq!(
        read.blocks[&decision].history_ref,
        HistoryRef::Commit(second)
    );
    commit(&b, planner_identity(&b), replace("决策", "# 决策\n\nown\n"))
        .await
        .unwrap();
    assert_eq!(
        counts(&b).await.0,
        before.0 + 1,
        "one edit produces exactly one history commit"
    );
    let read = known(&b).await;
    assert_eq!(
        read.blocks[&overview].history_ref,
        HistoryRef::Commit(first)
    );
    assert_eq!(
        read.blocks[&decision].history_ref,
        HistoryRef::Commit(head(&b).await)
    );
    assert_eq!(
        commit(
            &b,
            planner_identity(&b),
            replace("概要", "# 概要\n\nstolen\n")
        )
        .await
        .unwrap_err()
        .code,
        -32409
    );
}

#[tokio::test]
async fn mismatched_final_history_rolls_back_truth_history_knowledge_and_broadcast() {
    use crate::mcp_report_sections::{commit, payload, replace};
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要"]).await;
    let original = payload(&b).await;
    let read = known(&b).await;
    let current = head(&b).await;
    let before = counts(&b).await;
    let pool = b.repo.sqlite_db().unwrap();
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
        .await
        .unwrap();
    // Poison only the VCS input after the row was written and its event serialized.
    sqlx::query(&format!(
        "CREATE TRIGGER divergent_history AFTER INSERT ON events \
        WHEN NEW.kind='card.updated' BEGIN UPDATE cards \
        SET payload=json_set(payload,'$.body','divergent history') WHERE id='{}'; END",
        b.report_card_id
    ))
    .execute(&mut **tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let mut receiver = b.ctx.events.subscribe();
    let error = commit(
        &b,
        planner_identity(&b),
        replace("概要", "# 概要\n\nrollback\n"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32603);
    assert_eq!(payload(&b).await, original);
    assert_eq!(known(&b).await, read);
    assert_eq!(head(&b).await, current);
    assert_eq!(counts(&b).await, before);
    assert!(matches!(
        receiver.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn corrupt_or_missing_knowledge_commit_skips_prune_for_inactive_sessions() {
    use crate::mcp_report_sections::{commit, replace};
    use crate::mcp_track_report::assistant_identity;
    let b = boot().await;
    seed(&b, SECTIONS).await;
    read_sections(&b, planner_identity(&b), &["概要"]).await;
    read_sections(&b, assistant_identity(&b), &["决策"]).await;
    commit(
        &b,
        assistant_identity(&b),
        replace("决策", "# 决策\n\nnew\n"),
    )
    .await
    .unwrap();
    let pool = b.repo.sqlite_db().unwrap();
    for invalid in [
        serde_json::json!({}),
        serde_json::json!({"commit":"absent"}),
    ] {
        let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE worker_sessions SET state='exited',handle_state_json=NULL WHERE track_id=?",
        )
        .bind(b.track_id.as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
        sqlx::query("UPDATE report_read_ledger_blocks SET history_ref=? WHERE worker_session_id=?")
            .bind(invalid.to_string())
            .bind(planner_identity(&b).session_id)
            .execute(&mut **tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let before = counts(&b).await;
        assert_eq!(
            calm_truth::track_vcs::prune_all_tracks_once(&pool, 1)
                .await
                .unwrap(),
            0
        );
        assert_eq!(counts(&b).await, before);
    }
}

#[tokio::test]
async fn report_knowledge_delta_header_does_not_echo_known_report_after_sixty_edits() {
    use crate::mcp_report_sections::{commit, replace};
    use crate::mcp_track_report::assistant_identity;
    use calm_truth::track_vcs as vcs;
    let b = boot().await;
    seed(&b, SECTIONS).await;
    let before = head(&b).await;
    for n in 0..64 {
        if n % 4 < 2 {
            let identity = if n % 4 == 0 {
                assistant_identity(&b)
            } else {
                planner_identity(&b)
            };
            read_sections(&b, identity.clone(), &["决策"]).await;
            commit(
                &b,
                identity,
                replace("决策", &format!("# 决策\n\nedit {n}\n")),
            )
            .await
            .unwrap();
        } else {
            let (track, card, current) = calm_server::track_report::resolve_report_for_track(
                b.repo.as_ref(),
                b.track_id.as_str(),
            )
            .await
            .unwrap();
            let rev = current.doc_rev;
            let (actor, author) = if n % 4 == 2 {
                (
                    calm_server::ids::ActorId::User,
                    calm_server::event::EditAuthor::User,
                )
            } else {
                (
                    calm_server::ids::ActorId::Kernel,
                    calm_server::event::EditAuthor::Kernel,
                )
            };
            let body = format!(
                "{}{}",
                crate::mcp_report_sections::CONTRACT,
                SECTIONS.replace("gamma", &format!("edit {n}"))
            );
            calm_server::track_report::persist_report(
                b.repo.as_ref(),
                &b.ctx.events,
                &b.ctx.write,
                actor,
                author,
                track,
                card,
                current,
                calm_server::track_report::TrackReportPayload::new("", body),
                rev,
                None,
            )
            .await
            .unwrap();
        }
    }
    read_sections(&b, planner_identity(&b), &["决策"]).await;
    let pool = b.repo.sqlite_db().unwrap();
    let wake = vcs::since_last_turn_block(
        pool.read_pool(),
        &b.track_id,
        Some(&before),
        None,
        Some(&b.planner_card_id),
        &planner_identity(&b).session_id.into(),
        vcs::ReportPatch::Include,
    )
    .await
    .unwrap();
    let text = wake.block.unwrap_or_default();
    assert!(
        !text.contains("report.md"),
        "known Report must not echo through the header: {text}"
    );
    assert!(
        !text.contains(b.report_card_id.as_str()),
        "Report card summary must use knowledge: {text}"
    );
}
