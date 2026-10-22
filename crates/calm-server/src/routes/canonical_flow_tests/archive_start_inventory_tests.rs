#![cfg(test)]
//! Archive inventories must recognize unrelated starts produced by their owner.
use super::*;
use crate::operation::{OperationRepo, SqlxOperationRepo};
use crate::shared_codex_appserver::native_owner_tests::Peer;
use axum::extract::FromRef;
use calm_truth::db::RepoRead;

const PLANNER_B_THREAD: &str = "archive-existing-planner-thread";
const ASSISTANT_B_THREAD: &str = "archive-existing-assistant-thread";
const ASSISTANT_B_TURN: &str = "archive-existing-assistant-turn";
const START_A_THREAD: &str = "archive-unrelated-retained-thread";
const ASSISTANT_TEXT: &str = "Finish this discussion before archiving it";

async fn historical_response(state: &AppState, peer: &mut Peer, call: Value, thread: &str) -> bool {
    let (result, started) = match call["method"].as_str().unwrap() {
        "config/read" => (json!({"config":{}}), false),
        "thread/start" => (json!({"thread":{"id":thread}}), false),
        "turn/start" => {
            assert_eq!(thread, ASSISTANT_B_THREAD);
            assert_eq!(call["params"]["threadId"], ASSISTANT_B_THREAD);
            assert!(call["params"]["input"].to_string().contains(ASSISTANT_TEXT));
            (json!({"turn":{"id":ASSISTANT_B_TURN}}), true)
        }
        method => panic!("unexpected native request while creating Assistant B: {method}"),
    };
    send(peer, json!({"id":call["id"],"result":result})).await;
    if started {
        notify_thread(
            &state.shared_codex_appserver,
            peer,
            ASSISTANT_B_THREAD,
            "turn/started",
            json!({"id":ASSISTANT_B_TURN,"status":"inProgress"}),
        )
        .await;
    }
    started
}

async fn post_with_historical_peer(
    state: &AppState,
    peer: &mut Peer,
    thread: &str,
    response: impl std::future::Future<Output = Value>,
) -> (Value, bool) {
    tokio::pin!(response);
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut started = false;
        loop {
            tokio::select! {
                result = &mut response => break (result, started),
                call = request(peer) => {
                    let next = historical_response(state, peer, call, thread).await;
                    assert!(!(next && started), "Assistant B must start only one first turn");
                    started |= next;
                }
            }
        }
    })
    .await
    .expect("historical HTTP creation must finish through its real native responses")
}

async fn finished_assistant(
    fixture: &Fixture,
    state: &AppState,
    app: &axum::Router,
    cookie: &str,
    peer: &mut Peer,
    area: &Value,
) -> (String, String) {
    let (track, started) = post_with_historical_peer(
        state,
        peer,
        PLANNER_B_THREAD,
        post(
            app,
            cookie,
            "/api/tracks",
            "assistant-track",
            json!({
                "area_id":area["id"],"title":"Idle Assistant archive","planner_provider":"codex",
                "theme":{"fg":[255,255,255],"bg":[0,0,0]}
            }),
        ),
    )
    .await;
    assert!(!started, "an empty historical Planner has no first input");
    let track = track["id"].as_str().unwrap().to_owned();
    // This inventory fixture deliberately retains the supported historical entry.
    let create = async {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("Idempotency-Key", "assistant-b".parse().unwrap());
        let (_, crate::extract::Json(value)) = crate::routes::track_conversations::create_internal_conversation_for_test(
            state, headers, track.clone(), serde_json::from_value(json!({"provider":"codex","text":ASSISTANT_TEXT})).unwrap(),
            crate::operation::planner_harness_start_adapter::OpeningBriefing::TodaysActivityOnTheLaunchpad,
        ).await.unwrap();
        serde_json::to_value(value).unwrap()
    };
    let (assistant, mut started) =
        post_with_historical_peer(state, peer, ASSISTANT_B_THREAD, create).await;
    let card = assistant["id"].as_str().unwrap().to_owned();
    assert_eq!(
        fixture.repo.card_role_get(&card).await.unwrap(),
        Some(crate::model::CardRole::Assistant)
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while !started {
            let call = request(peer).await;
            started = historical_response(state, peer, call, ASSISTANT_B_THREAD).await;
        }
    })
    .await
    .expect("the actual Assistant must start its original first input");
    notify_thread(
        &state.shared_codex_appserver,
        peer,
        ASSISTANT_B_THREAD,
        "turn/completed",
        json!({"id":ASSISTANT_B_TURN,"status":"completed"}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let runtime = fixture
                .repo
                .session_projection_active_for_card(&card)
                .await
                .unwrap()
                .unwrap();
            let handle = state.harness.get(&runtime.id).unwrap();
            let snapshot = handle.snapshot().await.unwrap();
            if snapshot.phase == HarnessPhaseTag::TurnCompleted
                && snapshot.pending_entries().is_empty()
                && runtime.status == crate::session_projection_repo::WorkerSessionState::Idle
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("Assistant B must be genuinely idle after the real native END");
    (track, card)
}

async fn cleanup(state: &AppState, peer: &mut Peer) {
    for handle in state.harness.drain_all_for_dev() {
        let mut stopping = tokio::spawn(async move { handle.shutdown().await });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                tokio::select! {
                    result = &mut stopping => break result.unwrap().unwrap(),
                    call = request(peer) => {
                        assert_eq!(call["method"], "turn/interrupt");
                        send(peer, json!({"id":call["id"],"result":{}})).await;
                    }
                }
            }
        })
        .await
        .expect("normal harness shutdown must finish before the final archive assertion");
    }
}

