//! Native content travels through the real notification stream and running loop.
use super::*;
use crate::codex_appserver::Notification;
use crate::shared_codex_appserver::native_owner_tests::Peer;
use calm_truth::db::RepoEventWrite as _;

async fn next_start(peer: &mut Peer) -> Value {
    loop {
        let call = request(peer).await;
        if call["method"] == "turn/start" {
            return call;
        }
        assert_eq!(call["method"], "config/read");
        send(peer, json!({"id":call["id"],"result":{"config":{}}})).await;
    }
}

pub(super) async fn notify(fx: &Fixture, peer: &mut Peer, method: &str, params: Value) {
    let mut notifications = fx.daemon.subscribe_notifications();
    send(peer, json!({"method":method,"params":params})).await;
    let barrier = format!("fixture/content-barrier/{}", uuid::Uuid::new_v4());
    send(peer, json!({"method":barrier,"params":{}})).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(notifications.recv().await.unwrap(), Notification::Other {method,..} if method==barrier)
            {
                break;
            }
        }
    })
    .await
    .unwrap();
}

async fn item(fx: &Fixture, peer: &mut Peer, thread: &str, turn: &str, id: &str, text: &str) {
    notify(
        fx,
        peer,
        "item/completed",
        json!({"threadId":thread,"turnId":turn,
            "item":{"id":id,"type":"agentMessage","text":text}}),
    )
    .await;
}

pub(super) async fn wait_item(fx: &Fixture, id: &str) -> i64 {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let row: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM harness_items WHERE card_id=?1 AND item_uuid=?2 AND method='item/completed' ORDER BY id DESC LIMIT 1",
            )
            .bind(fx.harness.inner.card_id.as_str())
            .bind(id)
            .fetch_optional(fx.original.repo.pool())
            .await
            .unwrap();
            if let Some(row) = row {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the actual canonical loop must persist the native reply")
}

pub(super) async fn start(
    fx: &Fixture,
    peer: &mut Peer,
    message: MessageRevisionRef,
    turn: &str,
) -> String {
    fx.harness
        .observe_canonical_source(message, fx.source.clone())
        .await
        .unwrap();
    let call = next_start(peer).await;
    let thread = call["params"]["threadId"].as_str().unwrap().to_owned();
    notify_thread(
        &fx.daemon,
        peer,
        &thread,
        "turn/started",
        json!({"id":turn,"status":"inProgress"}),
    )
    .await;
    send(peer, json!({"id":call["id"],"result":{"turn":{"id":turn}}})).await;
    let running = fx
        .wait_phase_at(HarnessPhaseTag::TurnRunning, PhaseWaitStage::UserStart)
        .await;
    assert_eq!(running.header.last_turn_id.as_deref(), Some(turn));
    thread
}

pub(super) async fn complete(fx: &Fixture, peer: &mut Peer, thread: &str, turn: &str) {
    notify_thread(
        &fx.daemon,
        peer,
        thread,
        "turn/completed",
        json!({"id":turn,"status":"completed"}),
    )
    .await;
    fx.wait_phase_at(HarnessPhaseTag::TurnCompleted, PhaseWaitStage::UserComplete)
        .await;
}

