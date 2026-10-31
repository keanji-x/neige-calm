//! Canonical maintenance through the real transport and persisted runtime.
use super::*;
use crate::harness::compact_outcome::CompactStatus;
use crate::shared_codex_appserver::native_owner_tests::{Peer, assert_no_request};

pub(super) async fn ready() -> (Fixture, Arc<crate::codex_appserver::CodexAppServer>, Peer) {
    let fx = Fixture::new().await;
    let (client, mut peer) = connect(&fx.daemon).await;
    let thread = super::content_tests::start(&fx, &mut peer, fx.message.clone(), "user").await;
    super::content_tests::complete(&fx, &mut peer, &thread, "user").await;
    (fx, client, peer)
}

fn compact(fx: &Fixture) -> tokio::task::JoinHandle<Result<()>> {
    let harness = fx.harness.clone();
    let source = fx.source.clone();
    tokio::spawn(async move { harness.compact_canonical(source).await })
}

pub(super) async fn shutdown(harness: &PlannerHarness, peer: &mut Peer) {
    let handle = harness.clone();
    let mut task = tokio::spawn(async move { handle.shutdown().await });
    loop {
        tokio::select! {
            done = &mut task => { done.unwrap().unwrap(); return; }
            call = request(peer) => {
                assert_eq!(call["method"], "turn/interrupt", "shutdown must never submit compact");
                send(peer, json!({"id":call["id"],"result":{}})).await;
            }
        }
    }
}

#[tokio::test]
async fn canonical_codex_compact_positive_compact_and_settle_preserves_user() {
    for early in [true, false] {
        let (fx, _client, mut peer) = ready().await;
        let before = fx.frame().await;
        let task = compact(&fx);
        let call = request(&mut peer).await;
        assert_eq!(call["method"], "thread/compact/start");
        let thread = before.header.last_thread_id.as_deref().unwrap();
        assert_eq!(call["params"], json!({"threadId":thread}));
        assert_eq!(fx.frame().await.header.phase, HarnessPhaseTag::Compacting);
        if early {
            notify_thread(
                &fx.daemon,
                &mut peer,
                thread,
                "turn/started",
                json!({"id":"maintenance"}),
            )
            .await;
            notify_thread(
                &fx.daemon,
                &mut peer,
                thread,
                "turn/completed",
                json!({"id":"maintenance","status":"completed"}),
            )
            .await;
        }
        send(&mut peer, json!({"id":call["id"],"result":{}})).await;
        task.await.unwrap().unwrap();
        if !early {
            assert_eq!(
                fx.frame().await.header.phase,
                HarnessPhaseTag::Compacting,
                "ACK is not END"
            );
            notify_thread(
                &fx.daemon,
                &mut peer,
                thread,
                "turn/started",
                json!({"id":"maintenance"}),
            )
            .await;
            notify_thread(
                &fx.daemon,
                &mut peer,
                thread,
                "turn/completed",
                json!({"id":"maintenance","status":"completed"}),
            )
            .await;
        }
        let done = fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
        assert_eq!(
            done.header.compact_outcome.unwrap().status,
            CompactStatus::Completed
        );
        assert_eq!(done.header.last_turn_id, before.header.last_turn_id);
        assert_eq!(done.canonical_inputs, before.canonical_inputs);
        assert!(done.header.context_usage_invalidated);
        shutdown(&fx.harness, &mut peer).await;
    }
}

#[tokio::test]
async fn canonical_codex_compact_refused_restores_phase() {
    let (fx, client, mut peer) = ready().await;
    let before = fx.frame().await;
    let blocked = client.lock_outbound_sink_for_test().await;
    let mut task = compact(&fx);
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.pending_requests_for_test() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("compact must queue behind the real outbound sink");
    assert_eq!(fx.frame().await.header.phase, HarnessPhaseTag::Compacting);
    fx.auth.sessions.remove(&fx.session).unwrap();
    drop(blocked);
    let (result, unexpected) = tokio::select! {
        done = &mut task => (done.unwrap(), None),
        call = request(&mut peer) => {
            send(&mut peer, json!({"id":call["id"],"result":{}})).await;
            (task.await.unwrap(), Some(call))
        }
    };
    assert!(
        unexpected.is_none(),
        "revoked maintenance reached the wire: {unexpected:?}"
    );
    assert!(result.is_err());
    assert_eq!(
        fx.frame().await.header.compact_outcome.unwrap().status,
        CompactStatus::Refused
    );
    assert_eq!(fx.frame().await.header.phase, before.header.phase);
    assert_no_request(&mut peer).await;
    shutdown(&fx.harness, &mut peer).await;
}

