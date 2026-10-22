//! Controls exercise the real loop and protocol owner; no fixture creates a claim or END.
use super::*;
use calm_truth::db::RepoOutOfDomain as _;
use calm_truth::db::RepoRead;

async fn running(fx: &Fixture, peer: &mut crate::shared_codex_appserver::native_owner_tests::Peer) {
    fx.original
        .repo
        .settings_upsert(crate::planner_turn_budget::KEY, "1")
        .await
        .unwrap();
    fx.harness
        .observe_canonical_source(fx.message.clone(), fx.source.clone())
        .await
        .unwrap();
    let call = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let call = request(peer).await;
            match call["method"].as_str().unwrap() {
                "turn/start" => break call,
                "config/read" => {
                    send(peer, json!({"id":call["id"],"result":{"config":{}}})).await;
                }
                method => panic!("unexpected native request before running: {method}"),
            }
        }
    })
    .await
    .unwrap();
    let thread = call["params"]["threadId"].as_str().unwrap();
    notify_thread(
        &fx.daemon,
        peer,
        thread,
        "turn/started",
        json!({"id":"bounded-original"}),
    )
    .await;
    send(
        peer,
        json!({"id":call["id"],"result":{"turn":{"id":"bounded-original"}}}),
    )
    .await;
    fx.wait_phase(HarnessPhaseTag::TurnRunning).await;
    // SQL commits before publish installs the scheduler's running clock and
    // frozen budget. Wait for that publication before any test rewinds it;
    // observing the durable phase alone can leave the rewind a silent no-op.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let published = fx
                .harness
                .inner
                .inputs
                .canonical()
                .unwrap()
                .committed()
                .await;
            if published.snapshot.header.phase == HarnessPhaseTag::TurnRunning {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the scheduler must publish its running clock and budget");
}