async fn accept(fx: &Fixture, body: &str) -> MessageRevisionRef {
    let mut tx = begin_immediate_tx(fx.original.repo.db()).await.unwrap();
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
        HumanPayload {
            text: body.into(),
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

async fn stop(fx: &Fixture, peer: &mut Peer) {
    let mut stopping = tokio::spawn({
        let handle = fx.harness.clone();
        async move { handle.shutdown().await }
    });
    tokio::select! {
        stopped=&mut stopping => stopped.unwrap().unwrap(),
        call=request(peer) => {
            assert_eq!(call["method"],"turn/interrupt");
            send(peer,json!({"id":call["id"],"result":{}})).await;
            stopping.await.unwrap().unwrap();
        }
    }
}

#[tokio::test]
async fn objective_canonical_reply_and_end_before_start_ack_persist_without_accepting_echo() {
    let fx = Fixture::new().await;
    let mut tx = begin_immediate_tx(fx.original.repo.db()).await.unwrap();
    crate::db::sqlite::track_capabilities::stamp_tx(
        &mut tx,
        &fx.harness.inner.track_id,
        &[calm_types::track_capability::TrackCapability::Objectives],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (_client, mut peer) = connect(&fx.daemon).await;
    fx.harness
        .observe_canonical_source(fx.message.clone(), fx.source.clone())
        .await
        .unwrap();
    let call = next_start(&mut peer).await;
    let thread = call["params"]["threadId"].as_str().unwrap();
    notify_thread(
        &fx.daemon,
        &mut peer,
        thread,
        "turn/started",
        json!({"id":"early","status":"inProgress"}),
    )
    .await;
    notify(&fx, &mut peer, "item/completed", json!({"threadId":thread,"turnId":"early",
        "item":{"id":"human-echo","type":"userMessage","content":[{"type":"inputText","text":"canonical scheduler human body"}]}})).await;
    item(
        &fx,
        &mut peer,
        thread,
        "early",
        "early-reply",
        "real early answer",
    )
    .await;
    notify_thread(
        &fx.daemon,
        &mut peer,
        thread,
        "turn/completed",
        json!({"id":"early","status":"completed"}),
    )
    .await;
    let before_ack = fx.frame().await;
    assert_eq!(before_ack.header.phase, HarnessPhaseTag::IssuingTurn);
    assert!(matches!(
        before_ack.canonical_inputs.attempts[0].delivery,
        InputDeliveryFact::CrossingUnknown
    ));
    send(
        &mut peer,
        json!({"id":call["id"],"result":{"turn":{"id":"early"}}}),
    )
    .await;
    let done = fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (_, state) = calm_truth::objective_host::read_state(
                fx.original.repo.pool(),
                &fx.harness.inner.track_id,
            )
            .await
            .unwrap();
            assert_eq!(state.usage.context_tokens, 0, "no initial endpoint");
            if state.usage.turns == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("committed early END must count with no visible initial endpoint");
    let row = wait_item(&fx, "early-reply").await;
    let presentation: i64 = sqlx::query_scalar("SELECT native_item_row_id FROM conversation_presentations WHERE card_id=?1 AND source_kind='native_item'")
        .bind(fx.harness.inner.card_id.as_str()).fetch_one(fx.original.repo.pool()).await.unwrap();
    assert_eq!(presentation, row);
    let echoes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM harness_items WHERE card_id=?1 AND item_uuid='human-echo'",
    )
    .bind(fx.harness.inner.card_id.as_str())
    .fetch_one(fx.original.repo.pool())
    .await
    .unwrap();
    assert_eq!(echoes, 0, "native Human echo must never recreate a body");
    let humans: i64 =
        sqlx::query_scalar("SELECT count(*) FROM conversation_messages WHERE card_id=?1")
            .bind(fx.harness.inner.card_id.as_str())
            .fetch_one(fx.original.repo.pool())
            .await
            .unwrap();
    assert_eq!(humans, 1);
    assert!(done.canonical_inputs.pending.is_empty());
    assert!(done.canonical_inputs.active.is_none());
    assert!(matches!(
        done.canonical_inputs.attempts[0].delivery,
        InputDeliveryFact::Accepted { .. }
    ));
    stop(&fx, &mut peer).await;
}

#[tokio::test]
async fn canonical_late_reply_keeps_current_turn_and_new_pending_input() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    let thread = start(&fx, &mut peer, fx.message.clone(), "first").await;
    complete(&fx, &mut peer, &thread, "first").await;
    let second = accept(&fx, "second input").await;
    start(&fx, &mut peer, second, "second").await;
    let third = accept(&fx, "new pending input without live source").await;
    let before = fx.frame().await;
    item(
        &fx,
        &mut peer,
        &thread,
        "first",
        "late-first",
        "late original answer",
    )
    .await;
    let row = wait_item(&fx, "late-first").await;
    let linked: i64 = sqlx::query_scalar("SELECT count(*) FROM conversation_presentations WHERE card_id=?1 AND native_item_row_id=?2")
        .bind(fx.harness.inner.card_id.as_str()).bind(row).fetch_one(fx.original.repo.pool()).await.unwrap();
    assert_eq!(linked, 1);
    let after = fx.frame().await;
    assert_eq!(
        after.header, before.header,
        "historical content is not a current phase writer"
    );
    assert_eq!(after.canonical_inputs, before.canonical_inputs);
    assert!(
        matches!(&after.canonical_inputs.pending[0], PendingInput::Human { message, .. } if message == &third)
    );
    complete(&fx, &mut peer, &thread, "second").await;
    stop(&fx, &mut peer).await;
}

#[tokio::test]
async fn canonical_erased_native_reply_cannot_leave_a_raw_body_on_late_repeat() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    let thread = start(&fx, &mut peer, fx.message.clone(), "original").await;
    item(
        &fx,
        &mut peer,
        &thread,
        "original",
        "erased-reply",
        "private reply",
    )
    .await;
    let row = wait_item(&fx, "erased-reply").await;
    complete(&fx, &mut peer, &thread, "original").await;
    // Declare the existing presentation erasure owner's identity-only tombstone,
    // as its storage tests do. No synthetic END, delivery or runtime transition.
    let mut tx = begin_immediate_tx(fx.original.repo.db()).await.unwrap();
    let changed = sqlx::query(
        "UPDATE conversation_presentations
         SET erased_by_operation_id=?1,native_item_row_id=NULL
         WHERE card_id=?2 AND native_item_row_id=?3",
    )
    .bind("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    .bind(fx.harness.inner.card_id.as_str())
    .bind(row)
    .execute(&mut **tx)
    .await
    .unwrap();
    assert_eq!(changed.rows_affected(), 1);
    sqlx::query("DELETE FROM harness_items WHERE id=?1")
        .bind(row)
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let before = fx.frame().await;
    item(
        &fx,
        &mut peer,
        &thread,
        "original",
        "erased-reply",
        "private reply",
    )
    .await;
    // A following ordinary item proves this same loop consumed the repeat.
    item(
        &fx,
        &mut peer,
        &thread,
        "original",
        "following-reply",
        "visible tail",
    )
    .await;
    wait_item(&fx, "following-reply").await;
    let bodies: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM harness_items WHERE card_id=?1 AND item_uuid='erased-reply'",
    )
    .bind(fx.harness.inner.card_id.as_str())
    .fetch_one(fx.original.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        bodies, 0,
        "erasure refusal must happen before committing the raw body"
    );
    let erased: Option<i64> = sqlx::query_scalar("SELECT native_item_row_id FROM conversation_presentations WHERE card_id=?1 AND erased_by_operation_id IS NOT NULL")
        .bind(fx.harness.inner.card_id.as_str()).fetch_one(fx.original.repo.pool()).await.unwrap();
    assert_eq!(erased, None);
    assert_eq!(fx.frame().await, before);
    stop(&fx, &mut peer).await;
}