#[tokio::test]
async fn canonical_codex_compact_unconfirmed_wedges() {
    let (fx, _client, mut peer) = ready().await;
    let task = compact(&fx);
    let call = request(&mut peer).await;
    assert_eq!(call["method"], "thread/compact/start");
    send(&mut peer, json!({"id":call["id"],"result":null})).await;
    assert!(task.await.unwrap().is_err());
    let held = fx.wait_phase(HarnessPhaseTag::Wedged).await;
    assert_eq!(
        held.header.compact_outcome.unwrap().status,
        CompactStatus::Unconfirmed
    );
    assert_no_request(&mut peer).await;
    shutdown(&fx.harness, &mut peer).await;
}

#[tokio::test]
async fn canonical_codex_compact_restart_never_resubmits() {
    let (fx, _client, mut peer) = ready().await;
    let task = compact(&fx);
    let call = request(&mut peer).await;
    assert_eq!(call["method"], "thread/compact/start");
    send(&mut peer, json!({"id":call["id"],"result":{}})).await;
    task.await.unwrap().unwrap();
    let inner = &fx.harness.inner;
    shutdown(&fx.harness, &mut peer).await;
    let restored = PlannerHarness::run(PlannerHarnessParams {
        track_delete_locks: Default::default(),
        worker_session_id: inner.worker_session_id.clone(),
        track_id: inner.track_id.clone(),
        card_id: inner.card_id.clone(),
        thread_id: inner.thread_id.read().await.clone(),
        repo: inner.repo.clone(),
        events: inner.events.clone(),
        card_role_cache: inner.card_role_cache.clone(),
        track_area_cache: inner.track_area_cache.clone(),
        backend: inner.backend.clone(),
        registry: Default::default(),
        config: HarnessConfig::default(),
        snapshot: HarnessRuntimeSnapshot::decode(serde_json::to_value(fx.frame().await).unwrap())
            .unwrap(),
    })
    .await
    .unwrap();
    let header = restored.runtime_header().await.unwrap();
    assert_eq!(header.phase, HarnessPhaseTag::Wedged);
    assert_eq!(
        header.compact_outcome.unwrap().status,
        CompactStatus::Unconfirmed
    );
    assert_no_request(&mut peer).await;
    shutdown(&restored, &mut peer).await;
}

