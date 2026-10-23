//! Exercise the authenticated application router, with only the native provider faked.
use super::*;
use crate::db::sqlite::track_planner::PlannerSwitchRefusal as T;
use crate::planner_switch::PlannerSwitchRefusal as R;
use axum::response::Response;

fn body(p: &Pair) -> Value {
    json!({"target_assistant_card_id":p.cards[1],"expected_planner_card_id":p.cards[0],"expected_revision":0})
}
async fn decoded(response: Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}
async fn call(
    p: &mut Pair,
    cookie: &str,
    actor: &str,
    origin: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/api/tracks/{}/planner/switch", p.track))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, cookie)
        .header("X-Calm-Actor", actor);
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    let app = p.app.clone();
    let response = app.oneshot(builder.body(Body::from(body.to_string())).unwrap());
    tokio::pin!(response);
    let response = tokio::time::timeout(Duration::from_secs(10),async {
        loop { tokio::select! {
            result = &mut response => break result.unwrap(),
            call = request(&mut p.peer) => {
                assert!(matches!(call["method"].as_str(),Some("thread/unsubscribe"|"turn/interrupt"|"config/read")),"{call}");
                send(&mut p.peer,json!({"id":call["id"],"result":{}})).await;
            }
        }}
    }).await.unwrap();
    decoded(response).await
}
async fn submit(p: &mut Pair, value: Value) -> (StatusCode, Value) {
    let cookie = p.cookie.clone();
    call(p, &cookie, "user", None, value).await
}
async fn detail(p: &Pair) -> Value {
    let response = p
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/tracks/{}", p.track))
                .header(header::COOKIE, &p.cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, value) = decoded(response).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}