#[tokio::test]
async fn canonical_budget_warns_once_and_hard_stops_the_actual_original_execution() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    running(&fx, &mut peer).await;
    let original = fx.frame().await;
    let thread = original.header.last_thread_id.as_deref().unwrap();
    let turn = original.header.last_turn_id.as_deref().unwrap();
    // The existing timer seam moves only a running turn's monotonic clocks.
    fx.harness
        .rewind_turn_clock_for_test(Duration::from_secs(49))
        .await;
    let warning = tokio::time::timeout(Duration::from_secs(2), request(&mut peer))
        .await
        .expect("the actual canonical scheduler must issue its configured budget warning");
    assert_eq!(warning["method"], "turn/steer");
    assert_eq!(warning["params"]["threadId"], thread);
    assert_eq!(warning["params"]["expectedTurnId"], turn);
    assert!(
        warning["params"]["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| {
                item["text"].as_str().is_some_and(|text| {
                    text.starts_with("[Trusted Planner turn budget reminder]\n")
                        && text.contains("12 seconds of execution remain")
                })
            })
    );
    send(
        &mut peer,
        json!({"id":warning["id"],"result":{"turnId":turn}}),
    )
    .await;
    let warned = fx.frame().await;
    assert!(matches!(
        warned.header.turn_budget,
        budget::FrozenBudget::Planner {
            warning_attempted: true,
            ..
        }
    ));
    assert_eq!(warned.canonical_inputs, original.canonical_inputs);
    assert!(
        tokio::time::timeout(Duration::from_millis(150), request(&mut peer))
            .await
            .is_err(),
        "warning is attempted only once"
    );
    fx.harness
        .rewind_turn_clock_for_test(Duration::from_secs(20))
        .await;
    let stop = tokio::time::timeout(Duration::from_secs(2), request(&mut peer))
        .await
        .expect("the same scheduler must enforce the frozen hard stop");
    assert_eq!(stop["method"], "turn/interrupt");
    assert_eq!(stop["params"]["threadId"], thread);
    assert_eq!(stop["params"]["turnId"], turn);
    let stopping = fx.wait_phase(HarnessPhaseTag::IssuingInterrupt).await;
    assert_eq!(stopping.canonical_inputs, original.canonical_inputs);
    assert!(stopping.header.interruption_intent.is_some());
    send(&mut peer, json!({"id":stop["id"],"result":{}})).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        fx.frame().await.canonical_inputs,
        original.canonical_inputs,
        "control ACK is not END"
    );
    notify_thread(
        &fx.daemon,
        &mut peer,
        thread,
        "turn/completed",
        json!({"id":turn,"status":"interrupted"}),
    )
    .await;
    let done = fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
    assert!(done.canonical_inputs.active.is_none());
    assert_eq!(
        done.canonical_inputs.attempts,
        original.canonical_inputs.attempts
    );
    assert!(done.header.interruption_intent.is_none());
    let outcome: String = sqlx::query_scalar("SELECT params FROM harness_items WHERE card_id=?1 AND turn_id=?2 AND method='turn/completed'")
        .bind(fx.harness.inner.card_id.as_str()).bind(turn).fetch_one(fx.original.repo.pool()).await.unwrap();
    let outcome: Value = serde_json::from_str(&outcome).unwrap();
    assert_eq!(outcome["harness_interruption_reason"]["version"], "v1");
    assert_eq!(
        outcome["harness_interruption_reason"]["request"]["budget_ms"],
        60_000
    );
    peer.close(None).await.unwrap();
    fx.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn accepted_stop_drains_its_original_target_after_native_gate_failure_and_logout() {
    let fx = Fixture::with_config(HarnessConfig {
        interrupt_completion_budget: Duration::from_millis(100),
        ..HarnessConfig::default()
    })
    .await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    running(&fx, &mut peer).await;
    let original = fx.frame().await;
    let gate =
        crate::shared_codex_appserver::native_owner_tests::hold_connected_client_gate(&fx.daemon)
            .await;
    let stop = tokio::spawn({
        let harness = fx.harness.clone();
        let source = fx.source.clone();
        async move { harness.stop_canonical(source).await }
    });
    fx.wait_phase(HarnessPhaseTag::IssuingInterrupt).await;
    let held = fx.wait_phase(HarnessPhaseTag::Wedged).await;
    assert!(held.header.interruption_intent.is_some());
    assert_eq!(held.canonical_inputs, original.canonical_inputs);
    fx.auth.sessions.remove(&fx.session).unwrap();
    assert!(fx.source.current_principal().is_err());
    drop(gate);
    let call = tokio::time::timeout(Duration::from_secs(2), request(&mut peer)).await;
    if call.is_err() {
        assert!(stop.await.unwrap().is_err());
        peer.close(None).await.unwrap();
        fx.harness.shutdown().await.unwrap();
        panic!("the already committed Stop must still drain its original target after failure");
    }
    let call = call.unwrap();
    assert_eq!(call["method"], "turn/interrupt");
    assert_eq!(
        call["params"]["threadId"],
        original.header.last_thread_id.as_deref().unwrap()
    );
    assert_eq!(
        call["params"]["turnId"],
        original.header.last_turn_id.as_deref().unwrap()
    );
    send(&mut peer, json!({"id":call["id"],"result":{}})).await;
    stop.await.unwrap().unwrap();
    assert_eq!(fx.frame().await.header.phase, HarnessPhaseTag::Wedged);
    notify_thread(
        &fx.daemon,
        &mut peer,
        original.header.last_thread_id.as_deref().unwrap(),
        "turn/completed",
        json!({"id":original.header.last_turn_id,"status":"interrupted"}),
    )
    .await;
    let ended = fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
    assert!(ended.canonical_inputs.active.is_none());
    assert_eq!(
        ended.canonical_inputs.attempts,
        original.canonical_inputs.attempts
    );
    let row = fx
        .original
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(
            fx.harness.inner.worker_session_id.clone(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, calm_types::worker::WorkerSessionState::Failed);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), request(&mut peer))
            .await
            .is_err()
    );
    peer.close(None).await.unwrap();
    fx.harness.shutdown().await.unwrap();
}

