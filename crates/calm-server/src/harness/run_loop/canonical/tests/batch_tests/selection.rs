//! Freeze all selected revisions before reads, then fence the same whole prefix.
use super::fixture::*;
use super::*;
use crate::db::sqlite::conversation::{PendingHumanChange, PendingHumanWrite};

#[derive(Clone, Copy)]
enum Change {
    Edit,
    Withdraw,
    Erase,
    Revoke,
    Carrier,
}

async fn changed_selection(change: Change, during_catalog: bool) {
    let fx = Fixture::new().await;
    let tail = content_tests::accept(&fx, "selected tail before model read").await;
    selection_authority_tests::configure_default_effort(
        &fx.original.repo,
        fx.harness.inner.card_id.as_str(),
    )
    .await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    observe(&fx).await;
    let config = request(&mut peer).await;
    assert_eq!(config["method"], "config/read");
    let call = if during_catalog {
        selection_authority_tests::config_reply(&mut peer, &config).await;
        let page = request(&mut peer).await;
        assert_eq!(page["method"], "model/list");
        page
    } else {
        config
    };
    match change {
        Change::Edit | Change::Withdraw => {
            let (result, _, _, handoff) = crate::conversation_inputs::pending_controls::mutate(
                fx.original.repo.as_ref(),
                fx.registry.clone(),
                fx.source.clone(),
                crate::conversation_inputs::pending_controls::PendingHumanMutation {
                    card: fx.harness.inner.card_id.to_string(),
                    worker: fx.harness.inner.worker_session_id.clone(),
                    expected: tail,
                    change: if matches!(change, Change::Edit) {
                        PendingHumanChange::Edit {
                            text: "edited selected tail".into(),
                        }
                    } else {
                        PendingHumanChange::Withdraw
                    },
                },
            )
            .await
            .unwrap();
            assert!(matches!(result, PendingHumanWrite::Applied { .. }));
            drop(handoff);
            if matches!(change, Change::Withdraw) {
                // Preserve the length but change the selected identity. A count
                // or head-only fence would silently claim this newer input.
                content_tests::accept(&fx, "replacement appended after selection").await;
            }
        }
        Change::Erase => {
            erase(&fx, &tail).await;
        }
        Change::Revoke => {
            fx.auth.sessions.remove(&fx.session).unwrap();
        }
        Change::Carrier => {
            fx.original
                .repo
                .db()
                .writer()
                .with_writer(|write_session| {
                    let fx = &fx;
                    Box::pin(async move {
                        let mut tx = write_session.begin_immediate().await.unwrap();
                        let carrier = current_input_carrier_tx(
                            &mut tx,
                            fx.harness.inner.card_id.as_str(),
                            &fx.harness.inner.worker_session_id,
                        )
                        .await
                        .unwrap();
                        let mut snapshot: CanonicalHarnessSnapshot =
                            serde_json::from_value(carrier.snapshot().clone()).unwrap();
                        snapshot.header.last_thread_id =
                            Some("changed-batch-carrier-thread".into());
                        persist_runtime_frame_tx(
                            &mut tx,
                            carrier.card_id(),
                            carrier.worker_session_id(),
                            &RuntimeExecutionFence::from_carrier(&carrier),
                            header_value(&snapshot).unwrap(),
                        )
                        .await
                        .unwrap();
                        sqlx::query("UPDATE worker_sessions SET thread_id=?1 WHERE id=?2")
                            .bind(&snapshot.header.last_thread_id)
                            .bind(&fx.harness.inner.worker_session_id)
                            .execute(&mut **tx)
                            .await
                            .unwrap();
                        tx.commit().await.unwrap();
                    })
                })
                .await
                .expect("independent write operation");
        }
    }
    let expected = fx.frame().await;
    let reply = if during_catalog {
        selection_authority_tests::catalog_reply(&call, None)
    } else {
        json!({"id":call["id"],"result":{"config":{
            "model":"fixture-private-model","model_reasoning_effort":null
        }}})
    };
    assert_held_after_reply(&fx, &mut peer, reply, expected).await;
}

#[tokio::test]
async fn canonical_batch_selected_tail_edit_before_catalog_holds_all_inputs() {
    changed_selection(Change::Edit, false).await;
}

#[tokio::test]
async fn canonical_batch_selected_tail_edit_after_catalog_cannot_change_claim() {
    changed_selection(Change::Edit, true).await;
}

#[tokio::test]
async fn canonical_batch_selected_tail_withdraw_after_catalog_cannot_replace_claim() {
    changed_selection(Change::Withdraw, true).await;
}

#[tokio::test]
async fn canonical_batch_selected_tail_erasure_after_catalog_holds_all_inputs() {
    changed_selection(Change::Erase, true).await;
}

#[tokio::test]
async fn canonical_batch_source_revocation_after_catalog_holds_all_inputs() {
    changed_selection(Change::Revoke, true).await;
}

#[tokio::test]
async fn canonical_batch_carrier_change_after_catalog_holds_all_inputs() {
    changed_selection(Change::Carrier, true).await;
}
