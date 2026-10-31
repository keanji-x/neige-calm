//! Carrier lifecycle preparation and ingress checks owned by the content fixture.
use super::*;
use crate::harness::run_loop::hibernation::{OwnerProofError, OwnerRefusal};

// A same-input serial ack waits for the running loop to finish publishing END.
// It supplies no new pending work; the typed owner remains the idle authority.
async fn observe_consumed_source(fx: &Fixture) {
    fx.harness
        .observe_canonical_source(fx.message.clone(), fx.source.clone())
        .await
        .unwrap();
}

#[tokio::test]
async fn carrier_quiescence_completion_waits_for_owner_publication() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    let thread = start(&fx, &mut peer, fx.message.clone(), "publication").await;
    let inner = &fx.harness.inner;
    let issuance = inner.issuance.lock().await;
    let running = fx.harness.state_for_test().await;
    assert!(
        matches!(&running, HarnessState::TurnRunning { turn_id, .. } if turn_id == "publication")
    );
    let park = inner.parked.park().unwrap();
    drop(issuance);
    let publication = inner.issuance_block.lock().await;
    complete(&fx, &mut peer, &thread, "publication").await;
    drop(park);
    let committed = fx.frame().await;
    assert_eq!(committed.header.phase, HarnessPhaseTag::TurnCompleted);
    assert!(committed.canonical_inputs.active.is_none());
    assert_eq!(fx.harness.state_for_test().await, running);
    let refusal = match fx.harness.idle_carrier_owner_typed().await {
        Err(error) => error,
        Ok(_) => panic!("unpublished completion cannot authorize an idle owner"),
    };
    assert!(
        matches!(
            &refusal,
            OwnerProofError::Refused {
                reason: OwnerRefusal::TurnInFlight,
                ..
            }
        ),
        "{refusal:?}"
    );
    let observed = observe_consumed_source(&fx);
    tokio::pin!(observed);
    assert!(futures_util::poll!(observed.as_mut()).is_pending());
    drop(publication);
    observed.await;
    let owner = fx.harness.idle_carrier_owner_typed().await.unwrap();
    owner.recheck_typed().await.unwrap();
    drop(owner);
    stop(&fx, &mut peer).await;
}