#[tokio::test]
async fn planner_switch_http_authenticated_user_success() {
    let mut p = Pair::new().await;
    sqlx::query("UPDATE cards SET payload=json_set(payload,'$.permission_mode','ask') WHERE id=?1")
        .bind(&p.cards[0])
        .execute(p.fixture.repo.pool())
        .await
        .unwrap();
    let history: Vec<(String, i64, String)> = sqlx::query_as("SELECT message_id,revision,payload_json FROM conversation_message_revisions ORDER BY message_id,revision")
        .fetch_all(p.fixture.repo.pool()).await.unwrap();
    let before = detail(&p).await;
    assert_eq!(before["planner_revision"], 0);
    let value = body(&p);
    let (status, receipt) = submit(&mut p, value).await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(
        receipt,
        json!({"track_id":p.track,"planner_card_id":p.cards[1],"demoted_card_id":p.cards[0],"planner_revision":1})
    );
    let after = detail(&p).await;
    assert_eq!(after["planner_revision"], 1);
    for (index, provider_key) in [(0, "assistant_provider"), (1, "planner_provider")] {
        let card = after["cards"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == p.cards[index])
            .unwrap();
        assert_eq!(card["payload"][provider_key], "codex");
    }
    let roles: Vec<(String, String)> =
        sqlx::query_as("SELECT id,role FROM cards WHERE id IN (?1,?2) ORDER BY role")
            .bind(&p.cards[0])
            .bind(&p.cards[1])
            .fetch_all(p.fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        roles,
        vec![
            (p.cards[0].clone(), "assistant".into()),
            (p.cards[1].clone(), "planner".into())
        ]
    );
    let target = p.fixture.repo.card_get(&p.cards[1]).await.unwrap().unwrap();
    assert_eq!(target.payload["permission_mode"], "ask");
    let after_history: Vec<(String, i64, String)> = sqlx::query_as("SELECT message_id,revision,payload_json FROM conversation_message_revisions ORDER BY message_id,revision")
        .fetch_all(p.fixture.repo.pool()).await.unwrap();
    assert_eq!(history, after_history);
    let sent = post(
        &p.app,
        &p.cookie,
        &format!("/api/cards/{}/planner/input", p.cards[1]),
        "next-http",
        json!({"text":"New Planner HTTP turn"}),
    )
    .await;
    assert_ne!(sent["worker_session_id"], p.workers[1]);
    complete(&p.state, &mut p.peer, "http-new-planner", "http-new-turn").await;
    p.fixture
        .wait_phase(&p.track, HarnessPhaseTag::TurnCompleted)
        .await;
    p.close().await;
}
#[tokio::test]
async fn planner_switch_http_nonhuman_actor_refused() {
    let mut p = Pair::new().await;
    let (cookie, value) = (p.cookie.clone(), body(&p));
    let (status, error) = call(&mut p, &cookie, "ai:codex", None, value).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error["code"], "planner_user_required");
    assert_eq!(error["card_id"], p.cards[0]);
    p.unchanged().await;
    p.close().await;
}
#[tokio::test]
async fn planner_switch_http_session_required() {
    let mut p = Pair::new().await;
    let forged = format!("{}=forged", crate::auth::SESSION_COOKIE);
    for cookie in ["", forged.as_str()] {
        let value = body(&p);
        let (status, error) = call(&mut p, cookie, "user", None, value).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{error}");
    }
    let response = p
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/tracks/{}/planner/switch", p.track))
                .extension(p.human.current_principal().unwrap())
                .header(header::CONTENT_TYPE, "application/json")
                .header("X-Calm-Actor", "user")
                .body(Body::from(body(&p).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = p
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/logout")
                .header(header::COOKIE, &p.cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value = body(&p);
    let (status, _) = submit(&mut p, value).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    p.unchanged().await;
    p.close().await;
}
#[tokio::test]
async fn planner_switch_http_cross_origin_refused() {
    let mut p = Pair::new().await;
    let (cookie, value) = (p.cookie.clone(), body(&p));
    let (status, _) = call(&mut p, &cookie, "user", Some("https://evil.example"), value).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    p.unchanged().await;
    p.close().await;
}
#[tokio::test]
async fn planner_switch_http_access_denied() {
    let mut p = Pair::new().await;
    sqlx::query("DELETE FROM area_members WHERE area_id=(SELECT area_id FROM tracks WHERE id=?1)")
        .bind(&p.track)
        .execute(p.fixture.repo.pool())
        .await
        .unwrap();
    let value = body(&p);
    let (status, error) = submit(&mut p, value).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{error}");
    p.unchanged().await;
    p.close().await;
}
#[tokio::test]
async fn planner_switch_http_stale_revision_refused() {
    let mut p = Pair::new().await;
    let mut value = body(&p);
    value["expected_revision"] = json!(1);
    let (status, error) = submit(&mut p, value).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["code"], "planner_changed");
    assert_eq!(detail(&p).await["planner_revision"], 0);
    p.unchanged().await;
    p.close().await;
}
#[tokio::test]
async fn planner_switch_http_invalid_body() {
    let mut p = Pair::new().await;
    for key in [
        "target_assistant_card_id",
        "expected_planner_card_id",
        "expected_revision",
    ] {
        let mut value = body(&p);
        value.as_object_mut().unwrap().remove(key);
        assert_eq!(
            submit(&mut p, value).await.0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        let mut value = body(&p);
        value[key] = json!([]);
        assert_eq!(
            submit(&mut p, value).await.0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    for (key, value) in [
        ("expected_revision", json!(-1)),
        ("target_assistant_card_id", json!(" ")),
        ("expected_planner_card_id", json!("")),
    ] {
        let mut request = body(&p);
        request[key] = value;
        assert_eq!(submit(&mut p, request).await.0, StatusCode::BAD_REQUEST);
    }
    p.unchanged().await;
    p.close().await;
}
#[tokio::test]
async fn planner_switch_http_failed_passthrough() {
    let mut p = Pair::new().await;
    sqlx::query("ALTER TABLE operations RENAME TO unavailable_operations")
        .execute(p.fixture.repo.pool())
        .await
        .unwrap();
    let value = body(&p);
    let (status, error) = submit(&mut p, value).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{error}");
    assert_eq!(error["code"], "db_error");
    assert!(error.get("card_id").is_none());
    sqlx::query("ALTER TABLE unavailable_operations RENAME TO operations")
        .execute(p.fixture.repo.pool())
        .await
        .unwrap();
    p.unchanged().await;
    p.close().await;
}
#[tokio::test]
async fn planner_switch_http_typed_refusal_mapping() {
    for (reason, code, status) in [
        (R::UserRequired, "planner_user_required", 403),
        (R::Truth(T::TargetInvalid), "planner_target_invalid", 400),
        (R::Truth(T::Changed), "planner_changed", 409),
        (
            R::Truth(T::HistoricalStorage),
            "planner_historical_storage",
            409,
        ),
        (
            R::Truth(T::BindingUnsupported),
            "planner_binding_unsupported",
            409,
        ),
        (
            R::Truth(T::PendingMessages),
            "planner_pending_messages",
            409,
        ),
        (
            R::Truth(T::PendingSubmission),
            "planner_pending_submission",
            409,
        ),
        (R::Truth(T::TurnInFlight), "planner_turn_in_flight", 409),
        (
            R::Truth(T::SessionUnavailable),
            "planner_session_unavailable",
            409,
        ),
        (R::PendingObservations, "planner_pending_observations", 409),
        (R::OpenAsk, "planner_open_ask", 409),
    ] {
        let (actual, body) = decoded(crate::routes::planner_switch::refusal_response(
            PlannerSwitchError::Refused {
                reason,
                card_id: "blocking-card".into(),
            },
        ))
        .await;
        assert_eq!(actual.as_u16(), status, "{code}");
        assert_eq!(body["code"], code);
        assert_eq!(body["card_id"], "blocking-card");
        assert!(!body["error"].as_str().unwrap().is_empty());
        assert_eq!(body.as_object().unwrap().len(), 3);
    }
}

#[tokio::test]
async fn planner_switch_http_open_wake_refused_before_revoke() {
    let mut p = Pair::new().await;
    // Persist through the actual Planner ask and plural event owner.
    let mut tx = crate::db::sqlite::begin_immediate_tx(p.fixture.repo.db())
        .await
        .unwrap();
    let (scope, event) = crate::ask::ask_requested_tx(
        &mut tx,
        &p.cards[0].clone().into(),
        vec![crate::event::AskQuestion {
            title: "Open wake".into(),
            options: vec!["Yes".into()],
        }],
    )
    .await
    .unwrap();
    crate::db::sqlite::append_decision_event_in_tx(
        &mut tx,
        &crate::ids::ActorId::AiPlannerSession(p.workers[0].clone().into()),
        &scope,
        None,
        &event,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let value = body(&p);
    let (status, error) = submit(&mut p, value).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["code"], "planner_open_ask");
    assert_eq!(error["card_id"], p.cards[0]);
    p.unchanged().await;
    p.close().await;
}

#[tokio::test]
async fn planner_switch_http_pending_event_refused() {
    let mut p = Pair::new().await;
    let area = p
        .fixture
        .repo
        .track_get(&p.track)
        .await
        .unwrap()
        .unwrap()
        .area_id;
    p.fixture
        .repo
        .log_pure_event(
            crate::ids::ActorId::User,
            crate::event::EventScope::Track {
                track: p.track.clone().into(),
                area,
            },
            None,
            &p.state.events,
            &p.state.card_role_cache,
            &p.state.track_area_cache,
            crate::event::Event::TrackReportEdited {
                track_id: p.track.clone().into(),
                card_id: p.cards[0].clone().into(),
                author: crate::event::EditAuthor::User,
                author_plugin_id: None,
                edit_id: "pending".into(),
                summary_before: String::new(),
                summary_after: String::new(),
                body_before: String::new(),
                body_after: "pending report".into(),
                agent_message: None,
            },
        )
        .await
        .unwrap();
    let value = body(&p);
    let (status, error) = submit(&mut p, value).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["code"], "planner_pending_observations");
    assert_eq!(error["card_id"], p.cards[0]);
    p.unchanged().await;
    p.close().await;
}

#[tokio::test]
async fn planner_switch_http_pending_refused() {
    use crate::db::sqlite::conversation::{accept_pending_human_tx, current_input_carrier_tx};
    for (kind, code) in [
        ("message", "planner_pending_messages"),
        ("submission", "planner_pending_submission"),
        ("turn", "planner_turn_in_flight"),
    ] {
        let mut p = Pair::new().await;
        let mut tx = crate::db::sqlite::begin_immediate_tx(p.fixture.repo.db())
            .await
            .unwrap();
        match kind {
            "message" => {
                let mut carrier = current_input_carrier_tx(&mut tx, &p.cards[1], &p.workers[1])
                    .await
                    .unwrap();
                let principal = p.human.current_principal().unwrap();
                accept_pending_human_tx(
                    &mut tx,
                    &mut carrier,
                    &principal.user_id,
                    calm_types::conversation::HumanPayload {
                        text: "pending".into(),
                        bound_attachments: vec![],
                    },
                )
                .await
                .unwrap();
            }
            "submission" => {
                sqlx::query(concat!(
                    "INSERT INTO acp_submissions(worker_session_id,client_id,turn_id,thread_id,native_session_id,",
                    "input_json,state,created_at_ms) VALUES(?1,'client','turn','thread','native','{}','unknown',0)"
                ))
                    .bind(&p.workers[1]).execute(&mut **tx).await.unwrap();
            }
            "turn" => {
                sqlx::query("UPDATE worker_sessions SET active_turn_id='pending-turn' WHERE id=?1")
                    .bind(&p.workers[1])
                    .execute(&mut **tx)
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        tx.commit().await.unwrap();
        let value = body(&p);
        let (status, error) = submit(&mut p, value).await;
        assert_eq!(status, StatusCode::CONFLICT, "{kind}: {error}");
        assert_eq!(error["code"], code, "{kind}");
        assert_eq!(error["card_id"], p.cards[1]);
        p.unchanged().await;
        p.close().await;
    }
}
