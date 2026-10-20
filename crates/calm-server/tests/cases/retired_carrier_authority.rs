//! Exercise handlers after transport admission; retirement happens before their SQL writes.
use crate::mcp_track_report::{boot, call_tool, planner_identity};
use calm_server::db::sqlite::{acp_registration_claim, session_mark_superseded_runtime_tx};
use calm_server::db::write_in_tx_typed;
use calm_server::mcp_server::tools::source::TOOL_SOURCE_CAPTURE;
use calm_server::mcp_server::tools::user_ask::TOOL_USER_ASK;
use serde_json::json;

#[tokio::test]
async fn retired_carrier_authority_source_insert_and_append() {
    let boot = boot().await;
    let identity = planner_identity(&boot); // already admitted
    let args = json!({"manual":{"text":"original body"},"provenance":"manual","title":"source"});
    let source = call_tool(&boot, TOOL_SOURCE_CAPTURE, identity.clone(), args.clone())
        .await
        .unwrap();
    let worker = identity.session_id.clone();
    write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            session_mark_superseded_runtime_tx(tx, &worker).await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    for args in [
        args,
        json!({"source_id":source["source_id"],"quotes":["original"]}),
    ] {
        let error = call_tool(&boot, TOOL_SOURCE_CAPTURE, identity.clone(), args)
            .await
            .unwrap_err();
        assert_eq!(error.code, -32403, "{error}");
    }
}

#[tokio::test]
async fn retired_carrier_authority_current_pointer_and_contract() {
    let boot = boot().await;
    let identity = planner_identity(&boot);
    let db = boot.repo.sqlite_db().unwrap();
    // Keep the session active: a per-call liveness test alone cannot close this window.
    for mutation in [
        "UPDATE cards SET session_id=NULL WHERE id=?1",
        "UPDATE worker_sessions SET contract='executor' WHERE card_id=?1",
    ] {
        let mut tx = calm_server::db::sqlite::begin_immediate_tx(&db)
            .await
            .unwrap();
        sqlx::query(mutation)
            .bind(&identity.card_id)
            .execute(&mut **tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let error = call_tool(
            &boot,
            TOOL_USER_ASK,
            identity.clone(),
            json!({"questions":[{"title":"forbidden"}]}),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, -32403, "{error}");
        let error = call_tool(
            &boot,
            TOOL_SOURCE_CAPTURE,
            identity.clone(),
            json!({"manual":{"text":"body"},"provenance":"manual","title":"source"}),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, -32403, "{error}");
        sqlx::query("UPDATE cards SET session_id=?1 WHERE id=?2")
            .bind(&identity.session_id)
            .bind(&identity.card_id)
            .execute(db.writer_pool())
            .await
            .unwrap();
    }
    let asks: i64 = sqlx::query_scalar("SELECT count(*) FROM events WHERE kind='ask.requested'")
        .fetch_one(db.read_pool())
        .await
        .unwrap();
    assert_eq!(asks, 0);
}

#[tokio::test]
async fn retired_carrier_authority_native_registration_fresh_and_replay() {
    for registered in [false, true] {
        let boot = boot().await;
        let identity = planner_identity(&boot);
        let db = boot.repo.sqlite_db().unwrap();
        if registered {
            acp_registration_claim(&db, &identity.session_id, "digest", false)
                .await
                .unwrap();
        }
        let worker = identity.session_id.clone();
        write_in_tx_typed(boot.repo.as_ref(), move |tx| {
            Box::pin(async move {
                session_mark_superseded_runtime_tx(tx, &worker).await?;
                Ok(())
            })
        })
        .await
        .unwrap();
        assert!(
            acp_registration_claim(&db, &identity.session_id, "digest", false)
                .await
                .is_err(),
            "retired registration (existing={registered}) must be refused"
        );
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM acp_managed_sessions WHERE worker_session_id=?1",
        )
        .bind(&identity.session_id)
        .fetch_one(db.read_pool())
        .await
        .unwrap();
        assert_eq!(count, i64::from(registered));
    }
}

#[tokio::test]
async fn retired_carrier_authority_requires_card_link() {
    let boot = boot().await;
    let identity = planner_identity(&boot);
    let db = boot.repo.sqlite_db().unwrap();
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&db)
        .await
        .unwrap();
    let worker = calm_types::worker::WorkerSessionId::from(identity.session_id);
    sqlx::query("UPDATE worker_sessions SET card_id=NULL WHERE id=?1")
        .bind(worker.as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    // Native binding consumes this resolver directly, without the recorder's
    // separate cardless denial. It must not receive an active orphan as authority.
    let resolved = calm_truth::decision_gate::WriteTx::read_worker_session(&mut *tx, &worker)
        .await
        .unwrap();
    assert!(
        resolved.is_none(),
        "an active worker with no card link is not current authority"
    );
    tx.rollback().await.unwrap();
}