#[tokio::test]
async fn canonical_codex_compact_settle_failed_wedges_after_exact_boundary() {
    let (fx, _client, mut peer) = ready().await;
    let before = fx.frame().await;
    let thread = before.header.last_thread_id.as_deref().unwrap();
    let task = compact(&fx);
    let call = request(&mut peer).await;
    assert_eq!(call["method"], "thread/compact/start");
    send(&mut peer, json!({"id":call["id"],"result":{}})).await;
    task.await.unwrap().unwrap();
    notify_thread(
        &fx.daemon,
        &mut peer,
        thread,
        "turn/started",
        json!({"id":"maintenance"}),
    )
    .await;
    super::content_tests::notify(&fx, &mut peer, "item/completed", json!({
        "threadId":thread,"turnId":"other-turn","item":{"id":"wrong-boundary","type":"contextCompaction"}
    })).await;
    super::content_tests::wait_item(&fx, "wrong-boundary").await;
    assert!(!fx.frame().await.header.context_usage_invalidated);
    super::content_tests::notify(&fx, &mut peer, "item/completed", json!({
        "threadId":thread,"turnId":"maintenance","item":{"id":"boundary","type":"contextCompaction"}
    })).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !fx.frame().await.header.context_usage_invalidated {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    notify_thread(
        &fx.daemon,
        &mut peer,
        thread,
        "turn/completed",
        json!({"id":"maintenance","status":"failed"}),
    )
    .await;
    let done = fx.wait_phase(HarnessPhaseTag::Wedged).await;
    assert_eq!(
        done.header.compact_outcome.unwrap().status,
        CompactStatus::Unconfirmed
    );
    assert_eq!(done.header.last_turn_id, before.header.last_turn_id);
    assert_eq!(done.canonical_inputs, before.canonical_inputs);
    assert!(done.header.context_usage_invalidated);
    assert_no_request(&mut peer).await;
    shutdown(&fx.harness, &mut peer).await;
}

#[tokio::test]
async fn canonical_codex_compact_ambiguous_native_starts_cannot_supply_a_boundary() {
    let (fx, _client, mut peer) = ready().await;
    let before = fx.frame().await;
    let thread = before.header.last_thread_id.as_deref().unwrap();
    let task = compact(&fx);
    let call = request(&mut peer).await;
    for id in ["candidate-one", "candidate-two"] {
        notify_thread(
            &fx.daemon,
            &mut peer,
            thread,
            "turn/started",
            json!({"id":id}),
        )
        .await;
    }
    send(&mut peer, json!({"id":call["id"],"result":{}})).await;
    task.await.unwrap().unwrap();
    let held = fx.wait_phase(HarnessPhaseTag::Wedged).await;
    assert!(held.header.compact_outcome.unwrap().turn_id.is_none());
    super::content_tests::notify(&fx, &mut peer, "item/completed", json!({
        "threadId":thread,"turnId":"candidate-one","item":{"id":"ambiguous-boundary","type":"contextCompaction"}
    })).await;
    super::content_tests::wait_item(&fx, "ambiguous-boundary").await;
    assert!(!fx.frame().await.header.context_usage_invalidated);
    shutdown(&fx.harness, &mut peer).await;
}

#[tokio::test]
async fn canonical_codex_compact_lost_source_wedges_without_resubmitting() {
    let (fx, client, mut peer) = ready().await;
    let task = compact(&fx);
    let call = request(&mut peer).await;
    send(&mut peer, json!({"id":call["id"],"result":{}})).await;
    task.await.unwrap().unwrap();
    peer.close(None).await.unwrap();
    crate::shared_codex_appserver::native_owner_tests::reader_finished(&client).await;
    let held = fx.wait_phase(HarnessPhaseTag::Wedged).await;
    assert_eq!(
        held.header.compact_outcome.unwrap().status,
        CompactStatus::Unconfirmed
    );
    fx.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_codex_compact_refused_native_activity_cannot_become_maintenance() {
    let (fx, client, mut peer) = ready().await;
    let before = fx.frame().await;
    let thread = before.header.last_thread_id.as_deref().unwrap();
    let blocked = client.lock_outbound_sink_for_test().await;
    let mut task = compact(&fx);
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.pending_requests_for_test() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    notify_thread(
        &fx.daemon,
        &mut peer,
        thread,
        "turn/started",
        json!({"id":"foreign"}),
    )
    .await;
    drop(blocked);
    let (result, unexpected) = tokio::select! {
        done = &mut task => (done.unwrap(), None),
        call = request(&mut peer) => {
            send(&mut peer, json!({"id":call["id"],"result":{}})).await;
            (task.await.unwrap(), Some(call))
        }
    };
    assert!(
        unexpected.is_none(),
        "native activity must fence maintenance: {unexpected:?}"
    );
    assert!(result.is_err());
    fx.wait_phase(HarnessPhaseTag::Wedged).await;
    super::content_tests::notify(&fx, &mut peer, "item/completed", json!({
        "threadId":thread,"turnId":"foreign","item":{"id":"foreign-boundary","type":"contextCompaction"}
    })).await;
    super::content_tests::wait_item(&fx, "foreign-boundary").await;
    assert!(
        !fx.frame().await.header.context_usage_invalidated,
        "a turn preceding a proven NotWritten compact cannot become maintenance"
    );
    assert_no_request(&mut peer).await;
    shutdown(&fx.harness, &mut peer).await;
}

pub(super) async fn accept_during_compact(fx: &Fixture) -> CanonicalHarnessSnapshot {
    let mut tx = begin_immediate_tx(fx.original.repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(
        &mut tx,
        fx.harness.inner.card_id.as_str(),
        &fx.harness.inner.worker_session_id,
    )
    .await
    .unwrap();
    crate::conversation_inputs::accept_human_tx(
        &mut tx,
        &fx.source.current_principal().unwrap(),
        &mut carrier,
        HumanPayload {
            text: "pending during maintenance".into(),
            bound_attachments: vec![],
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    fx.frame().await
}

#[tokio::test]
async fn canonical_codex_compact_displaced_then_source_loss_wedges() {
    let fx = Fixture::new().await;
    let (client, mut peer, forwarder) =
        crate::shared_codex_appserver::native_owner_tests::connect_with_reader(&fx.daemon).await;
    let thread = super::content_tests::start(&fx, &mut peer, fx.message.clone(), "user").await;
    super::content_tests::complete(&fx, &mut peer, &thread, "user").await;
    let before = fx.frame().await;
    let thread = before.header.last_thread_id.as_deref().unwrap();
    let task = compact(&fx);
    let call = request(&mut peer).await;
    send(&mut peer, json!({"id":call["id"],"result":{}})).await;
    task.await.unwrap().unwrap();
    notify_thread(
        &fx.daemon,
        &mut peer,
        thread,
        "turn/started",
        json!({"id":"maintenance"}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while fx
            .frame()
            .await
            .header
            .compact_outcome
            .unwrap()
            .turn_id
            .as_deref()
            != Some("maintenance")
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Stop canonical observation while the independent native reader displaces
    // the maintenance execution and invalidates its source.
    let issuance = fx.harness.inner.issuance.lock().await;
    let pending = accept_during_compact(&fx).await;
    notify_thread(
        &fx.daemon,
        &mut peer,
        thread,
        "turn/started",
        json!({"id":"foreign"}),
    )
    .await;
    peer.close(None).await.unwrap();
    crate::shared_codex_appserver::native_owner_tests::reader_finished(&client).await;
    tokio::time::timeout(Duration::from_secs(5), forwarder)
        .await
        .unwrap()
        .unwrap();
    let outcome = pending.header.compact_outcome.as_ref().unwrap();
    let target = crate::harness::backend::MaintenanceTarget {
        card_id: fx.harness.inner.card_id.to_string(),
        track_id: fx.harness.inner.track_id.to_string(),
        worker_session_id: fx.harness.inner.worker_session_id.clone(),
        thread_id: thread.into(),
        operation_id: outcome.operation_id.clone(),
    };
    assert!(matches!(
        fx.harness
            .inner
            .backend
            .observe_maintenance(&target)
            .await
            .unwrap(),
        Some(crate::harness::backend::MaintenanceObservation::Unconfirmed)
    ));
    drop(issuance);
    let held = fx.wait_phase(HarnessPhaseTag::Wedged).await;
    assert_eq!(
        held.header.compact_outcome.unwrap().status,
        CompactStatus::Unconfirmed
    );
    assert_eq!(held.header.last_turn_id, before.header.last_turn_id);
    assert_eq!(held.canonical_inputs, pending.canonical_inputs);
    fx.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_codex_compact_ack_without_start_expires() {
    assert_observation_expires(false).await;
}

#[tokio::test]
async fn canonical_codex_compact_start_without_end_expires() {
    assert_observation_expires(true).await;
}

async fn assert_observation_expires(started: bool) {
    let (fx, _client, mut peer) = ready().await;
    let before = fx.frame().await;
    let task = compact(&fx);
    let call = request(&mut peer).await;
    send(&mut peer, json!({"id":call["id"],"result":{}})).await;
    task.await.unwrap().unwrap();
    if started {
        notify_thread(
            &fx.daemon,
            &mut peer,
            before.header.last_thread_id.as_deref().unwrap(),
            "turn/started",
            json!({"id":"maintenance"}),
        )
        .await;
        wait_turn(&fx, "maintenance").await;
    }
    let issuance = fx.harness.inner.issuance.lock().await;
    let pending = accept_during_compact(&fx).await;
    let inner = &fx.harness.inner;
    let aged = if started {
        HarnessState::CompactionRunning {
            turn_id: "maintenance".into(),
            started_at: Instant::now() - inner.config.max_turn_duration,
        }
    } else {
        HarnessState::Compacting {
            since: Instant::now() - inner.config.interrupt_completion_budget,
        }
    };
    *inner.state.lock().await = aged;
    // Publishing an unrelated input refresh must not renew the same operation's budget.
    publish(inner, read_current(inner).await.unwrap()).await;
    drop(issuance);
    let held = fx.wait_phase(HarnessPhaseTag::Wedged).await;
    assert_eq!(
        held.header.compact_outcome.unwrap().status,
        CompactStatus::Unconfirmed
    );
    assert_eq!(held.header.last_turn_id, before.header.last_turn_id);
    assert_eq!(held.canonical_inputs, pending.canonical_inputs);
    assert!(held.canonical_inputs.active.is_none());
    assert_no_request(&mut peer).await;
    shutdown(&fx.harness, &mut peer).await;
}

/// A durable bind precedes owner publication; only the published running clock
/// can safely be aged by timeout tests without the initial publish overwriting it.
pub(super) async fn wait_turn(fx: &Fixture, turn: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let durable = fx.frame().await;
            let published = fx
                .harness
                .inner
                .inputs
                .canonical()
                .unwrap()
                .committed()
                .await;
            let bound = |frame: &CanonicalHarnessSnapshot| {
                frame
                    .header
                    .compact_outcome
                    .as_ref()
                    .and_then(|outcome| outcome.turn_id.as_deref())
                    == Some(turn)
            };
            let running = matches!(&*fx.harness.inner.state.lock().await,
                HarnessState::CompactionRunning {turn_id, ..} if turn_id == turn);
            if bound(&durable) && bound(&published.snapshot) && running {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn canonical_codex_compact_healthy_started_uses_completion_budget() {
    let (fx, _client, mut peer) = ready().await;
    let task = compact(&fx);
    let call = request(&mut peer).await;
    send(&mut peer, json!({"id":call["id"],"result":{}})).await;
    task.await.unwrap().unwrap();
    let thread = fx.frame().await.header.last_thread_id.unwrap();
    notify_thread(
        &fx.daemon,
        &mut peer,
        &thread,
        "turn/started",
        json!({"id":"healthy"}),
    )
    .await;
    wait_turn(&fx, "healthy").await;
    let inner = &fx.harness.inner;
    let issuance = inner.issuance.lock().await;
    let age = inner.config.interrupt_completion_budget + Duration::from_secs(1);
    assert!(age < inner.config.max_turn_duration);
    *inner.state.lock().await = HarnessState::CompactionRunning {
        turn_id: "healthy".into(),
        started_at: Instant::now() - age,
    };
    super::super::super::compact_command::canonical::observe(inner)
        .await
        .unwrap();
    assert_eq!(fx.frame().await.header.phase, HarnessPhaseTag::Compacting);
    drop(issuance);
    notify_thread(
        &fx.daemon,
        &mut peer,
        &thread,
        "turn/completed",
        json!({"id":"healthy","status":"completed"}),
    )
    .await;
    fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
    shutdown(&fx.harness, &mut peer).await;
}

#[tokio::test]
async fn canonical_codex_compact_second_operation_has_fresh_timer() {
    let (fx, _client, mut peer) = ready().await;
    let mut ids = Vec::new();
    for turn in ["first", "second"] {
        let task = compact(&fx);
        let call = request(&mut peer).await;
        send(&mut peer, json!({"id":call["id"],"result":{}})).await;
        task.await.unwrap().unwrap();
        let frame = fx.frame().await;
        ids.push(frame.header.compact_outcome.unwrap().operation_id);
        assert!(
            matches!(&*fx.harness.inner.state.lock().await, HarnessState::Compacting {since} if since.elapsed() < Duration::from_secs(5))
        );
        let thread = frame.header.last_thread_id.unwrap();
        notify_thread(
            &fx.daemon,
            &mut peer,
            &thread,
            "turn/started",
            json!({"id":turn}),
        )
        .await;
        wait_turn(&fx, turn).await;
        // Age the first operation; successful completion must not carry this timer forward.
        if turn == "first" {
            let issuance = fx.harness.inner.issuance.lock().await;
            *fx.harness.inner.state.lock().await = HarnessState::CompactionRunning {
                turn_id: turn.into(),
                started_at: Instant::now() - fx.harness.inner.config.interrupt_completion_budget,
            };
            drop(issuance);
        }
        notify_thread(
            &fx.daemon,
            &mut peer,
            &thread,
            "turn/completed",
            json!({"id":turn,"status":"completed"}),
        )
        .await;
        fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
    }
    assert_ne!(ids[0], ids[1]);
    shutdown(&fx.harness, &mut peer).await;
}
