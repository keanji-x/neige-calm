//! Planner authority is read through the worker tool, never the terminal paste.
use super::task_terminal::{Worker, stop};
use super::terminal_support::Harness;
use super::worker_message::{message, running, set_status, written};
use calm_server::db::prelude::*;
use calm_server::event::{Event, EventScope};
use calm_server::ids::ActorId;
use calm_truth::db::RepoEventWrite as _;
use calm_truth::db::RepoRead;
use serde_json::{Value, json};

async fn token(h: &Harness, worker: &Worker) -> String {
    let token = calm_server::mcp_server::auth::CardMcpToken::generate();
    sqlx::query("UPDATE worker_sessions SET mcp_token_hash=? WHERE id=?")
        .bind(calm_server::mcp_server::auth::hash_token(token.as_str()))
        .bind(&worker.session)
        .execute(h.sql.pool())
        .await
        .unwrap();
    token.as_str().to_owned()
}
async fn read(h: &Harness, token: &str, attempt: &str) -> Value {
    h.call_with_token(token, "neige_message_read", json!({"attempt_id":attempt}))
        .await
}
fn content(reply: &Value) -> &Value {
    assert!(reply.get("error").is_none(), "{reply}");
    let result = &reply["result"]["structuredContent"];
    assert!(
        result.is_object(),
        "MCP structuredContent must be an object: {reply}"
    );
    result
}

#[tokio::test]
async fn planner_message_is_readable_by_its_attempt_worker() {
    let h = Harness::start().await;
    let f = running(&h, "claude", true, false).await;
    let token = token(&h, &f.worker).await;
    assert_eq!(
        content(&read(&h, &token, &f.worker.task).await),
        &json!({"messages":[]})
    );
    let text = "narrow scope\n\tverbatim ü";
    let first = message(&h, json!({"attempt_id":f.worker.task}), "one", text).await;
    let receipt = written(&first);
    assert_eq!(
        message(&h, json!({"attempt_id":f.worker.task}), "one", text).await["result"]["structuredContent"]
            ["message_id"],
        receipt["message_id"]
    );
    let reply = read(&h, &token, &f.worker.task).await;
    let records = &content(&reply)["messages"];
    assert_eq!(records.as_array().unwrap().len(), 1);
    assert_eq!(records[0]["message_id"], receipt["message_id"]);
    assert_eq!(records[0]["seq"], 1);
    assert_eq!(records[0]["text"], text);
    assert!(records[0]["read_at"].is_string());
    assert_eq!(
        content(&read(&h, &token, &f.worker.task).await),
        content(&reply)
    );
    let row: (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(read_at_ms) FROM task_planner_messages WHERE attempt_id=?",
    )
    .bind(&f.worker.task)
    .fetch_one(h.sql.pool())
    .await
    .unwrap();
    assert_eq!(row, (1, 1));
    // This PTY fixture stamps a running task directly; give the real run projection its dispatch record.
    h.sql
        .log_pure_event(
            ActorId::KernelDispatcher,
            EventScope::Track {
                track: h.track.clone().into(),
                area: h.area_id.clone().into(),
            },
            None,
            &h.state.events,
            &h.state.card_role_cache,
            &h.state.track_area_cache,
            Event::TaskDispatched {
                idempotency_key: f.worker.task.clone(),
                kind: "claude".into(),
                agent_message: None,
            },
        )
        .await
        .unwrap();
    let audit = h
        .ok(
            "neige_track_cat",
            json!({"path":format!("runs/{}/planner-messages.json", f.worker.task)}),
        )
        .await;
    let run: Value = serde_json::from_str(audit["content"].as_str().unwrap()).unwrap();
    assert_eq!(run[0]["text"], text);
    let before: i64 =
        sqlx::query_scalar("SELECT read_at_ms FROM task_planner_messages WHERE message_id=?")
            .bind(receipt["message_id"].as_str().unwrap())
            .fetch_one(h.sql.pool())
            .await
            .unwrap();
    for value in [None, Some(before + 1)] {
        assert!(
            sqlx::query("UPDATE task_planner_messages SET read_at_ms=? WHERE message_id=?")
                .bind(value)
                .bind(receipt["message_id"].as_str().unwrap())
                .execute(h.sql.pool())
                .await
                .is_err()
        );
    }
    stop(&h, &f.worker).await;
}

