//! #2774 — `POST /api/cards/{id}/planner/conversations`: a person starts a new, blank Planner
//! conversation on the same card. Admission refuses anything in flight and changes nothing when it
//! does; a key's replay answers without acting on the new conversation; concurrent requests make
//! one cutover; the transcript reads by conversation. The cutover itself is pinned in
//! `planner_new_conversation_cutover`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use axum::Extension;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::auth::Principal;
use calm_server::db::prelude::*;
use calm_server::harness::{HarnessSnapshot, HarnessState, QueueEntry, SendKey};
use calm_server::model::new_id;
use calm_server::session_projection_repo::WorkerSessionProjection;
use calm_server::test_seams::{PausePoint, install_pause_for_test};
use calm_types::organization::LEGACY_OWNER_USER_ID;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::sync::Notify;
use tower::ServiceExt;

use crate::planner_first_start::{Boot, post_input};
use crate::planner_repoint_restart_lock::{joined, wait_until};
use crate::planner_restart::{get, ordinary_track};

/// Where a start has committed its cutover and not yet minted its thread; keyed by card id.
pub(crate) const START_COMMITTED: &str = "conversation-start-prepared";

pub(crate) fn person(boot: &Boot, user: &str) -> axum::Router {
    calm_server::routes::router()
        .layer(Extension(Principal {
            user_id: user.into(),
            display_name: user.into(),
            role: "owner".into(),
            session_id: "new-conversation".into(),
        }))
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(boot.state.clone())
}

pub(crate) async fn send(
    app: axum::Router,
    method: &str,
    uri: String,
    key: Option<&str>,
    actor: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    if let Some(actor) = actor {
        request = request.header("X-Calm-Actor", actor);
    }
    let response = app
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Every row of every table, as text, so a refusal can be shown to have changed nothing.
pub(crate) async fn image(boot: &Boot, except: &[&str]) -> BTreeMap<String, Vec<String>> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' \
           AND name NOT LIKE '_sqlx%' ORDER BY name",
    )
    .fetch_all(boot.repo.pool())
    .await
    .unwrap();
    let mut out = BTreeMap::new();
    for table in tables.into_iter().filter(|t| !except.contains(&t.as_str())) {
        let columns: Vec<String> =
            sqlx::query_scalar(&format!("SELECT name FROM pragma_table_info('{table}')"))
                .fetch_all(boot.repo.pool())
                .await
                .unwrap();
        let row = columns
            .iter()
            .map(|c| format!("quote(\"{c}\")"))
            .collect::<Vec<_>>()
            .join("||','||");
        let mut rows: Vec<String> = sqlx::query_scalar(&format!("SELECT {row} FROM \"{table}\""))
            .fetch_all(boot.repo.pool())
            .await
            .unwrap();
        rows.sort();
        out.insert(table, rows);
    }
    out
}

impl Boot {
    pub(crate) async fn new_conversation(&self, key: &str) -> (StatusCode, Value) {
        send(
            person(self, LEGACY_OWNER_USER_ID),
            "POST",
            self.card_uri("planner/conversations"),
            Some(key),
            None,
        )
        .await
    }

    pub(crate) async fn floors(&self) -> i64 {
        self.scalar("SELECT COUNT(*) FROM planner_conversation_starts WHERE card_id = ?1")
            .await
    }

    /// Stop the live owner, leaving the row as its last persisted state.
    pub(crate) async fn without_owner(&self, session: &str) {
        self.shutdown_session(session).await;
    }

    /// Overwrite the persisted state of `session`, which has no live owner.
    pub(crate) async fn persist(&self, session: &str, state: &str, snapshot: Value) {
        sqlx::query("UPDATE worker_sessions SET state = ?2, handle_state_json = ?3 WHERE id = ?1")
            .bind(session)
            .bind(state)
            .bind(snapshot.to_string())
            .execute(self.repo.pool())
            .await
            .unwrap();
    }

