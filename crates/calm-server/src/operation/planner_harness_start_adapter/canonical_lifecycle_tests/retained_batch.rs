#![cfg(test)]
//! Retained activation observes the complete batch on the original carrier.
use super::*;

pub(super) async fn run() {
    use crate::shared_codex_appserver::native_owner_tests::{
        assert_no_request, connect, notify_thread, request, send,
    };
    let fx = Fixture::new().await;
    let (_auth, _token, source) = source().await;
    assert!(wake(&fx, source.clone()).await.is_err());
    let write_db = (fx.repo.db()).clone();
    write_db.writer().with_writer(|write_session| Box::pin(async move {
        let mut tx = write_session.begin_immediate().await.unwrap();
        let mut carrier = crate::db::sqlite::conversation::current_input_content_carrier_tx(
            &mut tx,
            fx.card.as_str(),
        )
        .await
        .unwrap();
        let first_ref = carrier.checkpoint().pending[0].accepted_input_ref().unwrap();
        let second = crate::conversation_inputs::accept_human_tx(
            &mut tx,
            &source.current_principal().unwrap(),
            &mut carrier,
            HumanPayload {
                text: "second accepted before native ready".into(),
                bound_attachments: vec![],
            },
        )
        .await
        .unwrap();
        let expected_refs = [first_ref, second.input_ref];
        assert_eq!(carrier.checkpoint().pending.iter().map(|input| input.accepted_input_ref().unwrap()).collect::<Vec<_>>(), expected_refs);
        tx.commit().await.unwrap();
        let daemon = fx.state.shared_codex_appserver.clone();
        let (_client, mut peer) = connect(&daemon).await;
        let unrelated_repo = crate::operation::SqlxOperationRepo::new(fx.repo.db().clone());
        use crate::operation::OperationRepo;
        let unrelated = unrelated_repo
            .insert_operation(
                "planner-harness-start",
                OperationKey {
                    operation_key: new_id(),
                    idempotency_key: None,
                    payload_hash: "unrelated pending legacy operation".into(),
                },
                serde_json::to_value(&fx.payload).unwrap(),
            )
            .await
            .unwrap();
        let request_source = source.clone();
        let owning = async { wake(&fx, request_source).await };
        let native = async {
            let call = request(&mut peer).await;
            assert_eq!(call["method"], "thread/start");
            let operation = fx.repo.session_get(&calm_types::worker::WorkerSessionId(fx.worker.clone())).await.unwrap().unwrap().spawn_op_id.unwrap();
            assert_ne!(operation, unrelated);
            assert!(
                !call
                    .to_string()
                    .contains("permanent accepted canonical history")
            );
            send(
                &mut peer,
                json!({"id":call["id"],"result":{"thread":{"id":"retained-native-thread"}}}),
            )
            .await;
            let turn = "retained-batch-turn";
            {
                let call = loop {
                    let call = request(&mut peer).await;
                    if call["method"] == "turn/start" {
                        break call;
                    }
                    assert_eq!(call["method"], "config/read");
                    send(&mut peer, json!({"id":call["id"],"result":{"config":{}}})).await;
                };
                let row = fx.repo.session_get(&calm_types::worker::WorkerSessionId(fx.worker.clone())).await.unwrap().unwrap();
                let frame: CanonicalHarnessSnapshot = serde_json::from_value(row.handle_state_json.unwrap()).unwrap();
                assert!(frame.canonical_inputs.pending.is_empty());
                assert_eq!(frame.canonical_inputs.attempts.len(), 1);
                let active = frame.canonical_inputs.active.as_ref().unwrap();
                assert_eq!(active.submission_ids.len(), 1);
                use crate::test_support::canonical_batch::{assert_native_text, assert_submission, native_text};
                assert_submission(&fx.repo, fx.card.as_str(), &active.submission_ids[0], &[
                    (expected_refs[0].clone(), "permanent accepted canonical history"),
                    (expected_refs[1].clone(), "second accepted before native ready"),
                ], (LEGACY_OWNER_USER_ID, "Owner")).await;
                assert_eq!(call["params"]["threadId"], "retained-native-thread");
                assert_native_text(&native_text(&call["params"]["input"]),
                    &["permanent accepted canonical history", "second accepted before native ready"], "Owner");
                notify_thread(
                    &daemon,
                    &mut peer,
                    "retained-native-thread",
                    "turn/started",
                    json!({"id":turn,"status":"inProgress"}),
                )
                .await;
                send(
                    &mut peer,
                    json!({"id":call["id"],"result":{"turn":{"id":turn}}}),
                )
                .await;
                notify_thread(
                    &daemon,
                    &mut peer,
                    "retained-native-thread",
                    "turn/completed",
                    json!({"id":turn,"status":"completed"}),
                )
                .await;
            }
            operation
        };
        let (result, operation) = tokio::join!(owning, native);
        result.unwrap();
        let row = fx
            .repo
            .session_get(&calm_types::worker::WorkerSessionId(fx.worker.clone()))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.thread_id.as_deref(), Some("retained-native-thread"));
        assert_eq!(row.spawn_op_id.as_deref(), Some(operation.as_str()));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM worker_sessions")
                .fetch_one(fx.repo.pool())
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversation_messages")
                .fetch_one(fx.repo.pool())
                .await
                .unwrap(),
            2
        );
        let done = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let row = fx
                    .repo
                    .session_get(&calm_types::worker::WorkerSessionId(fx.worker.clone()))
                    .await
                    .unwrap()
                    .unwrap();
                let frame: CanonicalHarnessSnapshot =
                    serde_json::from_value(row.handle_state_json.unwrap()).unwrap();
                if frame.header.phase == HarnessPhaseTag::TurnCompleted
                    && frame.header.last_turn_id.as_deref() == Some("retained-batch-turn")
                {
                    assert!(frame.canonical_inputs.active.is_none());
                    assert!(frame.canonical_inputs.pending.is_empty());
                    assert_eq!(frame.canonical_inputs.attempts.len(), 1);
                    assert!(matches!(frame.canonical_inputs.attempts[0].delivery, calm_types::conversation::InputDeliveryFact::Accepted { .. }));
                    break frame;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let counts: (i64, i64) = sqlx::query_as("SELECT
            (SELECT count(*) FROM conversation_input_submissions WHERE card_id=?1),
            (SELECT count(*) FROM operations)")
            .bind(fx.card.as_str()).fetch_one(fx.repo.pool()).await.unwrap();
        assert_eq!(counts, (1, 2));
        assert_eq!(crate::test_support::canonical_batch::transcript_count(
            &fx.repo, fx.card.as_str(), Some(&fx.worker), Some("turn/completed"), None,
        ).await, 1);
        // Re-observing the same live source reuses the owning operation and
        // settled carrier; it cannot bind the unrelated pending operation.
        wake(&fx, source.clone()).await.unwrap();
        assert_no_request(&mut peer).await;
        let replayed = fx.repo.session_get(&calm_types::worker::WorkerSessionId(fx.worker.clone())).await.unwrap().unwrap();
        assert_eq!(replayed.thread_id.as_deref(), Some("retained-native-thread"));
        assert_eq!(replayed.spawn_op_id.as_deref(), Some(operation.as_str()));
        let replayed_frame: CanonicalHarnessSnapshot = serde_json::from_value(replayed.handle_state_json.unwrap()).unwrap();
        assert_eq!(replayed_frame.canonical_inputs, done.canonical_inputs);
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM operations").fetch_one(fx.repo.pool()).await.unwrap(), 2);
        eprintln!("retained: two Human refs, one submission, Accepted/END, same operation, source replay sends no RPC");
        assert_eq!(
            unrelated_repo
                .get_operation(&unrelated)
                .await
                .unwrap()
                .unwrap()
                .phase
                .tag(),
            PhaseTag::Pending,
            "the source-bearing exact invocation must not claim another pending operation"
        );
        let token_before = fx
            .repo
            .session_get(&calm_types::worker::WorkerSessionId(fx.worker.clone()))
            .await
            .unwrap()
            .unwrap()
            .mcp_token_hash;
        assert!(
            daemon
                .resume_candidates_for_test()
                .iter()
                .any(|(thread, card)| thread == "retained-native-thread" && card == fx.card.as_str())
        );
        let automatic =
            crate::shared_codex_appserver::native_owner_tests::resume_cached_without_source(&daemon);
        use futures_util::StreamExt;
        tokio::select! {
            ()=automatic=>{},
            _request=peer.next()=>panic!("automatic recovery crossed canonical source boundary"),
        }
        assert_no_request(&mut peer).await;
        assert!(
            token_before
                == fx
                    .repo
                    .session_get(&calm_types::worker::WorkerSessionId(fx.worker.clone()))
                    .await
                    .unwrap()
                    .unwrap()
                    .mcp_token_hash
        );
        let handle = fx.state.harness.get(&fx.worker).unwrap();
        let mut stopping = tokio::spawn(async move { handle.shutdown().await });
        tokio::time::timeout(std::time::Duration::from_secs(5),async {
            tokio::select! {
                stopped=&mut stopping=>stopped.unwrap().unwrap(),
                call=request(&mut peer)=> { assert_eq!(call["method"],"turn/interrupt");send(&mut peer,json!({"id":call["id"],"result":{}})).await;stopping.await.unwrap().unwrap(); }
            }
        }).await.unwrap();

    })).await.expect("independent write operation")
}