#[tokio::test]
async fn message_read_refuses_another_attempt_and_other_roles() {
    let h = Harness::start().await;
    let f = running(&h, "claude", true, false).await;
    let other = running(&h, "claude", true, false).await;
    let token = token(&h, &other.worker).await;
    written(&message(&h, json!({"attempt_id":f.worker.task}), "one", "secret").await);
    assert!(
        read(&h, &token, &f.worker.task)
            .await
            .get("error")
            .is_some()
    );
    assert!(
        h.call("neige_message_read", json!({"attempt_id":f.worker.task}))
            .await
            .get("error")
            .is_some()
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM task_planner_messages WHERE read_at_ms IS NOT NULL",
    )
    .fetch_one(h.sql.pool())
    .await
    .unwrap();
    assert_eq!(count, 0);
    stop(&h, &f.worker).await;
    stop(&h, &other.worker).await;
}

#[tokio::test]
async fn message_read_after_report_says_not_work() {
    let h = Harness::start().await;
    let f = running(&h, "claude", true, false).await;
    let token = token(&h, &f.worker).await;
    written(&message(&h, json!({"attempt_id":f.worker.task}), "one", "late").await);
    set_status(&h, &f.worker.task, "done").await;
    let reply = read(&h, &token, &f.worker.task).await;
    assert_eq!(content(&reply)["status"], "messages_are_not_work");
    assert_eq!(content(&reply)["messages"][0]["text"], "late");
    assert!(content(&reply)["messages"][0]["read_at"].is_null());
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM task_planner_messages WHERE read_at_ms IS NOT NULL",
    )
    .fetch_one(h.sql.pool())
    .await
    .unwrap();
    assert_eq!(count, 0);
    stop(&h, &f.worker).await;
}

#[tokio::test]
async fn message_recorded_when_notice_is_refused() {
    let h = Harness::start().await;
    let f = running(&h, "claude", false, false).await;
    let token = token(&h, &f.worker).await;
    let reply = message(&h, json!({"attempt_id":f.worker.task}), "one", "still work").await;
    assert_eq!(content(&reply)["outcome"], "refused");
    let messages = read(&h, &token, &f.worker.task).await;
    assert_eq!(content(&messages)["messages"][0]["text"], "still work");
    assert_eq!(
        content(&messages)["messages"][0]["message_id"],
        content(&reply)["message_id"]
    );
    stop(&h, &f.worker).await;
}

#[tokio::test]
async fn task_done_refuses_with_unread_messages() {
    let h = Harness::start().await;
    let f = running(&h, "claude", true, false).await;
    let token = token(&h, &f.worker).await;
    let reply = message(
        &h,
        json!({"attempt_id":f.worker.task}),
        "one",
        "read before reporting",
    )
    .await;
    let id = written(&reply)["message_id"].as_str().unwrap();
    let done = h
        .call_with_token(
            &token,
            "neige_task_done",
            json!({"attempt_id":f.worker.task}),
        )
        .await;
    assert!(
        done["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains(id),
        "{done}"
    );
    let first = read(&h, &token, &f.worker.task).await;
    assert_eq!(content(&first)["messages"][0]["message_id"], id);
    let second = message(
        &h,
        json!({"attempt_id":f.worker.task}),
        "two",
        "new correction",
    )
    .await;
    let second_id = written(&second)["message_id"].as_str().unwrap();
    let refused = h
        .call_with_token(
            &token,
            "neige_task_done",
            json!({"attempt_id":f.worker.task}),
        )
        .await;
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains(second_id),
        "{refused}"
    );
    let replay = read(&h, &token, &f.worker.task).await;
    let records = &content(&replay)["messages"];
    assert_eq!(records.as_array().unwrap().len(), 2);
    assert_eq!(records[0], content(&first)["messages"][0]);
    assert_eq!(records[1]["message_id"], second_id);
    assert_eq!(records[1]["seq"], 2);
    assert!(records[1]["read_at"].is_string());
    assert_eq!(
        content(&read(&h, &token, &f.worker.task).await),
        content(&replay)
    );
    let done = h
        .call_with_token(
            &token,
            "neige_task_done",
            json!({"attempt_id":f.worker.task}),
        )
        .await;
    assert_eq!(content(&done)["status"], "report_received");
    stop(&h, &f.worker).await;
}