    pub(crate) async fn row_snapshot(&self, session: &str) -> HarnessSnapshot {
        let state: String =
            sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id = ?1")
                .bind(session)
                .fetch_one(self.repo.pool())
                .await
                .unwrap();
        HarnessSnapshot::from_value_strict(serde_json::from_str(&state).unwrap())
    }

    /// Refused with `status`, and the whole data image is as it was.
    async fn refused(&self, status: StatusCode, what: &str) -> Value {
        let before = image(self, &[]).await;
        let (actual, body) = self.new_conversation(&new_id()).await;
        assert_eq!(actual, status, "{what}: body={body}");
        assert_eq!(image(self, &[]).await, before, "{what}: nothing changed");
        assert_eq!(self.floors().await, 0, "{what}");
        body
    }
}

fn pause(card: &str) -> (Arc<Notify>, Arc<Notify>) {
    pause_at(START_COMMITTED, card)
}

pub(crate) fn pause_at(point: &str, card: &str) -> (Arc<Notify>, Arc<Notify>) {
    let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    install_pause_for_test(
        point,
        card,
        PausePoint {
            entered: entered.clone(),
            release: release.clone(),
        },
    );
    (entered, release)
}

#[tokio::test]
async fn planner_new_conversation_refuses_a_non_user_wrong_access_and_a_missing_key() {
    use calm_server::db::sqlite::area_membership::application_user_register;
    let boot = ordinary_track().await;
    let old = boot.active().await;
    let before = image(&boot, &[]).await;
    let uri = boot.card_uri("planner/conversations");
    let (status, body) = send(
        person(&boot, LEGACY_OWNER_USER_ID),
        "POST",
        uri.clone(),
        Some("k"),
        Some("ai:codex"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a non-user actor: {body}");
    assert!(
        boot.state.harness.get(&old.id).is_some(),
        "refused before admission: the live owner was not quiesced"
    );
    let mut tx = boot.repo.pool().begin().await.unwrap();
    application_user_register(&mut tx, "alice", "Alice")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let after_register = image(&boot, &[]).await;
    let (status, body) = send(person(&boot, "alice"), "POST", uri.clone(), Some("k"), None).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "no access to the Track: {body}"
    );
    assert!(boot.state.harness.get(&old.id).is_some());
    let (status, body) = send(person(&boot, LEGACY_OWNER_USER_ID), "POST", uri, None, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "no key: {body}");
    assert_eq!(image(&boot, &[]).await, after_register);
    assert_eq!(
        before.get("worker_sessions"),
        after_register.get("worker_sessions")
    );
    assert_eq!(boot.floors().await, 0);
    boot.shutdown().await;
}

/// Access is rechecked where the cutover commits: a member who loses the Track's Area while the
/// request waits on the owner's quiesce is refused, and the cutover writes nothing.
#[tokio::test]
async fn planner_new_conversation_refuses_a_member_who_lost_access_while_it_waited() {
    use calm_server::db::sqlite::area_membership::application_user_register;
    use calm_server::test_seams::PLANNER_HIBERNATION_QUIESCE;
    let boot = ordinary_track().await;
    let old = boot.active().await;
    let card = boot
        .repo
        .card_get(&boot.planner_card_id)
        .await
        .unwrap()
        .unwrap();
    let area = boot
        .repo
        .track_get(card.track_id.as_str())
        .await
        .unwrap()
        .unwrap()
        .area_id;
    let mut tx = boot.repo.pool().begin().await.unwrap();
    application_user_register(&mut tx, "alice", "Alice")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let membership = |sql: &'static str| {
        let (pool, area) = (boot.repo.pool().clone(), area.to_string());
        async move { sqlx::query(sql).bind(area).execute(&pool).await.unwrap() }
    };
    membership("INSERT INTO area_members VALUES(?1,'alice','member',0)").await;
    let sessions = async || -> Vec<(String, String)> {
        sqlx::query_as("SELECT id, state FROM worker_sessions ORDER BY id")
            .fetch_all(boot.repo.pool())
            .await
            .unwrap()
    };

    let (entered, release) = pause_at(PLANNER_HIBERNATION_QUIESCE, &boot.planner_card_id);
    let request = {
        let (app, uri) = (
            person(&boot, "alice"),
            boot.card_uri("planner/conversations"),
        );
        tokio::spawn(async move { send(app, "POST", uri, Some("revoked"), None).await })
    };
    entered.notified().await;
    membership("DELETE FROM area_members WHERE area_id = ?1 AND user_id = 'alice'").await;
    // The quiesce persists the owner's snapshot; everything else is the cutover's to write.
    let before = image(&boot, &["worker_sessions", "operations"]).await;
    let rows = sessions().await;
    release.notify_one();
    let (status, body) = joined(request, "the revoked request").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");
    assert_eq!(
        image(&boot, &["worker_sessions", "operations"]).await,
        before,
        "nothing written"
    );
    assert_eq!(sessions().await, rows, "no session retired or started");
    assert_eq!(boot.current_session().await, Some(old.id.clone()));
    assert_eq!(boot.floors().await, 0);
    boot.shutdown().await;
}

