//! The real CLI replay must settle input delivery, independently of execution END.
use super::*;

#[tokio::test]
async fn canonical_claude_matching_replay_removes_delivery_warning_without_keep() {
    let fx = Fixture::new("exit").await;
    fx.dispatch().await.unwrap();
    fx.wait_end().await;
    let checkpoint = fx.checkpoint().await;
    assert!(matches!(
        checkpoint.attempts[0].delivery,
        InputDeliveryFact::Accepted { .. }
    ));
    assert!(
        checkpoint.receipts.is_empty(),
        "native acknowledgement needs no Human decision"
    );
    use crate::db::RepoRead;
    assert!(
        fx.owner
            .repo
            .conversation_read_frame(&fx.owner.card)
            .await
            .unwrap()
            .active_input
            .is_none(),
        "completed acknowledged input must not remain in the held-input projection"
    );
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_missing_or_mismatched_replay_remains_unknown() {
    for scenario in [
        "no-input-replay",
        "wrong-input-replay",
        "receipt-before-init",
        "wrong-replay-session",
    ] {
        let fx = Fixture::new(scenario).await;
        fx.dispatch().await.unwrap();
        fx.wait_end().await;
        assert!(
            matches!(
                fx.checkpoint().await.attempts[0].delivery,
                InputDeliveryFact::Unknown
            ),
            "{scenario}"
        );
        use crate::db::RepoRead;
        assert!(
            fx.owner
                .repo
                .conversation_read_frame(&fx.owner.card)
                .await
                .unwrap()
                .active_input
                .is_some(),
            "{scenario}"
        );
        fx.session.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn canonical_claude_replay_confirms_input_before_execution_end() {
    let fx = Fixture::new("replay-then-stall").await;
    fx.dispatch().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if matches!(
                fx.checkpoint().await.attempts[0].delivery,
                InputDeliveryFact::Accepted { .. }
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        fx.checkpoint().await.active.is_some(),
        "input acceptance does not end the execution"
    );
    assert!(fx.complete().await.is_none());
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_replay_before_dispatch_return_is_not_downgraded() {
    let fx = Arc::new(Fixture::new("exit").await);
    let pause = crate::claude_planner::session::SettlePause {
        entered: Arc::default(),
        release: Arc::default(),
    };
    fx.session.hold_dispatch_result_for_test(pause.clone());
    let dispatch = tokio::spawn({
        let fx = fx.clone();
        async move { fx.dispatch().await }
    });
    tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if matches!(
                fx.checkpoint().await.attempts[0].delivery,
                InputDeliveryFact::Accepted { .. }
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let before = fx.checkpoint().await.attempts;
    pause.release.notify_one();
    assert!(dispatch.await.unwrap().unwrap().report.outcome().is_ok());
    fx.wait_end().await;
    assert_eq!(fx.checkpoint().await.attempts, before);
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_acknowledgement_allows_next_human_input_without_keep() {
    let fx = Fixture::new("exit").await;
    let harness = fx.start_loop().await;
    fx.phase("turn_completed").await;
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
        .await
        .unwrap();
    let accepted = crate::conversation_inputs::accept_human_tx(
        &mut tx,
        &fx.source.current_principal().unwrap(),
        &mut carrier,
        HumanPayload {
            text: "following acknowledged input".into(),
            bound_attachments: vec![],
        },
    )
    .await
    .unwrap();
    let AcceptedInputRef::Human { message } = accepted.input_ref else {
        panic!("Human")
    };
    tx.commit().await.unwrap();
    harness
        .observe_canonical_source(message, fx.source.clone())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            let cp = fx.checkpoint().await;
            if cp.attempts.len() == 2 && cp.active.is_none() {
                assert!(
                    cp.attempts.iter().all(|attempt| matches!(
                        attempt.delivery,
                        InputDeliveryFact::Accepted { .. }
                    ))
                );
                assert!(cp.pending.is_empty());
                assert!(cp.receipts.is_empty());
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(fx.session.user_line_writes_for_test(), 2);
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_replay_records_on_failed_current_carrier_without_reactivation() {
    let fx = Fixture::new("hold-input-replay").await;
    fx.dispatch().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !fx.dir.path().join("replay-ready").exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    crate::db::sqlite::session_fail_if_active_runtime_tx(&mut tx, &fx.owner.worker)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    std::fs::write(fx.dir.path().join("release-replay"), "").unwrap();
    fx.wait_end().await;
    assert!(matches!(
        fx.checkpoint().await.attempts[0].delivery,
        InputDeliveryFact::Accepted { .. }
    ));
    let state: String = sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id=?")
        .bind(&fx.owner.worker)
        .fetch_one(fx.owner.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        state, "failed",
        "factual acknowledgement must not restore execution authority"
    );
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_failed_acknowledgement_write_preserves_uncertainty_and_end() {
    let fx = Fixture::new("exit").await;
    sqlx::query("CREATE TRIGGER fail_input_ack BEFORE UPDATE OF handle_state_json ON worker_sessions
        WHEN json_extract(NEW.handle_state_json,'$.canonical_inputs.attempts[0].delivery.kind')='accepted'
        BEGIN SELECT RAISE(ABORT,'ack failed'); END")
        .execute(fx.owner.repo.pool()).await.unwrap();
    fx.dispatch().await.unwrap();
    fx.wait_end().await;
    let cp = fx.checkpoint().await;
    assert!(cp.active.is_none());
    assert!(matches!(
        cp.attempts[0].delivery,
        InputDeliveryFact::Unknown
    ));
    assert!(cp.receipts.is_empty());
    fx.session.shutdown().await.unwrap();
}