#[tokio::test]
async fn canonical_native_reply_does_not_upgrade_unknown_delivery_or_release_input() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    fx.harness
        .observe_canonical_source(fx.message.clone(), fx.source.clone())
        .await
        .unwrap();
    let call = next_start(&mut peer).await;
    let thread = call["params"]["threadId"].as_str().unwrap();
    notify_thread(
        &fx.daemon,
        &mut peer,
        thread,
        "turn/started",
        json!({"id":"unacknowledged","status":"inProgress"}),
    )
    .await;
    item(
        &fx,
        &mut peer,
        thread,
        "unacknowledged",
        "uncertain-reply",
        "observed content is not an ACK",
    )
    .await;
    send(
        &mut peer,
        json!({"id":call["id"],"error":{"code":-32000,"message":"no matched acceptance"}}),
    )
    .await;
    wait_item(&fx, "uncertain-reply").await;
    let observed = fx.frame().await;
    assert_eq!(observed.header.phase, HarnessPhaseTag::IssuingTurn);
    assert!(observed.canonical_inputs.active.is_some());
    assert!(observed.canonical_inputs.pending.is_empty());
    assert!(matches!(
        observed.canonical_inputs.attempts[0].delivery,
        InputDeliveryFact::Unknown
    ));
    notify_thread(
        &fx.daemon,
        &mut peer,
        thread,
        "turn/completed",
        json!({"id":"unacknowledged","status":"completed"}),
    )
    .await;
    item(
        &fx,
        &mut peer,
        thread,
        "unacknowledged",
        "uncertain-tail",
        "still only content",
    )
    .await;
    wait_item(&fx, "uncertain-tail").await;
    assert_eq!(
        fx.frame().await,
        observed,
        "raw END without original matched association cannot complete the claim"
    );
    stop(&fx, &mut peer).await;
}