/// A reply in flight and a compaction are refused on the owner's own loop; the owner keeps
/// running and accepts input afterwards.
#[tokio::test]
async fn planner_new_conversation_refuses_an_active_turn_or_compaction_and_keeps_the_owner() {
    let boot = ordinary_track().await;
    let old = boot.active().await;
    let harness = boot.state.harness.get(&old.id).unwrap();
    for state in [
        HarnessState::turn_running("running".into(), Instant::now()),
        HarnessState::Compacting {
            since: Instant::now(),
        },
    ] {
        harness.set_state_for_test(state.clone()).await;
        boot.refused(StatusCode::CONFLICT, &format!("{state:?}"))
            .await;
        assert_eq!(
            harness.state_for_test().await,
            state,
            "the owner is untouched"
        );
        assert!(boot.state.harness.get(&old.id).is_some());
    }
    harness.set_state_for_test(HarnessState::Idle).await;
    harness
        .observe_user_message_durable("still live".into(), vec![], SendKey::unique_for_test())
        .await
        .expect("the owner still accepts input");
    boot.shutdown().await;
}

/// A person's queued sentence is never carried into, or dropped by, a blank conversation.
#[tokio::test]
async fn planner_new_conversation_refuses_a_queued_user_message() {
    let boot = ordinary_track().await;
    let old = boot.active().await;
    boot.without_owner(&old.id).await;
    let mut snapshot = boot.row_snapshot(&old.id).await;
    snapshot.set_pending_entries(vec![QueueEntry::user_message(
        "typed while it slept".into(),
        None,
        Vec::new(),
    )]);
    boot.persist(&old.id, "idle", serde_json::to_value(&snapshot).unwrap())
        .await;
    boot.refused(StatusCode::CONFLICT, "a queued user message")
        .await;
    boot.shutdown().await;
}

