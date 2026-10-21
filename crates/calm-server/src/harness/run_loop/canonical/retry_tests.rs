//! Transport/scheduler tests for an internally committed intent. Truth's tests
//! exercise the real private resolution transaction; no HTTP activation is claimed.
use super::*;
use crate::db::sqlite::begin_immediate_tx;
use crate::shared_codex_appserver::native_owner_tests::{notify_thread, request, send};
use calm_types::conversation::*;
use calm_types::organization::LEGACY_OWNER_USER_ID;
use serde_json::json;

mod fixture;
use fixture::RetryFixture;

#[tokio::test]
async fn canonical_retry_waits_for_exact_thread_settled_activity() {
    let (r, mut peer) = RetryFixture::new().await;
    r.attach().await;
    r.read_activity(&mut peer, "active").await;
    let next = tokio::time::timeout(Duration::from_secs(5), request(&mut peer))
        .await
        .unwrap();
    assert_eq!(next["method"], "thread/read");
    let held = r.fx.frame().await;
    assert_eq!(held.canonical_inputs.pending.len(), 1);
    assert_eq!(
        held.canonical_inputs
            .active
            .as_ref()
            .unwrap()
            .submission_ids,
        vec![r.predecessor.submission_id.clone()]
    );
    assert_eq!(held.canonical_inputs.attempts.len(), 1);
    peer.close(None).await.unwrap();
    r.fx.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_retry_uses_frozen_model_permission_after_selection_change() {
    let (r, mut peer) = RetryFixture::new().await;
    let mut tx = begin_immediate_tx(r.fx.original.repo.db()).await.unwrap();
    let card = r.fx.harness.inner.card_id.as_str();
    let mut payload = crate::operation::codex_adapter::card_payload_get_tx(&mut tx, card)
        .await
        .unwrap();
    crate::planner_model::CardModelSelection::apply_to_payload(
        payload.as_object_mut().unwrap(),
        Some("different-current-model"),
        Some("low"),
    );
    crate::db::sqlite::card_update_tx(
        &mut tx,
        card,
        crate::model::CardPatch {
            title: None,
            kind: None,
            sort: None,
            payload: Some(payload),
            deletable: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    r.attach().await;
    let call = r.start(&mut peer).await;
    let thread = r.fx.frame().await.header.last_thread_id.unwrap();
    send(
        &mut peer,
        json!({"id":call["id"],"result":{"turn":{"id":"retry-native"}}}),
    )
    .await;
    r.fx.wait_phase(HarnessPhaseTag::TurnRunning).await;
    notify_thread(
        &r.fx.daemon,
        &mut peer,
        &thread,
        "turn/completed",
        json!({"id":"retry-native","status":"completed"}),
    )
    .await;
    r.fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
    let current = r.fx.frame().await;
    assert!(matches!(
        current.canonical_inputs.attempts[0].delivery,
        InputDeliveryFact::CrossingUnknown
    ));
    assert!(matches!(
        current.canonical_inputs.attempts[1].delivery,
        InputDeliveryFact::Accepted { .. }
    ));
    peer.close(None).await.unwrap();
    r.fx.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_retry_reopen_requires_live_repeat_source() {
    let (r, mut peer) = RetryFixture::new().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(120), request(&mut peer))
            .await
            .is_err()
    );
    let held = r.fx.frame().await;
    assert_eq!(held.canonical_inputs.pending.len(), 1);
    assert_eq!(held.canonical_inputs.attempts.len(), 1);
    r.fx.auth.sessions.remove(&r.fx.session).unwrap();
    assert!(
        r.fx.harness
            .observe_canonical_source(r.identity.clone(), r.fx.source.clone())
            .await
            .is_err()
    );
    assert_eq!(r.fx.frame().await, held);
    peer.close(None).await.unwrap();
    r.fx.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_retry_erasure_between_resolve_and_send_has_zero_effects() {
    let (r, mut peer) = RetryFixture::new().await;
    r.attach().await;
    let call = request(&mut peer).await;
    assert_eq!(call["method"], "thread/read");
    let mut tx = begin_immediate_tx(r.fx.original.repo.db()).await.unwrap();
    sqlx::query("UPDATE conversation_message_revisions SET payload_json=?1 WHERE message_id=?2")
        .bind(
            serde_json::to_string(
                &PrivateHistoryPayload::<HumanPayload>::ErasedByPrivateHistoryOperation {
                    operation_id: PrivateHistoryOperationId::parse(&"a".repeat(32)).unwrap(),
                },
            )
            .unwrap(),
        )
        .bind(r.fx.message.message_id.as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let thread = r.fx.frame().await.header.last_thread_id.unwrap();
    send(&mut peer,json!({"id":call["id"],"result":{"thread":{"id":thread,"status":{"type":"idle"},"turns":[]}}})).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), request(&mut peer))
            .await
            .is_err()
    );
    let held = r.fx.frame().await;
    assert_eq!(held.canonical_inputs.pending.len(), 1);
    assert_eq!(held.canonical_inputs.attempts.len(), 1);
    peer.close(None).await.unwrap();
    r.fx.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_retry_rechecks_activity_and_source_after_gate_await() {
    for change in ["activity", "logout", "erasure"] {
        let (r, mut peer) = RetryFixture::new().await;
        r.attach().await;
        r.read_activity(&mut peer, "idle").await;
        let activity = request(&mut peer).await;
        assert_eq!(activity["method"], "thread/read");
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let blocker = tokio::spawn({
            let client = r.client.clone();
            let (entered, release) = (entered.clone(), release.clone());
            async move {
                client
                    .config_read_gated(
                        None,
                        tokio::time::Instant::now() + Duration::from_secs(10),
                        &provider::send_boundary::PreSendGate::new(|| {
                            Box::pin(async {
                                entered.notify_one();
                                release.notified().await;
                                Ok(())
                            })
                        }),
                    )
                    .await
            }
        });
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        let thread = r.fx.frame().await.header.last_thread_id.unwrap();
        send(&mut peer,json!({"id":activity["id"],"result":{"thread":{"id":thread,"status":{"type":"idle"},"turns":[]}}})).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            // Original start, blocking config read and successor start. The
            // successor is now waiting on the real outbound gate after its idle read.
            while r.client.pending_requests_for_test() < 3 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        match change {
            "activity" => {
                notify_thread(
                    &r.fx.daemon,
                    &mut peer,
                    &thread,
                    "turn/started",
                    json!({"id":"late-active"}),
                )
                .await
            }
            "logout" => {
                r.fx.auth.sessions.remove(&r.fx.session).unwrap();
            }
            "erasure" => {
                sqlx::query(
                    "UPDATE conversation_message_revisions SET payload_json=?1 WHERE message_id=?2",
                )
                .bind(
                    serde_json::to_string(
                        &PrivateHistoryPayload::<HumanPayload>::ErasedByPrivateHistoryOperation {
                            operation_id: PrivateHistoryOperationId::parse(&"a".repeat(32))
                                .unwrap(),
                        },
                    )
                    .unwrap(),
                )
                .bind(r.fx.message.message_id.as_str())
                .execute(r.fx.original.repo.pool())
                .await
                .unwrap();
            }
            _ => panic!("closed test cases"),
        }
        release.notify_one();
        let call = request(&mut peer).await;
        assert_eq!(call["method"], "config/read");
        send(&mut peer, json!({"id":call["id"],"result":{"config":{}}})).await;
        blocker.await.unwrap().unwrap();
        let held = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let frame = r.fx.frame().await;
                if frame.canonical_inputs.attempts.len() == 2
                    && matches!(
                        frame.canonical_inputs.attempts[1].delivery,
                        InputDeliveryFact::NotDispatched { .. }
                    )
                {
                    break frame;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(held.canonical_inputs.pending.len(), 1, "{change}");
        assert_eq!(
            held.canonical_inputs.pending[0].identity(),
            r.identity,
            "{change}"
        );
        assert!(held.canonical_inputs.active.is_none(), "{change}");
        assert!(
            matches!(
                held.canonical_inputs.attempts[0].delivery,
                InputDeliveryFact::CrossingUnknown
            ),
            "{change}"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(120), request(&mut peer))
                .await
                .is_err(),
            "no successor bytes may cross after {change}"
        );
        peer.close(None).await.unwrap();
        r.fx.harness.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn canonical_retry_predecessor_late_facts_do_not_settle_successor() {
    let (mut r, mut peer) = RetryFixture::new().await;
    r.attach().await;
    let successor = r.start(&mut peer).await;
    let thread = r.fx.frame().await.header.last_thread_id.unwrap();
    let active = r.fx.frame().await.canonical_inputs.active.unwrap();
    send(
        &mut peer,
        json!({"id":r.original_call["id"],"result":{"turn":{"id":"predecessor-native"}}}),
    )
    .await;
    let original = (&mut r.original_dispatch).await.unwrap().unwrap();
    assert!(!original.consumed.contains(&r.identity));
    let after_ack = r.fx.frame().await;
    assert_eq!(after_ack.canonical_inputs.active.as_ref(), Some(&active));
    assert!(matches!(
        after_ack.canonical_inputs.attempts[0].delivery,
        InputDeliveryFact::Accepted { .. }
    ));
    assert!(matches!(
        after_ack.canonical_inputs.attempts[1].delivery,
        InputDeliveryFact::CrossingUnknown
    ));
    assert!(
        r.fx.harness
            .inner
            .inputs
            .canonical()
            .unwrap()
            .source(&r.identity)
            .await
            .is_some()
    );
    send(&mut peer,json!({"method":"item/completed","params":{"threadId":thread,"turnId":"predecessor-native","item":{"id":"late-predecessor-echo","type":"userMessage","clientId":r.predecessor.submission_id.as_str(),"content":[]}}})).await;
    notify_thread(
        &r.fx.daemon,
        &mut peer,
        &thread,
        "fixture/echo-synchronized",
        json!({}),
    )
    .await;
    assert!(
        crate::harness::input_dispatch::complete_once(
            r.fx.original.repo.as_ref(),
            &r.fx.harness.inner.backend,
            r.fx.harness.inner.card_id.as_str(),
            &r.fx.harness.inner.worker_session_id
        )
        .await
        .unwrap()
        .is_none()
    );
    let after_echo = r.fx.frame().await;
    assert_eq!(after_echo.canonical_inputs.active.as_ref(), Some(&active));
    assert!(matches!(
        after_echo.canonical_inputs.attempts[1].delivery,
        InputDeliveryFact::CrossingUnknown
    ));
    notify_thread(
        &r.fx.daemon,
        &mut peer,
        &thread,
        "turn/completed",
        json!({"id":"predecessor-native","status":"completed"}),
    )
    .await;
    assert!(
        crate::harness::input_dispatch::complete_once(
            r.fx.original.repo.as_ref(),
            &r.fx.harness.inner.backend,
            r.fx.harness.inner.card_id.as_str(),
            &r.fx.harness.inner.worker_session_id
        )
        .await
        .unwrap()
        .is_none()
    );
    let after_end = r.fx.frame().await;
    assert_eq!(after_end.header.phase, HarnessPhaseTag::IssuingTurn);
    assert_eq!(after_end.canonical_inputs.active.as_ref(), Some(&active));
    assert!(matches!(
        after_end.canonical_inputs.attempts[1].delivery,
        InputDeliveryFact::CrossingUnknown
    ));
    send(
        &mut peer,
        json!({"id":successor["id"],"result":{"turn":{"id":"successor-native"}}}),
    )
    .await;
    r.fx.wait_phase(HarnessPhaseTag::TurnRunning).await;
    notify_thread(
        &r.fx.daemon,
        &mut peer,
        &thread,
        "turn/completed",
        json!({"id":"successor-native","status":"completed"}),
    )
    .await;
    r.fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
    peer.close(None).await.unwrap();
    r.fx.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_retry_unknown_successor_holds_again() {
    let (r, mut peer) = RetryFixture::new().await;
    r.attach().await;
    let _sent = r.start(&mut peer).await;
    peer.close(None).await.unwrap();
    let held = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let current = r.fx.frame().await;
            if current.canonical_inputs.attempts.len() == 2
                && matches!(
                    current.canonical_inputs.attempts[1].delivery,
                    InputDeliveryFact::Unknown
                )
            {
                break current;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(held.header.phase, HarnessPhaseTag::IssuingTurn);
    assert!(held.canonical_inputs.pending.is_empty());
    assert_eq!(held.canonical_inputs.receipts.len(), 1);
    assert!(
        !held
            .canonical_inputs
            .is_human_resolved(&held.canonical_inputs.attempts[1].attempt)
    );
    assert_eq!(
        held.canonical_inputs
            .active
            .as_ref()
            .unwrap()
            .submission_ids,
        vec![
            held.canonical_inputs.attempts[1]
                .attempt
                .submission_id
                .clone()
        ]
    );
    r.fx.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_retry_system_plan_retains_provenance_under_human_repeat() {
    let (r, mut peer) = RetryFixture::with_system(true).await;
    r.attach().await;
    let call = r.start(&mut peer).await;
    assert!(
        call["params"]["input"]
            .to_string()
            .contains("frozen System provenance")
    );
    let current = r.fx.frame().await;
    let id = current.canonical_inputs.active.unwrap().submission_ids[0].clone();
    let mut tx = begin_immediate_tx(r.fx.original.repo.db()).await.unwrap();
    let stored = crate::db::sqlite::conversation::submission_plan_tx(&mut tx, &id)
        .await
        .unwrap();
    let refs = stored.plan.input_refs().unwrap();
    assert!(matches!(
        refs.as_slice(),
        [
            AcceptedInputRef::Human { .. },
            AcceptedInputRef::System { .. }
        ]
    ));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversation_messages WHERE card_id=?1")
            .bind(r.fx.harness.inner.card_id.as_str())
            .fetch_one(&mut **tx)
            .await
            .unwrap(),
        1,
        "a Human repeat must not create a Human copy of System provenance"
    );
    tx.commit().await.unwrap();
    send(
        &mut peer,
        json!({"id":call["id"],"result":{"turn":{"id":"mixed-repeat"}}}),
    )
    .await;
    r.fx.wait_phase(HarnessPhaseTag::TurnRunning).await;
    let thread = r.fx.frame().await.header.last_thread_id.unwrap();
    notify_thread(
        &r.fx.daemon,
        &mut peer,
        &thread,
        "turn/completed",
        json!({"id":"mixed-repeat","status":"completed"}),
    )
    .await;
    r.fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
    peer.close(None).await.unwrap();
    r.fx.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_retry_recovery_qualifies_intent_and_resumes_mixed_unattempted_claim() {
    let (r, mut peer) = RetryFixture::with_system(true).await;
    let inner = &r.fx.harness.inner;
    assert!(
        inner
            .inputs
            .canonical()
            .unwrap()
            .source(&r.identity)
            .await
            .is_none()
    );
    let mut tx = begin_immediate_tx(r.fx.original.repo.db()).await.unwrap();
    let mut carrier =
        current_input_carrier_tx(&mut tx, inner.card_id.as_str(), &inner.worker_session_id)
            .await
            .unwrap();
    let human = InputSource::from(&r.fx.source);
    let queued = crate::conversation_inputs::source_inputs_tx(&mut tx, &human, &carrier)
        .await
        .unwrap();
    assert_eq!(queued.identities(), std::slice::from_ref(&r.identity));
    assert!(!queued.is_unattempted_initial());
    let CanonicalPendingIdentity::ExplicitSuccessor { retry_intent_id } = &r.identity else {
        panic!("retry identity")
    };
    let scoped = human.for_identity(&r.identity);
    let claim = crate::conversation_inputs::claim_initial_tx(
        &mut tx,
        &scoped,
        &mut carrier,
        crate::db::sqlite::conversation::InitialClaim::ExplicitSuccessor {
            retry_intent_id: retry_intent_id.clone(),
        },
    )
    .await
    .unwrap();
    let recovered = crate::conversation_inputs::source_inputs_tx(&mut tx, &human, &carrier)
        .await
        .unwrap();
    assert_eq!(
        recovered.identities(),
        std::slice::from_ref(&r.identity),
        "mixed root provenance is not a source registry key"
    );
    assert!(recovered.is_unattempted_initial());
    tx.commit().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(120), request(&mut peer))
            .await
            .is_err(),
        "persisted principal/receipt cannot recreate a live repeat source"
    );
    r.attach().await;
    r.read_activity(&mut peer, "idle").await;
    let call = request(&mut peer).await;
    assert_eq!(
        call["method"], "turn/start",
        "resume must skip current config/catalog reads"
    );
    assert_eq!(call["params"]["model"], "frozen-retry-model");
    assert!(
        call["params"]["input"]
            .to_string()
            .contains("frozen System provenance")
    );
    assert_eq!(
        r.fx.frame()
            .await
            .canonical_inputs
            .active
            .as_ref()
            .unwrap()
            .submission_ids,
        vec![claim.submission_id]
    );
    send(
        &mut peer,
        json!({"id":call["id"],"result":{"turn":{"id":"resumed-repeat"}}}),
    )
    .await;
    r.fx.wait_phase(HarnessPhaseTag::TurnRunning).await;
    let thread = r.fx.frame().await.header.last_thread_id.unwrap();
    notify_thread(
        &r.fx.daemon,
        &mut peer,
        &thread,
        "turn/completed",
        json!({"id":"resumed-repeat","status":"completed"}),
    )
    .await;
    r.fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
    peer.close(None).await.unwrap();
    r.fx.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_retry_late_end_before_successor_ack_preserves_successor_end() {
    #[derive(Clone, Copy)]
    enum Order {
        EndBeforeReply,
        EndAfterReply,
        MultipleCandidates,
        StartAfterReply,
    }
    for order in [
        Order::EndBeforeReply,
        Order::EndAfterReply,
        Order::MultipleCandidates,
        Order::StartAfterReply,
    ] {
        let (mut r, mut peer) = RetryFixture::new().await;
        r.attach().await;
        let successor = r.start(&mut peer).await;
        let thread = r.fx.frame().await.header.last_thread_id.unwrap();
        match order {
            Order::EndBeforeReply => {
                notify_thread(
                    &r.fx.daemon,
                    &mut peer,
                    &thread,
                    "turn/completed",
                    json!({"id":"predecessor-native","status":"completed"}),
                )
                .await
            }
            Order::EndAfterReply => {
                notify_thread(
                    &r.fx.daemon,
                    &mut peer,
                    &thread,
                    "turn/started",
                    json!({"id":"predecessor-native"}),
                )
                .await
            }
            Order::MultipleCandidates => {
                notify_thread(
                    &r.fx.daemon,
                    &mut peer,
                    &thread,
                    "turn/started",
                    json!({"id":"successor-native"}),
                )
                .await;
                notify_thread(
                    &r.fx.daemon,
                    &mut peer,
                    &thread,
                    "turn/completed",
                    json!({"id":"successor-native","status":"completed"}),
                )
                .await;
                notify_thread(
                    &r.fx.daemon,
                    &mut peer,
                    &thread,
                    "turn/started",
                    json!({"id":"predecessor-native"}),
                )
                .await;
                notify_thread(
                    &r.fx.daemon,
                    &mut peer,
                    &thread,
                    "turn/completed",
                    json!({"id":"predecessor-native","status":"completed"}),
                )
                .await;
            }
            Order::StartAfterReply => {}
        }
        send(
            &mut peer,
            json!({"id":successor["id"],"result":{"turn":{"id":"successor-native"}}}),
        )
        .await;
        if matches!(order, Order::MultipleCandidates) {
            r.fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
        } else {
            r.fx.wait_phase(HarnessPhaseTag::TurnRunning).await;
        }
        if matches!(order, Order::StartAfterReply) {
            notify_thread(
                &r.fx.daemon,
                &mut peer,
                &thread,
                "turn/started",
                json!({"id":"predecessor-native"}),
            )
            .await;
        }
        send(
            &mut peer,
            json!({"id":r.original_call["id"],"result":{"turn":{"id":"predecessor-native"}}}),
        )
        .await;
        (&mut r.original_dispatch).await.unwrap().unwrap();
        let current = r.fx.frame().await;
        assert!(matches!(
            current.canonical_inputs.attempts[1].delivery,
            InputDeliveryFact::Accepted { .. }
        ));
        if !matches!(order, Order::MultipleCandidates) {
            assert_eq!(current.header.phase, HarnessPhaseTag::TurnRunning);
            notify_thread(
                &r.fx.daemon,
                &mut peer,
                &thread,
                "turn/completed",
                json!({"id":"successor-native","status":"completed"}),
            )
            .await;
            r.fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
        }
        if matches!(order, Order::EndAfterReply | Order::StartAfterReply) {
            // The successor's own END completes it before this predecessor END.
            notify_thread(
                &r.fx.daemon,
                &mut peer,
                &thread,
                "turn/completed",
                json!({"id":"predecessor-native","status":"completed"}),
            )
            .await;
        }
        assert_eq!(
            r.fx.frame().await.header.phase,
            HarnessPhaseTag::TurnCompleted
        );
        peer.close(None).await.unwrap();
        r.fx.harness.shutdown().await.unwrap();
    }
}