#[derive(Debug, PartialEq)]
struct HeldStart {
    operation: String,
    raw_payload_and_output: (String, Option<String>, Option<String>),
    raw_worker: (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    ),
}

async fn held_start(fixture: &Fixture, operation: &str, worker: &str) -> HeldStart {
    let operations = SqlxOperationRepo::new(fixture.repo.db().clone());
    HeldStart {
        operation: format!("{:?}", operations.get_operation(operation).await.unwrap().unwrap()),
        raw_payload_and_output: sqlx::query_as(
            "SELECT payload_json,phase_detail_json,tx_output_json FROM operations WHERE id=?1",
        ).bind(operation).fetch_one(fixture.repo.pool()).await.unwrap(),
        raw_worker: sqlx::query_as(
            "SELECT handle_state_json,thread_id,spawn_op_id,mcp_token_hash,state FROM worker_sessions WHERE id=?1",
        ).bind(worker).fetch_one(fixture.repo.pool()).await.unwrap(),
    }
}

async fn archive_with_retained_inventory(orphan: bool) {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let (client, mut peer) = connect(&state.shared_codex_appserver).await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "area",
        json!({"name":"Archive inventory interaction","color":"#123456"}),
    )
    .await;
    let (track_b, card_b) =
        finished_assistant(&fixture, &state, &app, &cookie, &mut peer, &area).await;
    let route = crate::state::RouteState::from_ref(&state);
    let operations = SqlxOperationRepo::new(fixture.repo.db().clone());
    assert!(
        operations
            .abandoned_running_operations_on_boot()
            .await
            .unwrap()
            .iter()
            .all(|op| op.kind != "planner-harness-start"),
        "B's real starts already finished"
    );
    let retained = if orphan {
        // Only fail phase advances after real interaction. Within-phase
        // crossing/binding writes, all other operations and native I/O remain real.
        sqlx::query(
            "CREATE TRIGGER hold_retained_archive_phase BEFORE UPDATE OF phase ON operations
             WHEN NEW.kind='planner-harness-start'
               AND json_extract(NEW.payload_json,'$.canonical_activation_version')=2
               AND OLD.phase='app_server_interact' AND NEW.phase<>OLD.phase
             BEGIN SELECT RAISE(ABORT,'fixture refuses retained phase advance'); END",
        )
        .execute(fixture.repo.pool())
        .await
        .unwrap();
        let created = post(
            &app,
            &cookie,
            "/api/tracks",
            "first",
            json!({
                "area_id":area["id"],"title":"Unrelated accepted A","planner_provider":"codex",
                "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY
            }),
        )
        .await;
        let track_a = created["id"].as_str().unwrap().to_owned();
        let (card_a, worker_a, _) = fixture.carrier(&track_a).await;
        let mut starts = 0;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let call = request(&mut peer).await;
                let result = match call["method"].as_str().unwrap() {
                    "config/read" => json!({"config":{}}),
                    "thread/start" => {
                        starts += 1;
                        json!({"thread":{"id":START_A_THREAD}})
                    }
                    method => panic!("A must bind its native start before any turn: {method}"),
                };
                send(&mut peer, json!({"id":call["id"],"result":result})).await;
                if starts == 1 {
                    break;
                }
            }
        })
        .await
        .expect("the real canonical activation must issue its original native thread/start");
        let operation: String = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let id: Option<String> = sqlx::query_scalar(
                    "SELECT id FROM operations WHERE kind='planner-harness-start'
                     AND phase='app_server_interact' AND json_extract(payload_json,'$.card_id')=?1
                     AND json_extract(tx_output_json,'$.data.activation')='bound'",
                )
                .bind(&card_a)
                .fetch_optional(fixture.repo.pool())
                .await
                .unwrap();
                if let Some(id) = id {
                    break id;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("production must retain its bound output when phase advance fails");
        // Ordinary in-flight A would still hold these locks and block archive.
        // Both must be genuinely released after the storage refusal propagates.
        let card_fence = tokio::time::timeout(
            Duration::from_secs(5),
            crate::per_card_lock::lock_card(&route.planner_recovery_locks, &card_a),
        )
        .await
        .unwrap();
        let drive = tokio::time::timeout(
            Duration::from_secs(5),
            route.operation_runtime.lock_for_track_delete(),
        )
        .await
        .unwrap();
        let inventory = operations
            .abandoned_running_operations_on_boot()
            .await
            .unwrap();
        let original = inventory
            .iter()
            .find(|op| op.id == operation)
            .expect("the actual recovery inventory contains A");
        assert_eq!(original.payload["canonical_activation_version"], 2);
        assert_eq!(original.payload["card_id"], card_a);
        assert_eq!(original.payload["worker_session_id"], worker_a);
        let snapshot = fixture.carrier(&track_a).await.2;
        assert_eq!(
            snapshot.header.last_thread_id.as_deref(),
            Some(START_A_THREAD)
        );
        assert_eq!(snapshot.canonical_inputs.pending.len(), 1);
        assert!(snapshot.canonical_inputs.active.is_none());
        assert!(snapshot.canonical_inputs.attempts.is_empty());
        let before = held_start(&fixture, &operation, &worker_a).await;
        let receipt = fixture.receipt(area["id"].as_str().unwrap()).await;
        let counts = fixture.counts(&card_a).await;
        assert_eq!(counts, (1, 1, 1, 0, 0));
        drop((drive, card_fence));
        Some((
            track_a, card_a, worker_a, operation, snapshot, before, receipt, counts,
        ))
    } else {
        None
    };
    let archive_request = Request::patch(format!(
        "/api/tracks/{track_b}/conversations/{card_b}/archive"
    ))
    .header(header::COOKIE, &cookie)
    .header(header::CONTENT_TYPE, "application/json")
    .body(Body::from(json!({"archived":true}).to_string()))
    .unwrap();
    let response = {
        let response = app.clone().oneshot(archive_request);
        tokio::pin!(response);
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                response = &mut response => response.unwrap(),
                call = request(&mut peer) => panic!("archive must not execute or resume A: {}",call["method"]),
            }
        }).await.expect("archive must run after the original start invocation released its locks")
    };
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let archived = crate::db::RepoRead::conversation_archived_at(fixture.repo.as_ref(), &card_b)
        .await
        .unwrap();
    let mut preserved = None;
    if let Some((track, card, worker, operation, snapshot, before, receipt, counts)) = retained {
        let after = held_start(&fixture, &operation, &worker).await;
        let after_snapshot = fixture.carrier(&track).await.2;
        let after_receipt = fixture.receipt(area["id"].as_str().unwrap()).await;
        let after_counts = fixture.counts(&card).await;
        let starts: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM operations WHERE kind='planner-harness-start' AND json_extract(payload_json,'$.card_id')=?1",
        ).bind(&card).fetch_one(fixture.repo.pool()).await.unwrap();
        sqlx::query("DROP TRIGGER hold_retained_archive_phase")
            .execute(fixture.repo.pool())
            .await
            .unwrap();
        preserved = Some((
            before,
            after,
            snapshot,
            after_snapshot,
            receipt,
            after_receipt,
            counts,
            after_counts,
            starts,
        ));
    }
    drop(route);
    cleanup(&state, &mut peer).await;
    peer.close(None).await.unwrap();
    drop((client, peer, state, app));
    fixture.repo.pool().close().await;
    // Expected-red assertions run only after the real fault and resources close.
    if let Some((
        before,
        after,
        snapshot,
        after_snapshot,
        receipt,
        after_receipt,
        counts,
        after_counts,
        starts,
    )) = preserved
    {
        assert_eq!(
            after, before,
            "archive must preserve A's original operation and worker bytes"
        );
        assert_eq!(
            after_snapshot, snapshot,
            "A's accepted pending input is unchanged"
        );
        assert_eq!(after_receipt, receipt);
        assert_eq!(after_counts, counts);
        assert_eq!(starts, 1, "archive creates no duplicate activation");
    }
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "archive B with retained A inventory: {body}"
    );
    assert!(archived.is_some());
}

#[tokio::test]
async fn production_http_archives_idle_assistant_without_retained_start_inventory() {
    archive_with_retained_inventory(false).await;
}

#[tokio::test]
async fn production_http_archives_idle_assistant_with_unrelated_bound_retained_start() {
    archive_with_retained_inventory(true).await;
}