/// The provider waits on a `hold` question of the current session.
#[tokio::test]
async fn planner_new_conversation_refuses_an_open_hold_ask() {
    use calm_server::event::{AskDelivery, AskQuestion, Event, EventScope};
    use calm_server::ids::{ActorId, TrackId};
    use calm_types::worker::WorkerSessionId;
    let boot = ordinary_track().await;
    let old = boot.active().await;
    let card = boot
        .repo
        .card_get(&boot.planner_card_id)
        .await
        .unwrap()
        .unwrap();
    let track = boot
        .repo
        .track_get(card.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    boot.repo
        .log_pure_event(
            ActorId::AiPlannerSession(WorkerSessionId::from(old.id.as_str())),
            EventScope::Track {
                track: TrackId::from(track.id.to_string()),
                area: track.area_id.clone(),
            },
            None,
            &boot.state.events,
            &boot.state.card_role_cache,
            &boot.state.track_area_cache,
            Event::AskRequested {
                track_id: track.id.clone(),
                questions: vec![AskQuestion {
                    title: "Allow?".into(),
                    options: vec!["Yes".into(), "No".into()],
                }],
                delivery: AskDelivery::Hold,
                action: None,
                source_item_id: None,
            },
        )
        .await
        .unwrap();
    boot.refused(StatusCode::CONFLICT, "an open hold ask").await;
    assert!(boot.state.harness.get(&old.id).is_some());
    boot.shutdown().await;
}

/// Activity nothing can confirm: an unparseable snapshot or an active row without an owner, a
/// provider submission whose outcome is unknown, and a failed session.
#[tokio::test]
async fn planner_new_conversation_refuses_unknown_activity() {
    let boot = ordinary_track().await;
    let old = boot.active().await;
    boot.without_owner(&old.id).await;
    let idle = serde_json::to_value(boot.row_snapshot(&old.id).await).unwrap();
    boot.persist(
        &old.id,
        "idle",
        json!({"mode": "harness", "schema_version": 999}),
    )
    .await;
    boot.refused(StatusCode::CONFLICT, "an unparseable snapshot")
        .await;
    boot.persist(&old.id, "running", idle.clone()).await;
    boot.refused(StatusCode::CONFLICT, "an active row without an owner")
        .await;
    boot.persist(&old.id, "idle", idle.clone()).await;
    sqlx::query(
        "INSERT INTO acp_submissions (worker_session_id, client_id, turn_id, thread_id, \
           native_session_id, input_json, state, created_at_ms) \
         VALUES (?1, 'c', 't', 'th', 'n', '[]', 'sending', 0)",
    )
    .bind(&old.id)
    .execute(boot.repo.pool())
    .await
    .unwrap();
    boot.refused(StatusCode::CONFLICT, "a submission in `sending`")
        .await;
    sqlx::query("DELETE FROM acp_submissions")
        .execute(boot.repo.pool())
        .await
        .unwrap();
    boot.persist(&old.id, "failed", idle).await;
    let body = boot.refused(StatusCode::CONFLICT, "a failed session").await;
    assert!(
        body["error"].as_str().unwrap().contains("restart"),
        "a failed session is recovered with restart first: {body}"
    );
    boot.shutdown().await;
}

/// A start that a cancelled request left behind is not settled yet.
#[tokio::test]
async fn planner_new_conversation_refuses_an_unsettled_start() {
    let boot = ordinary_track().await;
    let (entered, release) = pause(&boot.planner_card_id);
    let restart = tokio::spawn({
        let app = boot.app.clone();
        let uri = boot.card_uri("planner/restart");
        async move { send(app, "POST", uri, None, None).await }
    });
    entered.notified().await;
    restart.abort();
    let _ = restart.await;
    wait_until("the cancelled restart releases the card", || {
        boot.state
            .planner_recovery_lock_handles_for_test(&boot.planner_card_id)
            == 0
    })
    .await;
    let (status, body) = boot.new_conversation(&new_id()).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(boot.floors().await, 0);
    release.notify_one();
    boot.shutdown().await;
}

/// A replay after success answers the same body and acts on nothing: no second floor, session or
/// quiesce of the new conversation.
#[tokio::test]
async fn planner_new_conversation_replay_returns_the_same_answer_and_starts_nothing() {
    let boot = ordinary_track().await;
    let old = boot.active().await;
    let (status, first) = boot.new_conversation("same-key").await;
    assert_eq!(status, StatusCode::OK, "body={first}");
    let new = boot.active().await;
    assert_ne!(new.id, old.id);
    assert_eq!(first["card_id"], json!(boot.planner_card_id));
    assert_eq!(first["conversation"], json!(1));
    assert_eq!(first["new_thread_id"], json!(new.thread_id));
    let starts = boot.start_ops().await;

    let (status, replay) = boot.new_conversation("same-key").await;
    assert_eq!(status, StatusCode::OK, "body={replay}");
    assert_eq!(replay, first);
    assert_eq!(boot.floors().await, 1, "no second floor");
    assert_eq!(boot.start_ops().await, starts, "no second start");
    assert_eq!(boot.active().await.id, new.id, "no second session");
    let live = boot
        .state
        .harness
        .get(&new.id)
        .expect("the new conversation was not quiesced by the replay");
    live.observe_user_message_durable(
        "after the replay".into(),
        vec![],
        SendKey::unique_for_test(),
    )
    .await
    .expect("and still accepts input");
    boot.shutdown_session(&old.id).await;
    boot.shutdown().await;
}

/// A key already bound to another request is refused before anything is admitted.
#[tokio::test]
async fn planner_new_conversation_refuses_a_key_bound_to_other_content() {
    use calm_server::operation::OperationKey;
    let boot = ordinary_track().await;
    // A restart someone stored under this card's new-conversation key, with its own fingerprint.
    let mut restart: Value = serde_json::from_str(
        &sqlx::query_scalar::<_, String>(
            // The Track create's own start, the only one so far.
            "SELECT payload_json FROM operations WHERE kind = 'planner-harness-start'",
        )
        .fetch_one(boot.repo.pool())
        .await
        .unwrap(),
    )
    .unwrap();
    restart["force_new_thread"] = json!(true);
    let op = boot
        .state
        .operation_runtime
        .submit(
            "planner-harness-start",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(format!(
                    "planner-new-conversation:{}:reused",
                    boot.planner_card_id
                )),
                payload_hash: "another request".into(),
            },
            restart,
        )
        .await
        .unwrap();
    boot.state.operation_runtime.wait(&op).await.unwrap();
    let current = boot.active().await;
    let starts = boot.start_ops().await;
    let (status, body) = boot.new_conversation("reused").await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(body["code"], json!("idempotency_key_reused"), "body={body}");
    assert_eq!(boot.floors().await, 0);
    assert_eq!(boot.start_ops().await, starts);
    assert_eq!(boot.active().await.id, current.id);
    assert!(
        boot.state.harness.get(&current.id).is_some(),
        "nothing was quiesced"
    );
    boot.shutdown().await;
}

