//! F2-B: real settlement → harness prepare → early dynamic call → ACK → kernel recovery.
use super::*;
use calm_server::semantic_recovery::test_support;
use calm_server::shared_codex_appserver::TurnStartReturnHook;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

async fn call(
    fx: &Fixture,
    turn: &str,
    call: &str,
    reason: &str,
) -> calm_server::error::Result<Value> {
    test_support::call(
        fx.boot.repo.clone(),
        fx.boot.ctx.events.clone(),
        fx.boot.ctx.write.clone(),
        calm_server::codex_appserver::DynamicToolCallParams {
            thread_id: "planner-observer".into(),
            turn_id: turn.into(),
            call_id: call.into(),
            tool: "Recover".into(),
            namespace: None,
            arguments: json!({"key":"retry","reason":reason}),
        },
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn semantic_recovery_binds_actual_harness_input_before_ack_and_replays_one_successor() {
    let (fx, first) = planner_failure().await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(fx.boot.repo.clone(), None);
    let handle = planner_with_daemon(&fx, daemon.clone()).await;
    test_support::register_thread(
        fx.boot.repo.as_ref(),
        fx.boot.planner_card_id.as_str(),
        "planner-observer",
    )
    .await
    .unwrap();
    let hook = TurnStartReturnHook {
        entered: std::sync::Arc::new(tokio::sync::Notify::new()),
        release: std::sync::Arc::new(tokio::sync::Notify::new()),
    };
    daemon.install_turn_start_return_hook_for_test(hook.clone());
    queued_settlement(&fx, &handle).await;
    handle
        .force_phase_for_dev(calm_server::harness::HarnessPhaseTag::Idle)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
        .await
        .unwrap();
    let pool = fx.boot.repo.sqlite_pool().unwrap();
    let (input, actions): (String, String) =
        sqlx::query_as("SELECT input_json,actions_json FROM planner_recovery_issuances")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&input).unwrap(),
        serde_json::to_value(&daemon.started_turns_for_test()[0].1).unwrap()
    );
    assert!(input.contains("Prefer Recover over calm.plan.recover"));
    let actions: Value = serde_json::from_str(&actions).unwrap();
    assert_eq!(actions[0]["expected_attempt_id"], first.id);
    assert_eq!(actions[0]["capability"]["allowed"], true);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM planner_recovery_turns")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "unacknowledged turn has no authority");

    let (_client, _notifications, mut peer) = test_support::connection(
        fx.boot.repo.clone(),
        fx.boot.ctx.events.clone(),
        fx.boot.ctx.write.clone(),
    )
    .await;
    let reason = "Retry unchanged goal in new empty workspace";
    peer.send(Message::Text(
        json!({"id":"early-call","method":"item/tool/call","params":{
        "threadId":"planner-observer","turnId":"fake-turn-0001","callId":"call-a","tool":"Recover",
        "arguments":{"key":"retry","reason":reason}}})
        .to_string(),
    ))
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), peer.next())
            .await
            .is_err()
    );
    assert_eq!(
        fx.boot
            .repo
            .task_current_get(fx.boot.track_id.as_str(), "retry")
            .await
            .unwrap()
            .unwrap()
            .id,
        first.id
    );
    hook.release.notify_one();
    let response = tokio::time::timeout(Duration::from_secs(5), peer.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Message::Text(response) = response else {
        panic!("dynamic response")
    };
    let response: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["id"], "early-call");
    assert_eq!(response["result"]["success"], true, "{response}");
    let value: Value = serde_json::from_str(
        response["result"]["contentItems"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(value["receipt"]["previous_attempt_id"], first.id);
    assert_eq!(value["receipt"]["generation"], 2);
    assert_eq!(value["status"], "accepted");
    // Finish harness before further assertions; this does not supersede its session.
    handle.shutdown().await.unwrap();
    assert_eq!(
        call(&fx, "fake-turn-0001", "call-a", reason).await.unwrap(),
        value
    );
    assert_eq!(
        call(&fx, "fake-turn-0001", "call-b", reason).await.unwrap(),
        value
    );
    assert!(
        call(&fx, "fake-turn-0001", "call-a", "different reason")
            .await
            .unwrap_err()
            .to_string()
            .contains("different arguments")
    );
    assert!(
        call(&fx, "fake-turn-0001", "call-c", "different reason")
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM task_attempt_allocations WHERE track_id=?1 AND key='retry'",
    )
    .bind(fx.boot.track_id.as_str())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 2);
    // Rebuild the consumer and clear presentation state: durable binding still replays.
    sqlx::query("UPDATE worker_sessions SET handle_state_json=NULL WHERE id=?1")
        .bind(planner_identity(&fx.boot).session_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(&fx, "fake-turn-0001", "after-restart", reason)
            .await
            .unwrap(),
        value
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn semantic_recovery_refuses_user_owned_and_superseded_attempt_without_retargeting() {
    for user_owned in [true, false] {
        let (fx, first) = if user_owned {
            let (fx, first, _) = stopped_failure().await;
            (fx, first)
        } else {
            planner_failure().await
        };
        fx.state.dispatcher.abort_event_listener_for_test();
        let daemon =
            SharedCodexAppServer::new_fake_running_with_pending(fx.boot.repo.clone(), None);
        let handle = planner_with_daemon(&fx, daemon.clone()).await;
        test_support::register_thread(
            fx.boot.repo.as_ref(),
            fx.boot.planner_card_id.as_str(),
            "planner-observer",
        )
        .await
        .unwrap();
        queued_settlement(&fx, &handle).await;
        let brief = issued_briefing(&handle, &daemon).await;
        assert_eq!(brief["planner_recovery"]["allowed"], !user_owned);
        let expected = if user_owned {
            first.id.clone()
        } else {
            fx.state.dispatcher.semaphore().close();
            let (status, receipt) =
                rest(&fx, "POST", &route(&fx, "recover"), recovery(&first)).await;
            assert_eq!(status, StatusCode::OK, "{receipt}");
            receipt["attempt_id"].as_str().unwrap().to_string()
        };
        assert!(call(&fx, "fake-turn-0001", "late", "retry").await.is_err());
        assert_eq!(
            fx.boot
                .repo
                .task_current_get(fx.boot.track_id.as_str(), "retry")
                .await
                .unwrap()
                .unwrap()
                .id,
            expected
        );
    }
}
