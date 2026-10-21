//! Retry through the real session extractor and native transport, never raw receipts.
use super::*;
use crate::shared_codex_appserver::native_owner_tests::Peer;

async fn held(
    fixture: &Fixture,
    state: &AppState,
    app: &axum::Router,
    cookie: &str,
    frozen: bool,
) -> (String, String, String, CanonicalHarnessSnapshot, Peer) {
    let area = post(
        app,
        cookie,
        "/api/areas",
        "area",
        json!({"name":"Retry authority","color":"#123456"}),
    )
    .await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let mut body = json!({"area_id":area["id"],"title":"","planner_provider":"codex","theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY});
    if frozen {
        body["model"] = json!("retry-model");
        body["reasoning_effort"] = json!("high");
    }
    let create = post(app, cookie, "/api/tracks", "create", body);
    let native = async {
        if frozen {
            let catalog = request(&mut peer).await;
            assert_eq!(catalog["method"], "model/list");
            send(
                &mut peer,
                json!({"id":catalog["id"],"error":{"code":-32603,"message":"catalog unavailable"}}),
            )
            .await;
        }
        super::super::native_attempt_recovery_tests::initial_request(&mut peer).await
    };
    let (created, initial) = tokio::join!(create, native);
    send(&mut peer, json!({"id":initial["id"],"result":{"turn":{}}})).await;
    let track = created["id"].as_str().unwrap().to_owned();
    let (card, worker, frame) = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let facts = fixture.carrier(&track).await;
            if matches!(
                facts.2.canonical_inputs.attempts[0].delivery,
                InputDeliveryFact::Unknown
            ) {
                break facts;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    state.harness.get(&worker).unwrap().pause_issuance_for_dev();
    (track, card, worker, frame, peer)
}
fn args(worker: &str, frame: &CanonicalHarnessSnapshot) -> Value {
    let attempt = &frame.canonical_inputs.attempts[0];
    json!({"carrier_id":worker,"submission_id":attempt.attempt.submission_id,"attempt_id":attempt.attempt.attempt_id,"revision":attempt.revision.to_string()})
}
async fn run(app: &axum::Router, cookie: &str, card: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::get(format!(
                "/api/cards/{card}/planner/run?contract=conversation_v1"
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
async fn canonical_retry_http_response_loss_replays_original_successor() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let (track, card, worker, before, _peer) = held(&fixture, &state, &app, &cookie, true).await;
    let uri = format!("/api/cards/{card}/conversation/input/retry");
    let request = args(&worker, &before);
    let first = response(&app, &cookie, &uri, "retry", request.clone()).await;
    assert_eq!(first.0, StatusCode::OK, "{}", first.1);
    assert!(first.1["retry_intent_id"].as_str().is_some());
    let committed = fixture.carrier(&track).await.2.canonical_inputs;
    assert_eq!(
        response(&app, &cookie, &uri, "retry", request.clone()).await,
        first
    );
    assert_eq!(fixture.carrier(&track).await.2.canonical_inputs, committed);
    let intent = calm_types::conversation::RetryIntentId::parse(
        first.1["retry_intent_id"].as_str().unwrap(),
    )
    .unwrap();
    let identity = calm_types::conversation::CanonicalPendingIdentity::ExplicitSuccessor {
        retry_intent_id: intent.clone(),
    };
    let sources = state
        .harness
        .canonical_sources(&worker, &track.clone().into());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(source) = sources.source(&identity) {
                source.require_submission_intent(Some(&intent)).unwrap();
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("post-commit HTTP wake attaches only the live exact Human repeat source");
    let read = run(&app, &cookie, &card).await;
    assert_eq!(read["pending_inputs"][0]["kind"], "explicit_successor");
    assert_eq!(
        read["pending_inputs"][0]["retry_intent_id"],
        first.1["retry_intent_id"]
    );
    assert_eq!(
        read["active_input"]["submissions"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "no fictitious queued submission"
    );
    assert_eq!(
        read["active_input"]["submissions"][0]["attempt"]["resolution"],
        json!({"kind":"retry","result":first.1})
    );
    // A failed current runtime and a refused wake cannot undo a committed answer.
    let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.db())
        .await
        .unwrap();
    crate::db::sqlite::session_fail_if_active_runtime_tx(&mut tx, &worker)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let removed = state.harness.remove(&worker);
    if let Some(handle) = removed {
        handle.pause_issuance_for_dev();
    }
    assert_eq!(response(&app, &cookie, &uri, "retry", request).await, first);
    fixture.shutdown_process(&state).await;
}
#[tokio::test]
async fn canonical_retry_http_racing_keep_and_retry_resolves_once() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let (track, card, worker, before, _peer) = held(&fixture, &state, &app, &cookie, true).await;
    let request = args(&worker, &before);
    let keep = format!("/api/cards/{card}/conversation/input/keep");
    let retry = format!("/api/cards/{card}/conversation/input/retry");
    let (a, b) = tokio::join!(
        response(&app, &cookie, &keep, "keep", request.clone()),
        response(&app, &cookie, &retry, "retry", request.clone())
    );
    assert!(
        matches!(
            (a.0, b.0),
            (StatusCode::OK, StatusCode::CONFLICT) | (StatusCode::CONFLICT, StatusCode::OK)
        ),
        "{a:?} {b:?}"
    );
    let after = fixture.carrier(&track).await.2.canonical_inputs;
    assert_eq!(after.receipts.len(), 1);
    assert_eq!(after.pending.len(), usize::from(b.0 == StatusCode::OK));
    let committed = if a.0 == StatusCode::OK {
        "keep"
    } else {
        "retry"
    };
    let other_uri = if a.0 == StatusCode::OK { &retry } else { &keep };
    assert_eq!(
        response(&app, &cookie, other_uri, committed, request)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(fixture.carrier(&track).await.2.canonical_inputs, after);
    fixture.shutdown_process(&state).await;
}
#[tokio::test]
async fn canonical_retry_http_erased_or_unproven_context_is_keep_only() {
    for (frozen, withdrawn) in [(false, false), (true, false), (true, true)] {
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        let (track, card, worker, before, _peer) =
            held(&fixture, &state, &app, &cookie, frozen).await;
        if frozen {
            let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.db())
                .await
                .unwrap();
            if withdrawn {
                sqlx::query("UPDATE conversation_messages SET withdrawn_at_ms=1 WHERE card_id=?1")
                    .bind(&card)
                    .execute(&mut **tx)
                    .await
                    .unwrap();
            } else {
                let payload = calm_types::conversation::PrivateHistoryPayload::<
                    calm_types::conversation::HumanPayload,
                >::ErasedByPrivateHistoryOperation {
                    operation_id: calm_types::conversation::PrivateHistoryOperationId::parse(
                        &crate::model::new_id(),
                    )
                    .unwrap(),
                };
                sqlx::query(
                    "UPDATE conversation_message_revisions SET payload_json=?1
                    WHERE message_id IN (SELECT id FROM conversation_messages WHERE card_id=?2)",
                )
                .bind(serde_json::to_string(&payload).unwrap())
                .bind(&card)
                .execute(&mut **tx)
                .await
                .unwrap();
            }
            tx.commit().await.unwrap();
        }
        let expected = if frozen {
            "erased_or_withdrawn"
        } else {
            "invocation_not_fully_frozen"
        };
        let read = run(&app, &cookie, &card).await;
        assert_eq!(
            read["active_input"]["submissions"][0]["attempt"]["actions"]["retry"],
            json!({"kind":"unavailable","reason":expected})
        );
        let uri = format!("/api/cards/{card}/conversation/input/retry");
        assert_eq!(
            response(&app, &cookie, &uri, "retry", args(&worker, &before))
                .await
                .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            fixture.carrier(&track).await.2.canonical_inputs,
            before.canonical_inputs
        );
        let keep = format!("/api/cards/{card}/conversation/input/keep");
        assert_eq!(
            response(&app, &cookie, &keep, "keep", args(&worker, &before))
                .await
                .0,
            StatusCode::OK
        );
        fixture.shutdown_process(&state).await;
    }
}
#[tokio::test]
async fn canonical_retry_http_agent_logout_and_revoked_access_have_zero_writes() {
    let fixture = Fixture::new().await;
    let (state, app, cookie, auth) = fixture.application_with_auth(false).await;
    let (track, card, worker, before, _peer) = held(&fixture, &state, &app, &cookie, true).await;
    let uri = format!("/api/cards/{card}/conversation/input/retry");
    let request = args(&worker, &before);
    let persisted: (String, i64) = sqlx::query_as(
        "SELECT handle_state_json,(SELECT count(*) FROM events) FROM worker_sessions WHERE id=?1",
    )
    .bind(&worker)
    .fetch_one(fixture.repo.pool())
    .await
    .unwrap();
    for actor in ["ai:codex", "dispatcher"] {
        let denied = app
            .clone()
            .oneshot(
                Request::post(&uri)
                    .header("X-Calm-Actor", actor)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("Idempotency-Key", "agent")
                    .body(Body::from(request.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    }
    let first = response(&app, &cookie, &uri, "retry", request.clone()).await;
    assert_eq!(first.0, StatusCode::OK, "{}", first.1);
    let committed = fixture.carrier(&track).await.2.canonical_inputs;
    let saved: (String, i64) = sqlx::query_as(
        "SELECT handle_state_json,(SELECT count(*) FROM events) FROM worker_sessions WHERE id=?1",
    )
    .bind(&worker)
    .fetch_one(fixture.repo.pool())
    .await
    .unwrap();
    assert_ne!(saved.0, persisted.0);
    let token = cookie.split_once('=').unwrap().1;
    let session = auth.sessions.get(token).unwrap();
    let area: String = sqlx::query_scalar("SELECT area_id FROM tracks WHERE id=?1")
        .bind(&track)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM area_members WHERE area_id=?1 AND user_id=?2")
        .bind(area)
        .bind(&session.identity.user_id)
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    for key in ["retry", "fresh"] {
        let denied = response(&app, &cookie, &uri, key, request.clone()).await;
        assert!(
            matches!(denied.0, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
            "{denied:?}"
        );
        assert!(denied.1.get("retry_intent_id").is_none());
        let after: (String,i64)=sqlx::query_as("SELECT handle_state_json,(SELECT count(*) FROM events) FROM worker_sessions WHERE id=?1").bind(&worker).fetch_one(fixture.repo.pool()).await.unwrap();
        assert_eq!(after, saved);
    }
    post(&app, &cookie, "/api/auth/logout", "logout", json!({})).await;
    assert_eq!(
        response(&app, &cookie, &uri, "retry", request).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(fixture.carrier(&track).await.2.canonical_inputs, committed);
    fixture.shutdown_process(&state).await;
}
#[tokio::test]
async fn canonical_retry_http_reopen_requires_live_repeat_source() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let (track, card, worker, before, _peer) = held(&fixture, &state, &app, &cookie, true).await;
    let uri = format!("/api/cards/{card}/conversation/input/retry");
    let request = args(&worker, &before);
    let first = response(&app, &cookie, &uri, "retry", request.clone()).await;
    assert_eq!(first.0, StatusCode::OK, "{}", first.1);
    let committed = fixture.carrier(&track).await.2.canonical_inputs;
    fixture.shutdown_process(&state).await;
    let reopened = SqlxRepo::open(&fixture.url).await.unwrap();
    let read = crate::db::RepoRead::conversation_read_frame(&reopened, &card)
        .await
        .unwrap();
    let snapshot: CanonicalHarnessSnapshot =
        serde_json::from_value(read.snapshot.unwrap()).unwrap();
    assert_eq!(snapshot.canonical_inputs, committed);
    let (new_state, new_app, new_cookie) = fixture.application().await;
    let sources = new_state
        .harness
        .canonical_sources(&worker, &track.clone().into());
    assert!(
        sources.is_empty(),
        "reopen must not reconstruct a stored source grant"
    );
    assert_eq!(
        response(&new_app, &new_cookie, &uri, "retry", request).await,
        first
    );
    assert_eq!(fixture.carrier(&track).await.2.canonical_inputs, committed);
    fixture.shutdown_process(&new_state).await;
}

#[tokio::test]
async fn canonical_retry_http_system_repeat_preserves_provenance() {
    super::super::dispatcher_inputs_tests::retry_system_via_http().await;
}