/// A replay while the first request is in flight joins it: one start, one floor, one body.
#[tokio::test]
async fn planner_new_conversation_replay_in_flight_joins_the_first_request() {
    let boot = ordinary_track().await;
    let old = boot.active().await;
    let (entered, release) = pause(&boot.planner_card_id);
    let first = {
        let app = person(&boot, LEGACY_OWNER_USER_ID);
        let uri = boot.card_uri("planner/conversations");
        tokio::spawn(async move { send(app, "POST", uri, Some("joined"), None).await })
    };
    entered.notified().await;
    let second = {
        let app = person(&boot, LEGACY_OWNER_USER_ID);
        let uri = boot.card_uri("planner/conversations");
        tokio::spawn(async move { send(app, "POST", uri, Some("joined"), None).await })
    };
    // It finds the stored start under its key and waits for that start's outcome.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        !second.is_finished(),
        "the replay waits for the first request"
    );
    release.notify_one();
    let (status, a) = joined(first, "the first request").await;
    assert_eq!(status, StatusCode::OK, "body={a}");
    let (status, b) = joined(second, "the replay").await;
    assert_eq!(status, StatusCode::OK, "body={b}");
    assert_eq!(a, b);
    assert_eq!(boot.floors().await, 1);
    assert_eq!(boot.start_ops().await, 2, "the create's start and this one");
    boot.shutdown_session(&old.id).await;
    boot.shutdown().await;
}

/// Two requests under different keys: one cutover; the one made for the replaced session is 409.
#[tokio::test]
async fn planner_new_conversation_concurrent_requests_give_one_cutover_and_one_conflict() {
    let boot = ordinary_track().await;
    let old = boot.active().await;
    // Hold the first request inside its quiesce, before its cutover commits.
    let (entered, release) = pause_at(
        calm_server::test_seams::PLANNER_HIBERNATION_QUIESCE,
        &boot.planner_card_id,
    );
    let first = {
        let app = person(&boot, LEGACY_OWNER_USER_ID);
        let uri = boot.card_uri("planner/conversations");
        tokio::spawn(async move { send(app, "POST", uri, Some("first"), None).await })
    };
    entered.notified().await;
    let held = boot
        .state
        .planner_recovery_lock_handles_for_test(&boot.planner_card_id);
    let second = {
        let app = person(&boot, LEGACY_OWNER_USER_ID);
        let uri = boot.card_uri("planner/conversations");
        tokio::spawn(async move { send(app, "POST", uri, Some("second"), None).await })
    };
    wait_until("the second request waits on the card", || {
        boot.state
            .planner_recovery_lock_handles_for_test(&boot.planner_card_id)
            > held
    })
    .await;
    release.notify_one();
    let (status, body) = joined(first, "the first request").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let (status, body) = joined(second, "the second request").await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(boot.floors().await, 1);
    assert_eq!(boot.active_session_rows().await, 1);
    boot.shutdown_session(&old.id).await;
    boot.shutdown().await;
}

