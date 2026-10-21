//! A real HTTP send/END, replacement and next authorized activation. Only the
//! Codex app-server protocol is fake; persistence and lifecycle owners are real.
use super::*;
use crate::db::sqlite::conversation::{
    self, CallerRevision, ExpectedCarrier, RuntimeExecutionFence,
};
use calm_truth::planner_binding::PlannerBinding;
use calm_types::worker::WorkerSessionId;

async fn history(app: &axum::Router, cookie: &str, card: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::get(format!(
                "/api/cards/{card}/harness/items?contract=conversation_v1&limit=1"
            ))
            .header(header::COOKIE, cookie)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[tokio::test]
async fn carrier_replacement_codex_next_send_history_credentials_and_cold_boot() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "area",
        json!({"name":"Carrier replacement","color":"#123456"}),
    )
    .await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let created = post(
        &app,
        &cookie,
        "/api/tracks",
        "first",
        json!({
            "area_id":area["id"],"title":"","planner_provider":"codex",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY
        }),
    )
    .await;
    let track = created["id"].as_str().unwrap();
    let first = super::native_attempt_recovery_tests::initial_request(&mut peer).await;
    send(
        &mut peer,
        json!({"id":first["id"],"result":{"turn":{"id":TURN}}}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/started",
        json!({"id":TURN,"status":"inProgress"}),
    )
    .await;
    fixture
        .wait_phase(track, HarnessPhaseTag::TurnRunning)
        .await;
    let (card, worker, _) = fixture.carrier(track).await;
    let handle = state.harness.get(&worker).unwrap();
    let context = state.mcp_context();
    let calls = context.tool_calls.fence(&worker).unwrap();

    assert!(
        handle.idle_carrier_owner().await.is_err(),
        "active native turn refuses replacement"
    );

    drop(calls);
    send(&mut peer, json!({"method":"item/completed","params":{
        "threadId":THREAD,"turnId":TURN,
        "item":{"id":"original-native-reply","type":"agentMessage","text":"Original execution result"}
    }})).await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/completed",
        json!({"id":TURN,"status":"completed"}),
    )
    .await;
    fixture
        .wait_phase(track, HarnessPhaseTag::TurnCompleted)
        .await;
    fixture.wait_for_stored_reply(&card).await;
    let before_history = history(&app, &cookie, &card).await;
    assert!(
        !before_history["next_cursor"].is_null(),
        "old history remains paginatable"
    );
    let before_facts: Vec<(String,String,String)> = sqlx::query_as("SELECT id,initiating_runtime_id,initial_submission_id FROM conversation_turns WHERE card_id=?1")
        .bind(&card).fetch_all(fixture.repo.pool()).await.unwrap();
    let old_hash: String =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    let retirement = state.harness.carrier_retirement_slot(&worker);
    let calls = context.tool_calls.fence(&worker).unwrap();

    let idle = handle.idle_carrier_owner().await.unwrap();
    idle.recheck().await.unwrap();
    let owner = crate::harness::carrier_replacement::CarrierReplacementOwner {
        idle: &idle,
        calls: &calls,
        dispatcher: &state.dispatcher,
        track: track.into(),
        arrivals: &[],
    };
    let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.db())
        .await
        .unwrap();
    let carrier = conversation::current_input_carrier_tx(&mut tx, &card, &worker)
        .await
        .unwrap();
    use conversation::CarrierReplacementGuard;
    owner.recheck_tx(&mut tx, &carrier).await.unwrap();
    for refusal in ["start", "wake", "hold"] {
        sqlx::query("SAVEPOINT refusal")
            .execute(&mut **tx)
            .await
            .unwrap();
        if refusal == "start" {
            sqlx::query(concat!("INSERT INTO ","operations(id,operation_key,kind,payload_hash,target_type,target_json,payload_json,phase,created_at_ms,up","dated_at_ms) ","VALUES('reserved-start','reserved-start','planner-harness-start','hash','card','{}',?1,'pending',0,0)"))
                .bind(json!({"canonical_activation_version":2,"card_id":card,"worker_session_id":worker}).to_string())
                .execute(&mut **tx).await.unwrap();
        } else {
            let questions = vec![crate::event::AskQuestion {
                title: "Still open".into(),
                options: vec!["Yes".into()],
            }];
            let (scope, event) = if refusal == "hold" {
                crate::ask::hold_ask_requested_tx(&mut tx, &card.clone().into(), questions)
                    .await
                    .unwrap()
            } else {
                crate::ask::ask_requested_tx(&mut tx, &card.clone().into(), questions)
                    .await
                    .unwrap()
            };
            crate::db::sqlite::append_decision_event_in_tx(
                &mut tx,
                &crate::ids::ActorId::AiPlannerSession(worker.clone().into()),
                &scope,
                None,
                &event,
            )
            .await
            .unwrap();
        }
        assert!(
            owner.recheck_tx(&mut tx, &carrier).await.is_err(),
            "{refusal}"
        );
        sqlx::query("ROLLBACK TO refusal")
            .execute(&mut **tx)
            .await
            .unwrap();
        sqlx::query("RELEASE refusal")
            .execute(&mut **tx)
            .await
            .unwrap();
    }
    let stored = fixture.repo.card_get(&card).await.unwrap().unwrap();
    let binding = PlannerBinding::from_card(&stored, crate::model::CardRole::Planner).unwrap();
    let expected = ExpectedCarrier {
        card_id: card.clone(),
        track_id: track.into(),
        worker_session_id: worker.clone(),
        original_snapshot: carrier.original_snapshot().into(),
        execution: RuntimeExecutionFence::from_carrier(&carrier),
        accepted_prefix: carrier.snapshot()["push_watermark"].as_i64().unwrap(),
        binding: binding.clone(),
        ingress: conversation::carrier_ingress_revision_tx(&mut tx, &card, track, &worker)
            .await
            .unwrap(),
        owner: &owner,
        revision: CallerRevision::Unversioned,
    };
    let replacement = conversation::replace_idle_carrier_tx(&mut tx, expected, binding)
        .await
        .unwrap();
    replacement.repoint_root_tx(&mut tx).await.unwrap();
    tx.commit().await.unwrap();

    // Existing publication owners may send thread/unsubscribe; answer their real
    // native protocol while cleanup completes.
    let publish = state.harness.publish_carrier_replacement(
        &replacement,
        retirement,
        fixture.repo.card_role_cache(),
        Some(&state.shared_codex_appserver),
    );
    tokio::pin!(publish);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop { tokio::select! {
            result = &mut publish => { result.unwrap(); break; },
            call = request(&mut peer) => {
                assert!(matches!(call["method"].as_str(),Some("thread/unsubscribe"|"config/read"|"turn/interrupt")),"{call}");
                send(&mut peer,json!({"id":call["id"],"result":{}})).await;
            }
        }}
    }).await.unwrap();

    drop(idle);
    drop(calls);
    assert!(state.harness.get(&worker).is_none());
    assert_eq!(
        state.shared_codex_appserver.cached_card_for_thread(THREAD),
        None
    );
    assert_eq!(history(&app, &cookie, &card).await, before_history);
    let after_facts: Vec<(String,String,String)> = sqlx::query_as("SELECT id,initiating_runtime_id,initial_submission_id FROM conversation_turns WHERE card_id=?1")
        .bind(&card).fetch_all(fixture.repo.pool()).await.unwrap();
    assert_eq!(before_facts, after_facts);
    let boot = fixture
        .repo
        .session_projection_recover_harnesses_on_boot()
        .await
        .unwrap();
    assert!(
        !boot.iter().any(|row| row.id == worker),
        "cold boot must reject retired rows"
    );
    let next = post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "second",
        json!({"text":"Fresh carrier only"}),
    )
    .await;
    assert_eq!(
        next["worker_session_id"],
        replacement.new_worker.id.as_str()
    );
    let next_call = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let call = request(&mut peer).await;
            let result = match call["method"].as_str().unwrap() {
                "config/read" => json!({"config":{}}),
                "thread/start" => json!({"thread":{"id":"replacement-thread"}}),
                "turn/start" => break call,
                method => panic!("successor must start fresh: {method}"),
            };
            send(&mut peer, json!({"id":call["id"],"result":result})).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(next_call["params"]["threadId"], "replacement-thread");
    assert!(
        next_call["params"]["input"]
            .to_string()
            .contains("Fresh carrier only")
    );
    assert!(
        !next_call["params"]["input"]
            .to_string()
            .contains(FIRST_BODY)
    );
    let new_hash: String =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?1")
            .bind(replacement.new_worker.id.as_str())
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_ne!(old_hash, new_hash);
    let mirror: String =
        sqlx::query_scalar("SELECT hashed_token FROM card_mcp_tokens WHERE card_id=?1")
            .bind(&card)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(new_hash, mirror);
    let mut tx = fixture.repo.pool().begin().await.unwrap();
    assert!(
        crate::db::sqlite::session_get_tx(&mut tx, &WorkerSessionId(worker.clone()))
            .await
            .unwrap()
            .unwrap()
            .mcp_token_hash
            .is_none()
    );
    tx.rollback().await.unwrap();
    send(
        &mut peer,
        json!({"id":next_call["id"],"result":{"turn":{"id":"replacement-turn"}}}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        "replacement-thread",
        "turn/started",
        json!({"id":"replacement-turn","status":"inProgress"}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        "replacement-thread",
        "turn/completed",
        json!({"id":"replacement-turn","status":"completed"}),
    )
    .await;
    fixture
        .wait_phase(track, HarnessPhaseTag::TurnCompleted)
        .await;
    fixture.shutdown_process(&state).await;
}
