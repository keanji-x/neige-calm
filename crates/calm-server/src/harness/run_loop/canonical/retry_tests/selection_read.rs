//! Retry selection uses the same live runtime locator before and after claim.
use super::*;
use crate::conversation_inputs::{InputSource, SelectionReadAdmission};
use crate::harness::issuance::SelectionSource;

#[tokio::test]
async fn retry_pending_and_stored_reads_keep_scope_and_reject_rebinding() {
    for stored in [false, true] {
        let (r, mut peer) = RetryFixture::new().await;
        let repo = r.fx.original.repo.clone();
        let card = r.fx.harness.inner.card_id.to_string();
        let worker = r.fx.harness.inner.worker_session_id.clone();
        let source = InputSource::from(r.fx.source.clone()).for_identity(&r.identity);
        let CanonicalPendingIdentity::ExplicitSuccessor {
            retry_intent_id: id,
        } = r.identity.clone()
        else {
            panic!("retry identity")
        };
        let read = write_in_tx_typed(repo.as_ref(), {
            let (repo, card, worker) = (repo.clone(), card.clone(), worker.clone());
            move |tx| {
                Box::pin(async move {
                    crate::harness::wiring::record_bound_session_cwd_tx(
                        tx,
                        &card,
                        &worker,
                        "/retry-original-session",
                    )
                    .await?;
                    let mut carrier = current_input_carrier_tx(tx, &card, &worker).await?;
                    if stored {
                        let claimed = crate::conversation_inputs::claim_initial_tx(
                            tx,
                            source.clone(),
                            &mut carrier,
                            crate::db::sqlite::conversation::InitialClaim::ExplicitSuccessor {
                                retry_intent_id: id,
                            },
                        )
                        .await?;
                        SelectionReadAdmission::stored_tx(
                            tx,
                            repo,
                            source,
                            &carrier,
                            claimed.submission_id,
                        )
                        .await
                    } else {
                        SelectionReadAdmission::retry_pending_tx(tx, repo, source, &carrier, id)
                            .await
                    }
                })
            }
        })
        .await
        .unwrap();
        assert_eq!(read.cwd(), "/retry-original-session");
        read.revalidate().await.unwrap();
        let before = r.fx.frame().await;
        let gate = crate::shared_codex_appserver::native_owner_tests::hold_connected_client_gate(
            &r.fx.daemon,
        )
        .await;
        let native = crate::harness::native_admission::NativePromptAdmission::new(
            || read.check_current(),
            || read.admit_native_read(),
        );
        let effect = r.fx.daemon.config_read_admitted(
            Some(read.cwd()),
            tokio::time::Instant::now() + Duration::from_secs(5),
            &native,
        );
        tokio::pin!(effect);
        assert!(futures_util::poll!(effect.as_mut()).is_pending());
        write_in_tx_typed(repo.as_ref(), move |tx| {
            Box::pin(async move {
                crate::harness::wiring::record_bound_session_cwd_tx(
                    tx,
                    &card,
                    "other-worker",
                    "/retry-original-session",
                )
                .await
            })
        })
        .await
        .unwrap();
        drop(gate);
        let unexpected = tokio::select! {
            result = &mut effect => {
                assert!(matches!(result, Err(CalmError::Conflict(_))));
                None
            }
            call = request(&mut peer) => {
                send(&mut peer, json!({"id":call["id"],"error":{"code":-32000,"message":"unexpected rebound retry read"}})).await;
                let _ = effect.await;
                Some(call["method"].clone())
            }
        };
        assert_eq!(r.fx.frame().await, before);
        peer.close(None).await.unwrap();
        r.fx.harness.shutdown().await.unwrap();
        assert_eq!(unexpected, None);
    }
}
