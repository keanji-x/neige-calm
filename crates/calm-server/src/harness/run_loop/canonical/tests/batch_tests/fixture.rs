//! Production ingress and deterministic native-response boundaries for batch tests.
use super::*;
use crate::shared_codex_appserver::native_owner_tests::Peer;
use calm_types::conversation::SubmissionRenderPlan;

pub(super) async fn accept_payload(fx: &Fixture, payload: HumanPayload) -> MessageRevisionRef {
    fx.original
        .repo
        .db()
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                let mut tx = write_session.begin_immediate().await.unwrap();
                let mut carrier = current_input_carrier_tx(
                    &mut tx,
                    fx.harness.inner.card_id.as_str(),
                    &fx.harness.inner.worker_session_id,
                )
                .await
                .unwrap();
                let accepted = crate::conversation_inputs::accept_human_tx(
                    &mut tx,
                    &fx.source.current_principal().unwrap(),
                    &mut carrier,
                    payload,
                )
                .await
                .unwrap();
                tx.commit().await.unwrap();
                let AcceptedInputRef::Human { message } = accepted.input_ref else {
                    panic!("human")
                };
                message
            })
        })
        .await
        .expect("independent write operation")
}

pub(super) async fn stored_plan(
    fx: &Fixture,
    frame: &CanonicalHarnessSnapshot,
) -> SubmissionRenderPlan {
    fx.original
        .repo
        .db()
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                let mut tx = write_session.begin_immediate().await.unwrap();
                let submission = &frame
                    .canonical_inputs
                    .active
                    .as_ref()
                    .unwrap()
                    .submission_ids[0];
                let stored =
                    crate::db::sqlite::conversation::submission_plan_tx(&mut tx, submission)
                        .await
                        .unwrap();
                tx.rollback().await.unwrap();
                stored.plan
            })
        })
        .await
        .expect("independent write operation")
}

pub(super) async fn acknowledge(
    fx: &Fixture,
    peer: &mut Peer,
    call: &Value,
    turn: &str,
) -> CanonicalHarnessSnapshot {
    notify_thread(
        &fx.daemon,
        peer,
        call["params"]["threadId"].as_str().unwrap(),
        "turn/started",
        json!({"id":turn,"status":"inProgress"}),
    )
    .await;
    send(peer, json!({"id":call["id"],"result":{"turn":{"id":turn}}})).await;
    fx.wait_phase(HarnessPhaseTag::TurnRunning).await
}

pub(super) async fn observe(fx: &Fixture) {
    fx.harness
        .observe_canonical_source(fx.message.clone(), fx.source.clone())
        .await
        .unwrap();
}

pub(super) async fn erase(fx: &Fixture, message: &MessageRevisionRef) {
    let payload = calm_types::conversation::PrivateHistoryPayload::<HumanPayload>::ErasedByPrivateHistoryOperation {
        operation_id: calm_types::conversation::PrivateHistoryOperationId::parse(&crate::model::new_id()).unwrap(),
    };
    // Change the owning storage fact, as in the existing native source-gate
    // tests; admission and rendering still run through their production owners.
    let updated = fx.original.repo.db().writer().execute(sqlx::query("UPDATE conversation_message_revisions SET payload_json=?1 WHERE message_id=?2 AND revision=?3")
        .bind(serde_json::to_string(&payload).unwrap())
        .bind(message.message_id.as_str())
        .bind(i64::from(message.revision.get()))
        )
        .await
        .unwrap();
    assert_eq!(updated.rows_affected(), 1);
}

pub(super) async fn assert_held_after_reply(
    fx: &Fixture,
    peer: &mut Peer,
    reply: Value,
    expected: CanonicalHarnessSnapshot,
) {
    // A completed scheduler pass proves there was no later RPC/claim on this
    // selection. Keep it paused so a fresh selection cannot hide that result.
    let tick = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "canonical-tick-returned",
        fx.harness.inner.card_id.as_str(),
        tick.clone(),
    );
    send(peer, reply).await;
    let mut unexpected = vec![];
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                _ = tick.entered.notified() => break,
                call = request(peer) => {
                    unexpected.push(call["method"].clone());
                    if call["method"] == "model/list" {
                        send(peer, selection_authority_tests::catalog_reply(&call, None)).await;
                    } else {
                        send(peer, json!({"id":call["id"],"error":{"code":"malformed","message":"unexpected batch effect"}})).await;
                    }
                }
            }
        }
    })
    .await
    .unwrap();
    let after = fx.frame().await;
    fx.auth.sessions.remove(&fx.session).unwrap();
    tick.release.notify_one();
    fx.harness.shutdown().await.unwrap();
    assert!(
        unexpected.is_empty(),
        "unexpected native effects: {unexpected:?}"
    );
    assert_eq!(after, expected);
    assert!(after.canonical_inputs.active.is_none());
    assert!(after.canonical_inputs.attempts.is_empty());
    let submissions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM conversation_input_submissions WHERE card_id=?1")
            .bind(fx.harness.inner.card_id.as_str())
            .fetch_one(fx.original.repo.pool())
            .await
            .unwrap();
    assert_eq!(submissions, 0);
}

pub(super) async fn restart(fx: &Fixture) -> PlannerHarness {
    let inner = &fx.harness.inner;
    PlannerHarness::run(PlannerHarnessParams {
        track_delete_locks: inner.track_delete_locks.clone(),
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
    .unwrap()
}