#[tokio::test]
async fn canonical_native_questions_and_usage_preserve_current_input_checkpoint() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    let thread = start(&fx, &mut peer, fx.message.clone(), "observed").await;
    accept(&fx, "pending during usage").await;
    let before = fx.frame().await;
    notify(
        &fx,
        &mut peer,
        "item/completed",
        json!({"threadId":thread,"turnId":"observed",
        "item":{"id":"question","type":"agentMessage","text":"Which option?",
        "questions":[{"title":"Choose an option","options":["first","second"]}]}}),
    )
    .await;
    wait_item(&fx, "question").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let asks = fx
                .original
                .repo
                .events_for_track(fx.harness.inner.track_id.as_str(), &["ask.requested"], None)
                .await
                .unwrap();
            if !asks.is_empty() {
                assert_eq!(asks.len(), 1);
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("native questions must use the existing ask owner");
    notify(&fx, &mut peer, "thread/tokenUsage/updated", json!({"threadId":thread,"turnId":"observed",
        "tokenUsage":{"last":{"totalTokens":100},"total":{"totalTokens":1234},"modelContextWindow":200000}})).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if fx
                .frame()
                .await
                .header
                .token_usage
                .as_ref()
                .is_some_and(|usage| usage.used_tokens == 100)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("current usage must be persisted through the actual canonical frame owner");
    let with_usage = fx.frame().await;
    assert_eq!(with_usage.canonical_inputs, before.canonical_inputs);
    assert_eq!(with_usage.header.phase, before.header.phase);
    assert_eq!(with_usage.header.last_turn_id, before.header.last_turn_id);
    assert_eq!(
        with_usage.header.token_usage.as_ref().unwrap().total_tokens,
        1234
    );
    notify(
        &fx,
        &mut peer,
        "thread/tokenUsage/updated",
        json!({"threadId":thread,"turnId":"stale",
        "tokenUsage":{"last":{"totalTokens":1},"total":{"totalTokens":1}}}),
    )
    .await;
    item(
        &fx,
        &mut peer,
        &thread,
        "observed",
        "after-usage",
        "usage barrier",
    )
    .await;
    wait_item(&fx, "after-usage").await;
    assert_eq!(
        fx.frame().await,
        with_usage,
        "stale usage cannot overwrite the new turn's frame"
    );
    notify(
        &fx,
        &mut peer,
        "item/completed",
        json!({"threadId":thread,"turnId":"observed",
        "item":{"id":"automatic-boundary","type":"contextCompaction"}}),
    )
    .await;
    wait_item(&fx, "automatic-boundary").await;
    let invalidated = fx.frame().await;
    assert!(invalidated.header.context_usage_invalidated);
    assert_eq!(
        invalidated.header.context_usage_invalidated_turn.as_deref(),
        Some("observed")
    );
    notify(
        &fx,
        &mut peer,
        "thread/tokenUsage/updated",
        json!({"threadId":thread,"turnId":"observed",
        "tokenUsage":{"last":{"totalTokens":75},"total":{"totalTokens":5678}}}),
    )
    .await;
    item(
        &fx,
        &mut peer,
        &thread,
        "observed",
        "after-boundary-usage",
        "billing barrier",
    )
    .await;
    wait_item(&fx, "after-boundary-usage").await;
    let billing = fx.frame().await;
    assert!(
        billing.header.context_usage_invalidated,
        "billing after a boundary is not retained occupancy"
    );
    assert_eq!(
        billing.header.token_usage.as_ref().unwrap().total_tokens,
        5678
    );
    assert_eq!(
        billing.header.token_usage.as_ref().unwrap().context_window,
        Some(200000)
    );
    assert_eq!(billing.canonical_inputs, before.canonical_inputs);
    complete(&fx, &mut peer, &thread, "observed").await;
    stop(&fx, &mut peer).await;
}