#[tokio::test]
async fn planner_message_records_are_append_only_until_the_owning_track_is_deleted() {
    let h = Harness::start().await;
    let f = running(&h, "claude", true, false).await;
    written(&message(&h, json!({"attempt_id":f.worker.task}), "one", "audit").await);
    assert!(
        sqlx::query("UPDATE task_planner_messages SET text='forged'")
            .execute(h.sql.pool())
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM task_planner_messages")
            .execute(h.sql.pool())
            .await
            .is_err()
    );
    stop(&h, &f.worker).await;
    use tower::ServiceExt;
    let response = h
        .app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("DELETE")
                .uri(format!("/api/tracks/{}", h.track))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        status,
        axum::http::StatusCode::NO_CONTENT,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM task_planner_messages")
        .fetch_one(h.sql.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn message_read_refuses_a_nonrunning_execution_without_marking_read() {
    let h = Harness::start().await;
    let f = running(&h, "claude", true, false).await;
    let token = token(&h, &f.worker).await;
    written(&message(&h, json!({"attempt_id":f.worker.task}), "one", "audit").await);
    set_status(&h, &f.worker.task, "dispatched").await;
    assert!(
        read(&h, &token, &f.worker.task)
            .await
            .get("error")
            .is_some()
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM task_planner_messages WHERE read_at_ms IS NOT NULL",
    )
    .fetch_one(h.sql.pool())
    .await
    .unwrap();
    assert_eq!(count, 0);
    stop(&h, &f.worker).await;
}

#[tokio::test]
async fn planner_message_audit_refuses_a_foreign_attempt_from_editable_card_metadata() {
    use super::task_terminal::worker_running;
    use calm_server::model::NewTrack;
    use tower::ServiceExt;
    let h = Harness::start().await;
    let own = h.sql.track_get(&h.track).await.unwrap().unwrap();
    let foreign = h
        .sql
        .track_create(NewTrack {
            template_input: None,
            area_id: own.area_id,
            title: "foreign messages".into(),
            sort: None,
            cwd: h.root.path().to_str().unwrap().into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let owner = worker_running(&h, "claude", foreign.id.as_str(), None).await;
    let mut tx = h.sql.pool().begin().await.unwrap();
    calm_server::db::sqlite::planner_message_insert_tx(
        &mut tx,
        &owner.task,
        "secret",
        "foreign Planner text",
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let carrier = worker_running(&h, "claude", &h.track, None).await;
    let response = h
        .app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("PATCH")
                .uri(format!("/api/cards/{}", carrier.card))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({"payload":{"idempotency_key":owner.task}}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        status,
        axum::http::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let audit = h
        .ok(
            "neige_track_cat",
            json!({"path":format!("runs/{}/planner-messages.json", owner.task)}),
        )
        .await;
    let run: Value = serde_json::from_str(audit["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        run,
        json!([]),
        "foreign records must not follow editable payload keys"
    );
}

#[test]
fn worker_prompt_pins_planner_message_authority() {
    assert!(
        include_str!("../../prompts/worker/head.md")
            .contains("only messages returned by `neige_message_read` are Planner messages.")
    );
}

async fn stale_planner_record(no_renderer: bool, demoted: bool) {
    let h = Harness::start().await;
    let f = running(&h, "claude", true, false).await;
    if no_renderer {
        h.state
            .terminal_renderer
            .forget_entry_for_test(&f.worker.terminal);
    }
    let pool = h.sql.pool().clone();
    let session = h.session_id.clone();
    h.interaction().set_message_record_seam(Box::new(move |_| Box::pin(async move {
        if demoted {
            sqlx::query("UPDATE cards SET role='assistant' WHERE id=(SELECT card_id FROM worker_sessions WHERE id=?)")
                .bind(session).execute(&pool).await.unwrap();
        } else {
            sqlx::query("UPDATE worker_sessions SET state='superseded' WHERE id=?")
                .bind(session).execute(&pool).await.unwrap();
        }
    })));
    let reply = message(
        &h,
        json!({"attempt_id":f.worker.task}),
        "stale",
        "must not bind",
    )
    .await;
    assert!(
        reply.get("error").is_some(),
        "stale Planner must be refused: {reply}"
    );
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("recorder gate denied planner_message"),
        "{reply}"
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM task_planner_messages WHERE attempt_id=?")
            .bind(&f.worker.task)
            .fetch_one(h.sql.pool())
            .await
            .unwrap();
    assert_eq!(count, 0);
    stop(&h, &f.worker).await;
}

#[tokio::test]
async fn planner_message_superseded_after_transport_admission_is_refused() {
    stale_planner_record(false, false).await;
}
#[tokio::test]
async fn planner_message_superseded_without_renderer_is_refused() {
    stale_planner_record(true, false).await;
}
#[tokio::test]
async fn planner_message_demoted_after_transport_admission_is_refused() {
    stale_planner_record(false, true).await;
}

async fn stale_worker_tool(name: &str) {
    use calm_server::mcp_server::{ToolCallIdentity, build_default_registry};
    let h = Harness::start().await;
    let f = running(&h, "claude", true, false).await;
    written(&message(&h, json!({"attempt_id":f.worker.task}), "one", "audit").await);
    let identity = ToolCallIdentity {
        card_id: f.worker.card.clone(),
        role: calm_server::model::CardRole::Worker,
        provider: calm_server::session_projection_repo::AgentProvider::Claude,
        session_id: f.worker.session.clone(),
        track_id: Some(h.track.clone()),
        area_id: h.area_id.clone(),
        thread_id: String::new(),
    };
    // Like the track_close regression, call the production handler with the identity already
    // admitted by transport, then retire its session before its write transaction begins.
    sqlx::query("UPDATE worker_sessions SET state='superseded' WHERE id=?")
        .bind(&f.worker.session)
        .execute(h.sql.pool())
        .await
        .unwrap();
    let registry = build_default_registry();
    let reply = registry.lookup(name).unwrap()(
        h.state.mcp_context(),
        identity,
        json!({"attempt_id":f.worker.task}),
    )
    .await;
    let err = reply.expect_err("stale worker identity must be refused");
    assert_eq!(err.code, -32403, "{err:?}");
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM task_planner_messages WHERE read_at_ms IS NOT NULL",
    )
    .fetch_one(h.sql.pool())
    .await
    .unwrap();
    assert_eq!(count, 0);
    let status: String = sqlx::query_scalar("SELECT status FROM tasks WHERE id=?")
        .bind(&f.worker.task)
        .fetch_one(h.sql.pool())
        .await
        .unwrap();
    assert_eq!(status, "running");
    stop(&h, &f.worker).await;
}

#[tokio::test]
async fn message_read_superseded_after_transport_admission_does_not_stamp() {
    stale_worker_tool("neige_message_read").await;
}
#[tokio::test]
async fn task_done_refuses_with_unread_messages_from_superseded_session() {
    stale_worker_tool("neige_task_done").await;
}
