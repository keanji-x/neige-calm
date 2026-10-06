//! #2192 — `POST /api/cards/{id}/planner/restart` starts a fresh session on a harness card and
//! keeps its history: a new thread, the transcript untouched, under the card's start fence. A
//! fresh start (restart and reset alike) also carries the queued, unsent messages of a session
//! that failed mid-conversation, exactly once.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::harness::run_loop::{
    ANY_RUNTIME, PlannerHarnessDrainRaceHook, install_planner_harness_drain_race_hook_for_test,
};
use calm_server::harness::{
    HarnessSnapshot, HarnessState, Observation, SendKey, is_harness_snapshot_value,
};
use calm_server::model::{NewArea, new_id};
use calm_server::planner_attachments::bind::BoundAttachment;
use calm_types::planner_attachment::AttachmentId;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::sync::Notify;
use tower::ServiceExt;

use crate::planner_first_start::{AppServer, Boot, app_state, post_input, router};
use crate::planner_repoint_restart_lock::{joined, request, wait_until};

/// An ordinary Track created through `POST /api/tracks`, whose create started its Planner.
async fn ordinary_track() -> Boot {
    let (tmp, repo, state) = app_state(AppServer::Running).await;
    let area = repo
        .area_create(NewArea {
            name: "restart".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let app = router(&state);
    let (status, track) = request(
        app.clone(),
        "POST",
        "/api/tracks".into(),
        json!({
            "planner_provider": "codex",
            "area_id": area.id.as_str(),
            "title": "restarting",
            "theme": {"fg": [216, 219, 226], "bg": [15, 20, 24]},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={track}");
    let planner_card_id: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id = ?1 AND role = 'planner'")
            .bind(track["id"].as_str().unwrap())
            .fetch_one(repo.pool())
            .await
            .unwrap();
    let boot = Boot {
        app,
        state,
        repo,
        planner_card_id,
        _tmp: tmp,
    };
    assert_eq!(boot.start_ops().await, 1, "premise: the create's own start");
    assert_eq!(boot.active_session_rows().await, 1, "premise: it is live");
    boot
}

async fn get(app: axum::Router, uri: String) -> (StatusCode, Value) {
    let response = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

impl Boot {
    fn card_uri(&self, tail: &str) -> String {
        format!("/api/cards/{}/{tail}", self.planner_card_id)
    }

    async fn fresh_start(&self, route: &str) -> (StatusCode, Value) {
        request(
            self.app.clone(),
            "POST",
            self.card_uri(&format!("planner/{route}")),
            Value::Null,
        )
        .await
    }

    async fn active(&self) -> calm_server::session_projection_repo::WorkerSessionProjection {
        self.repo
            .session_projection_active_for_card(&self.planner_card_id)
            .await
            .unwrap()
            .expect("the card has an active session")
    }

    /// The transcript as `GET …/harness/items` returns it.
    async fn transcript(&self) -> Vec<Value> {
        let (status, rows) = get(self.app.clone(), self.card_uri("harness/items")).await;
        assert_eq!(status, StatusCode::OK, "body={rows}");
        rows.as_array().expect("an array of rows").clone()
    }

    async fn transcript_cleared_events(&self) -> i64 {
        self.scalar(
            "SELECT COUNT(*) FROM events WHERE kind = 'harness.transcript.cleared' \
               AND scope_card = ?1",
        )
        .await
    }

    /// Every session row of the card that still owes its queue (unstamped) and holds `text` as a
    /// queued user message, with how many copies it holds, read from the rows themselves.
    async fn queued_copies(&self, text: &str) -> Vec<(String, usize)> {
        let rows: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT id, handle_state_json FROM worker_sessions WHERE card_id = ?1 \
               AND queue_harvested_at_ms IS NULL ORDER BY created_at_ms, id",
        )
        .bind(&self.planner_card_id)
        .fetch_all(self.repo.pool())
        .await
        .unwrap();
        rows.into_iter()
            .filter_map(|(id, state)| {
                let value: Value = serde_json::from_str(&state?).ok()?;
                let snapshot = is_harness_snapshot_value(&value)
                    .then(|| HarnessSnapshot::from_value_strict(value))?;
                let copies = snapshot
                    .pending_entries()
                    .iter()
                    .filter(|entry| {
                        entry.is_user_authored()
                            && entry.observation()
                                == Observation::UserMessage {
                                    text: text.to_string(),
                                }
                    })
                    .count();
                (copies > 0).then_some((id, copies))
            })
            .collect()
    }

    /// Each `turn/start` or steer that carried `text` to the model, and whether it carried the image
    /// at `image` with it.
    fn deliveries(&self, text: &str, image: &str) -> Vec<bool> {
        let daemon = &self.state.shared_codex_appserver;
        let turns = daemon
            .started_turns_for_test()
            .into_iter()
            .map(|(_, input)| format!("{input:?}"));
        let steers = daemon
            .steered_turns_for_test()
            .into_iter()
            .map(|steer| format!("{steer:?}"));
        turns
            .chain(steers)
            .filter(|input| input.contains(text))
            .map(|input| input.contains(&format!("LocalImage {{ path: {image:?} }}")))
            .collect()
    }

    /// Wedge the live session with `text` and an image queued behind its turn: `interrupt_timeout`
    /// is an unconfirmed Stop, `system_error` a provider error. Returns the wedged session's id; its
    /// row is now `failed` mid-conversation and its harness stays registered.
    async fn wedge_with_queued(&self, text: &str, reason: &str, image: &BoundAttachment) -> String {
        let session = self.active().await;
        let harness = self
            .state
            .harness
            .get(&session.id)
            .expect("the live session is registered");
        harness
            .set_state_for_test(HarnessState::TurnRunning {
                turn_id: "unconfirmed".into(),
                started_at: Instant::now(),
            })
            .await;
        harness
            .observe_user_message_durable(
                text.to_string(),
                vec![image.clone()],
                SendKey::unique_for_test(),
            )
            .await
            .unwrap();
        harness
            .set_state_for_test(HarnessState::Wedged {
                since: Instant::now(),
                reason: reason.into(),
            })
            .await;
        harness.persist_snapshot().await.unwrap();
        assert_eq!(
            self.scalar(&format!(
                "SELECT COUNT(*) FROM worker_sessions WHERE card_id = ?1 AND id = '{}' \
                   AND state = 'failed' AND completed_at_ms IS NULL",
                session.id
            ))
            .await,
            1,
            "premise: the wedge leaves the row failed mid-conversation"
        );
        let (status, run) = get(self.app.clone(), self.card_uri("planner/run")).await;
        assert_eq!(status, StatusCode::OK, "body={run}");
        assert_eq!(run["pending"][0]["text"], text, "premise: body={run}");
        assert_eq!(
            run["pending"][0]["attachments"].as_array().map(Vec::len),
            Some(1),
            "premise: body={run}"
        );
        session.id
    }

    async fn current_session(&self) -> Option<String> {
        sqlx::query_scalar("SELECT session_id FROM cards WHERE id = ?1")
            .bind(&self.planner_card_id)
            .fetch_one(self.repo.pool())
            .await
            .unwrap()
    }

    async fn harvested_at(&self, session_id: &str) -> Option<i64> {
        sqlx::query_scalar("SELECT queue_harvested_at_ms FROM worker_sessions WHERE id = ?1")
            .bind(session_id)
            .fetch_one(self.repo.pool())
            .await
            .unwrap()
    }

    async fn shutdown_session(&self, session_id: &str) {
        if let Some(harness) = self.state.harness.remove(session_id) {
            harness.shutdown().await.unwrap();
        }
    }
}

/// Hold the next session to reach a drain with its queue whole, so what a start put on that
/// session's row can be read before the model takes it.
fn hold_next_drain() -> Arc<Notify> {
    let release = Arc::new(Notify::new());
    install_planner_harness_drain_race_hook_for_test(
        ANY_RUNTIME,
        PlannerHarnessDrainRaceHook {
            entered: Arc::new(Notify::new()),
            release: release.clone(),
        },
    );
    release
}

async fn wait_for(what: &str, mut done: impl AsyncFnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done().await {
        assert!(Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A bound image as the upload route leaves one: a real file, addressed by id.
fn bound_image(boot: &Boot) -> BoundAttachment {
    let id = AttachmentId::parse(&format!("{}.png", uuid::Uuid::new_v4())).unwrap();
    let path = boot._tmp.path().join(id.as_str());
    std::fs::write(&path, b"queued image").unwrap();
    BoundAttachment {
        id,
        size: 12,
        path: path.to_string_lossy().into_owned(),
    }
}

/// A message queued behind a turn that then wedged is on a `failed` row, which neither the
/// inherit (active rows) nor the harvest (superseded rows) used to read. The first fresh start
/// inherits it whole, image included, and stamps the row; the second takes nothing.
async fn a_fresh_start_carries_a_wedged_sessions_queue_exactly_once(route: &str) {
    const QUEUED: &str = "the message typed before the turn wedged";
    let boot = ordinary_track().await;
    let image = bound_image(&boot);
    let wedged = boot
        .wedge_with_queued(QUEUED, "interrupt_timeout", &image)
        .await;

    let release = hold_next_drain();
    let (status, body) = boot.fresh_start(route).await;
    assert_eq!(status, StatusCode::OK, "{route}: body={body}");
    let first = boot.active().await;
    assert_ne!(first.id, wedged, "{route}: a new session");
    assert_eq!(
        boot.queued_copies(QUEUED).await,
        vec![(first.id.clone(), 1)],
        "{route}: only the new session owes the wedged session's queued message, once"
    );
    assert!(
        boot.harvested_at(&wedged).await.is_some(),
        "{route}: the wedged row is stamped, so no later start takes its queue again"
    );
    release.notify_one();
    wait_for("the carried message reaches the model", async || {
        !boot.deliveries(QUEUED, &image.path).is_empty()
            && boot.queued_copies(QUEUED).await.is_empty()
    })
    .await;
    assert_eq!(
        boot.deliveries(QUEUED, &image.path),
        vec![true],
        "{route}: the text and its image reach the model together"
    );

    let release = hold_next_drain();
    let (status, body) = boot.fresh_start(route).await;
    assert_eq!(status, StatusCode::OK, "{route}: second start: body={body}");
    let second = boot.active().await;
    assert_ne!(second.id, first.id);
    assert_eq!(
        boot.queued_copies(QUEUED).await,
        Vec::<(String, usize)>::new(),
        "{route}: the second start carries nothing: the message was already delivered"
    );
    release.notify_one();
    assert_eq!(
        boot.deliveries(QUEUED, &image.path),
        vec![true],
        "{route}: delivered exactly once"
    );

    boot.shutdown_session(&first.id).await;
    boot.shutdown().await;
}

#[tokio::test]
async fn reset_carries_a_wedged_sessions_queued_message_and_image_exactly_once() {
    a_fresh_start_carries_a_wedged_sessions_queue_exactly_once("reset").await;
}

#[tokio::test]
async fn restart_in_a_wedged_session_carries_its_queued_message_and_image_exactly_once() {
    a_fresh_start_carries_a_wedged_sessions_queue_exactly_once("restart").await;
}

/// A restart whose `thread/start` fails gives the card back to the wedged session, with the queue
/// still owed there; the next restart delivers it once, image included.
#[tokio::test]
async fn a_failed_restart_gives_the_card_back_to_its_wedged_session() {
    const QUEUED: &str = "the message a failed restart must not strand";
    let boot = ordinary_track().await;
    let image = bound_image(&boot);
    let wedged = boot
        .wedge_with_queued(QUEUED, "interrupt_timeout", &image)
        .await;

    boot.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (status, body) = boot.fresh_start("restart").await;
    assert!(
        !status.is_success(),
        "premise: the start failed: {status} body={body}"
    );

    assert_eq!(
        boot.current_session().await,
        Some(wedged.clone()),
        "the card's session is the wedged one again"
    );
    assert_eq!(
        boot.harvested_at(&wedged).await,
        None,
        "and it owes its queue again"
    );
    let (status, run) = get(boot.app.clone(), boot.card_uri("planner/run")).await;
    assert_eq!(status, StatusCode::OK, "body={run}");
    assert_eq!(run["phase"], "wedged", "body={run}");
    assert_eq!(run["pending"][0]["text"], QUEUED, "body={run}");
    assert_eq!(
        run["pending"][0]["attachments"].as_array().map(Vec::len),
        Some(1),
        "body={run}"
    );
    assert_eq!(
        boot.queued_copies(QUEUED).await,
        vec![(wedged.clone(), 1)],
        "the failed start's own row owes nothing"
    );

    let (status, body) = boot.fresh_start("restart").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    wait_for("the message reaches the model", async || {
        !boot.deliveries(QUEUED, &image.path).is_empty()
            && boot.queued_copies(QUEUED).await.is_empty()
    })
    .await;
    assert_eq!(boot.deliveries(QUEUED, &image.path), vec![true]);
    boot.shutdown().await;
}

/// A failed restart over a system-error session leaves it recoverable: a person's send resumes it,
/// and the queued message and its image go out with it.
#[tokio::test]
async fn a_failed_restart_leaves_a_system_error_session_recoverable_by_a_send() {
    const QUEUED: &str = "the message queued before the provider error";
    let boot = ordinary_track().await;
    let image = bound_image(&boot);
    let failed = boot.wedge_with_queued(QUEUED, "system_error", &image).await;

    boot.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (status, body) = boot.fresh_start("restart").await;
    assert!(
        !status.is_success(),
        "premise: the start failed: {status} body={body}"
    );
    assert_eq!(boot.current_session().await, Some(failed.clone()));

    let (status, body) =
        post_input(boot.app.clone(), &boot.input_uri(), "carry on", &new_id()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(
        body["worker_session_id"],
        json!(failed),
        "the send recovered the original session"
    );
    wait_for("the queued message reaches the model", async || {
        !boot.deliveries(QUEUED, &image.path).is_empty()
    })
    .await;
    assert_eq!(boot.deliveries(QUEUED, &image.path), vec![true]);
    boot.shutdown().await;
}

/// Restart refuses what reset refuses, before anything starts.
#[tokio::test]
async fn restart_refuses_an_unknown_card_a_non_harness_card_and_an_area_chat_planner() {
    let boot = ordinary_track().await;
    let (status, body) = request(
        boot.app.clone(),
        "POST",
        "/api/cards/no-such-card/planner/restart".into(),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body={body}");

    let other: String = sqlx::query_scalar(
        "SELECT id FROM cards WHERE track_id = (SELECT track_id FROM cards WHERE id = ?1) \
           AND id != ?1 LIMIT 1",
    )
    .bind(&boot.planner_card_id)
    .fetch_one(boot.repo.pool())
    .await
    .unwrap();
    let (status, body) = request(
        boot.app.clone(),
        "POST",
        format!("/api/cards/{other}/planner/restart"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");

    sqlx::query(
        "UPDATE tracks SET purpose = 'area-chat' \
           WHERE id = (SELECT track_id FROM cards WHERE id = ?1)",
    )
    .bind(&boot.planner_card_id)
    .execute(boot.repo.pool())
    .await
    .unwrap();
    let (status, body) = boot.fresh_start("restart").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");
    assert_eq!(boot.start_ops().await, 1, "nothing was started");
    boot.shutdown().await;
}

/// Restart mints a new thread and keeps every transcript row; nothing announces a clear.
#[tokio::test]
async fn restart_keeps_the_transcript_and_mints_a_new_thread() {
    let boot = ordinary_track().await;
    let (status, body) = post_input(
        boot.app.clone(),
        &boot.input_uri(),
        "remember me",
        &new_id(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    wait_for("the sent message reaches the transcript", async || {
        !boot.transcript().await.is_empty()
    })
    .await;
    let before = boot.transcript().await;
    let old = boot.active().await;

    let (status, body) = boot.fresh_start("restart").await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    let new = boot.active().await;
    assert_ne!(new.id, old.id, "a new session");
    assert_eq!(body["card_id"], json!(boot.planner_card_id));
    assert_eq!(body["new_thread_id"], json!(new.thread_id));
    assert_ne!(new.thread_id, old.thread_id, "on a new thread");
    let after = boot.transcript().await;
    let ids = |rows: &[Value]| rows.iter().map(|row| row["id"].clone()).collect::<Vec<_>>();
    assert_eq!(ids(&after), ids(&before), "every transcript row is kept");
    assert_eq!(
        boot.transcript_cleared_events().await,
        0,
        "a restart clears nothing, so it announces no clear"
    );
    boot.shutdown_session(&old.id).await;
    boot.shutdown().await;
}

/// The dormant answer a send gets names no reset; after a restart the same send is queued once.
#[tokio::test]
async fn a_send_refused_as_dormant_is_accepted_once_after_a_restart() {
    let boot = ordinary_track().await;
    let retired = boot.active().await;
    boot.shutdown_session(&retired.id).await;
    sqlx::query("UPDATE worker_sessions SET state = 'superseded' WHERE id = ?1")
        .bind(&retired.id)
        .execute(boot.repo.pool())
        .await
        .unwrap();

    let (status, body) =
        post_input(boot.app.clone(), &boot.input_uri(), "anyone?", &new_id()).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(
        body["code"],
        json!("planner_harness_dormant"),
        "body={body}"
    );
    let message = body["error"].as_str().unwrap();
    assert!(
        !message.contains("reset"),
        "the dormant answer offers a fresh session, not a reset: {message}"
    );
    assert!(message.contains("history is kept"), "{message}");

    let (status, body) = boot.fresh_start("restart").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let starts = boot.start_ops().await;
    let audits = boot.enqueued_audits().await;

    let (status, body) =
        post_input(boot.app.clone(), &boot.input_uri(), "anyone?", &new_id()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["worker_session_id"], json!(boot.active().await.id));
    assert_eq!(boot.enqueued_audits().await, audits + 1, "enqueued once");
    assert_eq!(boot.start_ops().await, starts, "the send started nothing");
    assert_eq!(boot.active_session_rows().await, 1);
    boot.wait_delivered("anyone?").await;
    boot.shutdown().await;
}

/// The restart is held in `thread/start` while it holds the card's lock; a send is shown queued on
/// that lock and, once the restart lands, goes to the restarted session.
#[tokio::test]
async fn a_send_during_a_restart_waits_on_the_cards_lock() {
    let boot = ordinary_track().await;
    let card_id = boot.planner_card_id.clone();
    let old = boot.active().await;
    assert_eq!(
        boot.state.planner_recovery_lock_handles_for_test(&card_id),
        0,
        "premise: nothing holds the card's lock"
    );

    let thread_start = boot
        .state
        .shared_codex_appserver
        .lock_thread_start_serial_for_test()
        .await;
    let restart = tokio::spawn(request(
        boot.app.clone(),
        "POST",
        boot.card_uri("planner/restart"),
        Value::Null,
    ));
    wait_for("the restart submits its start", async || {
        boot.start_ops().await == 2
    })
    .await;
    let held = boot.state.planner_recovery_lock_handles_for_test(&card_id);
    assert!(
        held > 0,
        "the restart submitted its start without holding the card's lock"
    );

    let send = {
        let (app, uri) = (boot.app.clone(), boot.input_uri());
        tokio::spawn(async move { post_input(app, &uri, "after the restart", &new_id()).await })
    };
    wait_until("the send queues on the card's lock", || {
        boot.state.planner_recovery_lock_handles_for_test(&card_id) > held
    })
    .await;
    assert!(!send.is_finished(), "the send waits behind the restart");
    assert!(
        !restart.is_finished(),
        "the restart is still in thread/start"
    );

    drop(thread_start);
    let (status, body) = joined(restart, "the restart finishes").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let (status, body) = joined(send, "the send finishes").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let active = boot.active().await;
    assert_ne!(active.id, old.id);
    assert_eq!(
        body["worker_session_id"],
        json!(active.id),
        "the send went to the restarted session"
    );
    assert_eq!(boot.start_ops().await, 2, "the send started nothing itself");
    assert_eq!(boot.active_session_rows().await, 1);
    boot.wait_delivered("after the restart").await;
    boot.shutdown().await;
}