#[tokio::test]
async fn canonical_installed_live_reply_streams_without_a_native_phase_writer() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    let thread = start(&fx, &mut peer, fx.message.clone(), "streaming").await;
    let before = fx.frame().await;
    notify(
        &fx,
        &mut peer,
        "item/started",
        json!({"threadId":thread,"turnId":"streaming",
        "item":{"id":"streamed","type":"agentMessage","text":""}}),
    )
    .await;
    notify(
        &fx,
        &mut peer,
        "item/agentMessage/delta",
        json!({"threadId":thread,"turnId":"streaming",
        "itemId":"streamed","delta":"live answer"}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let live = fx.registry.live_replies().read(&fx.harness.inner.card_id);
            if live
                .items
                .iter()
                .any(|item| item.item_id == "streamed" && item.text == "live answer")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the installed existing live-reply owner must receive canonical native content");
    assert_eq!(fx.frame().await, before);
    // Subscribe after the streamed delta, so the start's presentation event is past.
    let mut presentations = fx.harness.inner.events.subscribe();
    notify_thread(
        &fx.daemon,
        &mut peer,
        &thread,
        "turn/completed",
        json!({"id":"streaming","status":"failed","error":{"message":"peer stopped"}}),
    )
    .await;
    fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
    let row = wait_item(&fx, "streamed").await;
    let params: String = sqlx::query_scalar("SELECT params FROM harness_items WHERE id=?1")
        .bind(row)
        .fetch_one(fx.original.repo.pool())
        .await
        .unwrap();
    let params: Value = serde_json::from_str(&params).unwrap();
    assert_eq!(params["_partial"], true);
    assert_eq!(params["item"]["text"], "live answer");
    let sequence: i64 = sqlx::query_scalar(
        "SELECT sequence FROM conversation_presentations WHERE card_id=?1 AND native_item_row_id=?2",
    )
    .bind(fx.harness.inner.card_id.as_str())
    .bind(row)
    .fetch_one(fx.original.repo.pool())
    .await
    .unwrap();
    let partial_cursor = calm_types::conversation_display::ConversationCursor::new(
        fx.harness.inner.card_id.as_str(),
        calm_types::conversation_display::ConversationCursorSource::Presentation,
        sequence,
    );
    // The partial row's commit can precede live cleanup; its event follows item_stored.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(presentations.recv().await.unwrap().event,
                Event::HarnessPresentationChanged { cursor, .. } if cursor == partial_cursor)
            {
                break;
            }
        }
    })
    .await
    .expect("the partial reply must be announced after leaving the live view");
    let live = fx.registry.live_replies().read(&fx.harness.inner.card_id);
    assert!(live.items.is_empty());
    stop(&fx, &mut peer).await;
}

#[cfg(test)]
mod carrier_quiescence_tests;