/// A send waits on the card during the cutover and lands on the new session.
#[tokio::test]
async fn planner_new_conversation_a_send_during_the_cutover_lands_on_the_new_session() {
    let boot = ordinary_track().await;
    let old = boot.active().await;
    let (entered, release) = pause(&boot.planner_card_id);
    let cutover = {
        let app = person(&boot, LEGACY_OWNER_USER_ID);
        let uri = boot.card_uri("planner/conversations");
        tokio::spawn(async move { send(app, "POST", uri, Some("cut"), None).await })
    };
    entered.notified().await;
    let held = boot
        .state
        .planner_recovery_lock_handles_for_test(&boot.planner_card_id);
    let message = {
        let (app, uri) = (boot.app.clone(), boot.input_uri());
        tokio::spawn(async move { post_input(app, &uri, "for the new one", &new_id()).await })
    };
    wait_until("the send waits on the card", || {
        boot.state
            .planner_recovery_lock_handles_for_test(&boot.planner_card_id)
            > held
    })
    .await;
    release.notify_one();
    let (status, body) = joined(cutover, "the cutover").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let (status, body) = joined(message, "the send").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let new = boot.active().await;
    assert_ne!(new.id, old.id);
    assert_eq!(body["worker_session_id"], json!(new.id));
    boot.wait_delivered("for the new one").await;
    boot.shutdown_session(&old.id).await;
    boot.shutdown().await;
}

/// A reply `session` already recorded, through the transcript writer.
pub(crate) async fn record_a_reply(boot: &Boot, session: &WorkerSessionProjection) {
    let card = boot
        .repo
        .card_get(&boot.planner_card_id)
        .await
        .unwrap()
        .unwrap();
    boot.repo
        .harness_item_insert(
            &session.id,
            card.id.as_str(),
            card.track_id.as_str(),
            session.thread_id.as_deref().unwrap(),
            Some("old-turn"),
            Some("old-answer"),
            Some("agentMessage"),
            "item/completed",
            &json!({"item": {"id": "old-answer", "type": "agentMessage", "text": "hi"}})
                .to_string(),
            None,
        )
        .await
        .unwrap()
        .expect("the session is current");
}

/// Reads default to the current conversation; `conversation=k` reads an earlier one, and only an
/// earlier conversation with rows is listed.
#[tokio::test]
async fn planner_new_conversation_history_reads_by_conversation() {
    let boot = ordinary_track().await;
    let old = boot.active().await;
    record_a_reply(&boot, &old).await;
    let items = |query: &'static str| {
        let (app, uri) = (
            boot.app.clone(),
            boot.card_uri(&format!("harness/items{query}")),
        );
        async move { get(app, uri).await }
    };
    let (_, before) = items("").await;
    assert_eq!(
        before.as_array().map(Vec::len),
        Some(1),
        "premise: {before}"
    );
    let (status, body) = boot.new_conversation(&new_id()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    let (status, current) = items("").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(current, json!([]), "the new conversation is blank");
    let (status, earlier) = items("?conversation=0").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(earlier, before, "the old rows are read-only history");
    let (status, _) = items("?conversation=2").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, list) = get(boot.app.clone(), boot.card_uri("planner/conversations")).await;
    assert_eq!(status, StatusCode::OK, "body={list}");
    assert_eq!(list.as_array().map(Vec::len), Some(1), "body={list}");
    assert_eq!(list[0]["ordinal"], json!(0));
    boot.shutdown_session(&old.id).await;
    boot.shutdown().await;
}