#[tokio::test]
async fn carrier_quiescence_parked_loop_keeps_ingress_polling() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    let thread = start(&fx, &mut peer, fx.message.clone(), "parked").await;
    complete(&fx, &mut peer, &thread, "parked").await;
    observe_consumed_source(&fx).await;
    let config = HarnessConfig::default();
    let sibling = Fixture::with_source(config, None, false, Some(fx.daemon.clone())).await;
    let owner = fx.harness.idle_carrier_owner_typed().await.unwrap();
    owner.recheck_typed().await.unwrap();
    let track = crate::per_card_lock::lock_key(
        &fx.harness.inner.track_delete_locks,
        fx.harness.inner.track_id.as_str(),
    )
    .await;
    tokio::time::timeout(
        Duration::from_secs(1),
        fx.harness.compact_canonical(fx.source.clone()),
    )
    .await
    .expect("parked compact must refuse without waiting for Track")
    .unwrap_err();
    let sibling_thread = start(&sibling, &mut peer, sibling.message.clone(), "sibling").await;
    let mut incoming = fx.daemon.subscribe_notifications();
    notify_thread(
        &fx.daemon,
        &mut peer,
        &thread,
        "turn/completed",
        json!({"id":"parked","status":"completed"}),
    )
    .await;
    assert!(
        matches!(incoming.try_recv(), Ok(Notification::TurnCompleted { .. })),
        "parking cannot exclude completion ingress"
    );
    observe_consumed_source(&fx).await;
    item(&fx, &mut peer, &thread, "parked", "during-park", "late").await;
    wait_item(&fx, "during-park").await;
    for n in 0..32 {
        let id = format!("sibling-{n}");
        item(
            &sibling,
            &mut peer,
            &sibling_thread,
            "sibling",
            &id,
            "reply",
        )
        .await;
        wait_item(&sibling, &id).await;
    }

    struct Refuse;
    impl crate::harness::held_requests::HeldResponder for Refuse {
        fn respond(self: Box<Self>, _: usize) {}
    }
    use crate::harness::held_requests::{ConnectionId, HeldRequestMessage, RequestKey};
    fx.harness
        .held_request_sender()
        .send(HeldRequestMessage::Open {
            request_key: RequestKey("parked-approval".into()),
            connection: ConnectionId("parked-source".into()),
            questions: vec![crate::event::AskQuestion {
                title: "Proceed?".into(),
                options: vec!["Yes".into(), "No".into()],
            }],
            responder: Box::new(Refuse),
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while fx.harness.held_requests().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("parked loop must persist approvals");
    assert!(
        matches!(
            owner.recheck_typed().await,
            Err(OwnerProofError::Refused {
                reason: OwnerRefusal::OpenAsk,
                ..
            })
        ),
        "held approval independently invalidates idle proof before new pending work"
    );
    let next = accept(&fx, "must stay pending").await;
    fx.harness
        .observe_canonical_source(next, fx.source.clone())
        .await
        .unwrap();
    let tx = begin_immediate_tx(fx.original.repo.db()).await.unwrap();
    tokio::time::timeout(Duration::from_millis(200), tick(&fx.harness.inner, true))
        .await
        .expect("parked tick must skip SQL entirely")
        .unwrap();
    tokio::time::timeout(
        Duration::from_millis(200),
        fx.harness.stop_canonical(fx.source.clone()),
    )
    .await
    .expect("parked Stop must skip SQL")
    .unwrap_err();
    tx.rollback().await.unwrap();
    assert_eq!(
        fx.frame().await.header.phase,
        HarnessPhaseTag::TurnCompleted
    );
    let parked = fx.frame().await;
    assert!(parked.canonical_inputs.active.is_none());
    assert_eq!(parked.canonical_inputs.pending.len(), 1);
    assert_eq!(parked.header.last_turn_id.as_deref(), Some("parked"));
    fx.harness.inner.shutting_down.store(true, Ordering::SeqCst);
    drop(track);
    drop(owner);
    complete(&sibling, &mut peer, &sibling_thread, "sibling").await;
    stop(&sibling, &mut peer).await;
    stop(&fx, &mut peer).await;
}

#[tokio::test]
async fn carrier_quiescence_ingress_writers_refuse_committed_replacement() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    // Native presentation requires a real crossing attempt, but no idle proof.
    let thread = start(&fx, &mut peer, fx.message.clone(), "retired").await;
    complete(&fx, &mut peer, &thread, "retired").await;
    let inner = &fx.harness.inner;
    let row = persist_native_item(
        inner,
        &thread,
        Some("retired"),
        "item/completed",
        &json!({"item":{"id":"before-rotation","type":"agentMessage","text":"before"}}),
    )
    .await
    .unwrap()
    .unwrap();
    row.publish(inner);
    let mut tx = begin_immediate_tx(fx.original.repo.db()).await.unwrap();
    crate::db::sqlite::session_supersede_and_start_tx(
        &mut tx,
        &inner.worker_session_id,
        crate::session_projection_repo::WorkerSessionInit {
            id: "replacement".into(),
            card_id: inner.card_id.to_string(),
            kind: crate::session_projection_repo::WorkerSessionKind::SharedPlanner,
            agent_provider: Some(crate::session_projection_repo::AgentProvider::Codex),
            status: crate::session_projection_repo::WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(
                serde_json::to_value(CanonicalHarnessSnapshot::dormant(0)).unwrap(),
            ),
            spawn_op_id: None,
            now_ms: crate::model::now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let terminal_refused = crate::harness::turn_outcome::record(
        inner.repo.as_ref(),
        &inner.worker_session_id,
        inner.card_id.as_str(),
        inner.track_id.as_str(),
        &thread,
        "after-rotation",
        &json!({"id":"after-rotation","status":"completed"}),
        crate::harness::turn_outcome::Confirmation::ProviderTerminal,
    )
    .await
    .is_err();
    let presentation_refused = persist_native_item(
        inner,
        &thread,
        Some("retired"),
        "item/completed",
        &json!({"item":{"id":"before-rotation","type":"agentMessage","text":"before"}}),
    )
    .await
    .is_err();
    let legacy_refused = insert_item_row(
        inner,
        &thread,
        Some("after-rotation"),
        Some("after-rotation"),
        Some("agentMessage"),
        "item/completed",
        "{}",
        None,
    )
    .await
    .is_err();
    assert_eq!(
        (terminal_refused, presentation_refused, legacy_refused),
        (true, true, true)
    );
    let rows = inner
        .repo
        .transcript_rows_of_thread(inner.card_id.as_str(), &thread)
        .await
        .unwrap();
    assert!(
        rows.iter()
            .all(|row| row.turn_id.as_deref() != Some("after-rotation"))
    );
    stop(&fx, &mut peer).await;
}

#[tokio::test]
async fn carrier_quiescence_unpark_retries_work_deferred_before_observation() {
    use crate::test_seams::{PausePoint, install_pause_for_test};
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    let thread = start(&fx, &mut peer, fx.message.clone(), "before-park").await;
    let card = fx.harness.inner.card_id.as_str();
    let idle = PausePoint::default();
    install_pause_for_test("canonical-idle-wait", card, idle.clone());
    complete(&fx, &mut peer, &thread, "before-park").await;
    tokio::time::timeout(Duration::from_secs(5), idle.entered.notified())
        .await
        .unwrap();
    let owner = fx.harness.idle_carrier_owner_typed().await.unwrap();
    let deferred = PausePoint::default();
    install_pause_for_test("canonical-tick-returned", card, deferred.clone());
    let message = accept(&fx, "pending while parked").await;
    let observed = fx
        .harness
        .observe_canonical_source(message, fx.source.clone());
    tokio::pin!(observed);
    idle.release.notify_one();
    observed.await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), deferred.entered.notified())
        .await
        .unwrap();
    // Release after the pass skipped observation but before pacing is chosen.
    // Current flags alone cannot establish whether the idle cache was refreshed.
    drop(owner);
    deferred.release.notify_one();
    let call = tokio::time::timeout(Duration::from_secs(3), next_start(&mut peer))
        .await
        .expect("unparked pending work must not wait for idle reconciliation");
    assert!(
        call["params"]["input"]
            .to_string()
            .contains("pending while parked")
    );
    notify_thread(
        &fx.daemon,
        &mut peer,
        &thread,
        "turn/started",
        json!({"id":"after-park"}),
    )
    .await;
    send(
        &mut peer,
        json!({"id":call["id"],"result":{"turn":{"id":"after-park"}}}),
    )
    .await;
    fx.wait_phase(HarnessPhaseTag::TurnRunning).await;
    complete(&fx, &mut peer, &thread, "after-park").await;
    stop(&fx, &mut peer).await;
}