async fn pending(fx: &Fixture) -> MessageRevisionRef {
    let mut tx = begin_immediate_tx(fx.original.repo.db()).await.unwrap();
    let mut carrier = crate::db::sqlite::conversation::current_input_content_carrier_tx(
        &mut tx,
        fx.harness.inner.card_id.as_str(),
    )
    .await
    .unwrap();
    let accepted = crate::conversation_inputs::accept_human_tx(
        &mut tx,
        &fx.source.current_principal().unwrap(),
        &mut carrier,
        HumanPayload {
            text: "pending after control".into(),
            bound_attachments: vec![],
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let AcceptedInputRef::Human { message } = accepted.input_ref else {
        panic!("human")
    };
    message
}

async fn failed_stays_failed_after_end(
    fx: &Fixture,
    peer: &mut crate::shared_codex_appserver::native_owner_tests::Peer,
    original: &CanonicalHarnessSnapshot,
) {
    let message = pending(fx).await;
    let held = fx.frame().await;
    assert_eq!(
        held.canonical_inputs.active,
        original.canonical_inputs.active
    );
    assert_eq!(
        held.canonical_inputs.attempts,
        original.canonical_inputs.attempts
    );
    let thread = original.header.last_thread_id.as_deref().unwrap();
    let turn = original.header.last_turn_id.as_deref().unwrap();
    send(peer,json!({"method":"item/completed","params":{
        "threadId":thread,"turnId":turn,
        "item":{"id":"late-failed-reply","type":"agentMessage","text":"Original result after failure"}
    }})).await;
    let late_reply = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let stored: Option<String> = sqlx::query_scalar(
                "SELECT i.params FROM conversation_presentations p
                 JOIN harness_items i ON i.id=p.native_item_row_id
                 WHERE p.card_id=?1 AND i.item_uuid='late-failed-reply'",
            )
            .bind(fx.harness.inner.card_id.as_str())
            .fetch_optional(fx.original.repo.pool())
            .await
            .unwrap();
            if let Some(stored) = stored {
                break stored;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    notify_thread(
        &fx.daemon,
        peer,
        "unrelated-old-thread",
        "turn/completed",
        json!({"id":turn,"status":"interrupted"}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(
        fx.frame().await.canonical_inputs.active.is_some(),
        "old thread cannot close the current execution"
    );
    notify_thread(
        &fx.daemon,
        peer,
        thread,
        "turn/completed",
        json!({"id":turn,"status":"interrupted"}),
    )
    .await;
    let done = fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
    assert!(done.canonical_inputs.active.is_none());
    assert_eq!(
        done.canonical_inputs.attempts,
        original.canonical_inputs.attempts
    );
    assert_eq!(
        done.canonical_inputs.pending[0]
            .accepted_input_ref()
            .unwrap(),
        AcceptedInputRef::Human { message }
    );
    let row = fx
        .original
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(
            fx.harness.inner.worker_session_id.clone(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.state,
        calm_types::worker::WorkerSessionState::Failed,
        "END is a fact, not restored execution authority"
    );
    let mut tx = begin_immediate_tx(fx.original.repo.db()).await.unwrap();
    let mut carrier = crate::db::sqlite::conversation::current_input_fact_carrier_tx(
        &mut tx,
        fx.harness.inner.card_id.as_str(),
        &fx.harness.inner.worker_session_id,
    )
    .await
    .unwrap();
    let plan = human_plan_for_pending_prefix_tx(
        &mut tx,
        &carrier,
        std::num::NonZeroU32::new(1).unwrap(),
        vec![],
    )
    .await
    .unwrap();
    assert!(
        crate::conversation_inputs::claim_initial_tx(&mut tx, &fx.source, &mut carrier, plan)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), request(peer))
            .await
            .is_err(),
        "failed carrier must not redispatch its preserved pending input"
    );
    peer.close(None).await.unwrap();
    fx.harness.shutdown().await.unwrap();
    let stored = late_reply
        .expect("a Failed carrier must preserve the original native reply and its presentation");
    assert_eq!(
        serde_json::from_str::<Value>(&stored).unwrap()["item"]["text"],
        "Original result after failure"
    );
}

#[tokio::test]
async fn canonical_stop_deadline_is_durable_and_actual_late_end_keeps_runtime_failed() {
    let fx = Fixture::with_config(HarnessConfig {
        interrupt_completion_budget: Duration::from_millis(100),
        ..HarnessConfig::default()
    })
    .await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    running(&fx, &mut peer).await;
    let original = fx.frame().await;
    let stop = tokio::spawn({
        let harness = fx.harness.clone();
        let source = fx.source.clone();
        async move { harness.stop_canonical(source).await }
    });
    let call = request(&mut peer).await;
    assert_eq!(call["method"], "turn/interrupt");
    let wedged = fx.wait_phase(HarnessPhaseTag::Wedged).await;
    assert_eq!(
        wedged.header.wedged_reason.as_deref(),
        Some(calm_types::harness::HARNESS_INTERRUPT_TIMEOUT_REASON)
    );
    assert!(
        wedged
            .header
            .interruption_intent
            .as_ref()
            .unwrap()
            .timeout_reason()
            .is_none(),
        "manual stop is not a budget timeout cause"
    );
    send(&mut peer, json!({"id":call["id"],"result":{}})).await;
    stop.await.unwrap().unwrap();
    failed_stays_failed_after_end(&fx, &mut peer, &original).await;
}

#[tokio::test]
async fn canonical_native_system_error_holds_original_and_real_end_does_not_restore_authority() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    running(&fx, &mut peer).await;
    let original = fx.frame().await;
    send(
        &mut peer,
        json!({"method":"thread/status/changed","params":{
            "threadId":original.header.last_thread_id,"status":{"type":"systemError"}
        }}),
    )
    .await;
    let held = fx.wait_phase(HarnessPhaseTag::Wedged).await;
    assert_eq!(
        held.header.wedged_reason.as_deref(),
        Some(HARNESS_SYSTEM_ERROR_REASON)
    );
    failed_stays_failed_after_end(&fx, &mut peer, &original).await;
}

#[tokio::test]
async fn canonical_metadata_refuses_a_corrupt_control_checkpoint_without_a_native_target() {
    let fx = Fixture::new().await;
    let before = fx.frame().await;
    // Deliberately corrupt persisted format data, not a simulated valid turn.
    // The real metadata boundary must refuse it rather than panic or make V1.
    let mut value = serde_json::to_value(&before).unwrap();
    value["phase"] = json!("issuing_interrupt");
    let mut tx = begin_immediate_tx(fx.original.repo.db()).await.unwrap();
    session_set_handle_state_tx(&mut tx, &fx.harness.inner.worker_session_id, Some(value))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(fx.harness.runtime_header().await.is_err());
    let current = fx.frame().await;
    assert_eq!(current.canonical_inputs, before.canonical_inputs);
    assert!(current.header.last_turn_id.is_none());
    fx.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_qualified_end_records_closed_facts_without_private_native_extras() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    running(&fx, &mut peer).await;
    let running = fx.frame().await;
    let thread = running.header.last_thread_id.as_deref().unwrap();
    let turn = running.header.last_turn_id.as_deref().unwrap();
    notify_thread(
        &fx.daemon,
        &mut peer,
        thread,
        "turn/completed",
        json!({
            "id":turn,"status":"failed",
            "error":{"message":"canonical scheduler human body"},
            "privateEcho":"canonical scheduler human body",
            "items":[{"text":"canonical scheduler human body"}]
        }),
    )
    .await;
    fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
    let outcome: String = sqlx::query_scalar("SELECT params FROM harness_items WHERE card_id=?1 AND turn_id=?2 AND method='turn/completed'")
        .bind(fx.harness.inner.card_id.as_str()).bind(turn).fetch_one(fx.original.repo.pool()).await.unwrap();
    assert!(
        !outcome.contains("canonical scheduler human body"),
        "a native diagnostic must not create a second private body: {outcome}"
    );
    let value: Value = serde_json::from_str(&outcome).unwrap();
    assert_eq!(value["id"], turn);
    assert_eq!(value["status"], "failed");
    assert_eq!(
        value["error"]["message"],
        "The provider ended this execution with an error."
    );
    peer.close(None).await.unwrap();
    fx.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn compact_planner_request_refuses_canonical_conversation() {
    let f = Fixture::new().await;
    let result = f
        .harness
        .request_compaction("request".into())
        .await
        .unwrap();
    assert_eq!(
        result,
        crate::harness::compact_outcome::CompactionAnswer::Refused(
            crate::harness::compact_outcome::CompactionRefusal::UnsupportedConversation
        )
    );
    crate::harness::run_loop::compact_task_tests::declared(&f.original).await;
    let id = format!("{}:compact", f.original.harness.inner.track_id);
    let task = f.original.repo.task_get(&id).await.unwrap().unwrap();
    assert_eq!(task.status, crate::model::TaskStatus::Failed);
    assert_eq!(
        task.status_detail.as_deref(),
        Some("compaction-refused: unsupported_conversation")
    );
}
