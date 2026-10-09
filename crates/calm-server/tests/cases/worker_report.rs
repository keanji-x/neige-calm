//! #2492: `neige_worker_report` over the production MCP socket. Each outcome becomes one
//! kernel-written `track.wake_requested` (source `worker_watch`) for the caller's Track, keyed so a
//! repeat in the same turn is not delivered twice; malformed, cross-Track and non-Assistant reports
//! write nothing.
use super::assistant_terminal::{agent_token, foreign_track};
use super::task_terminal::worker_running;
use super::terminal_support::Harness;
use calm_server::db::prelude::*;
use calm_server::model::CardRole;
use calm_server::worker_watch::{Note, Outcome, Verdict, report_line};
use calm_types::observation::WORKER_WATCH_WAKE_SOURCE;
use serde_json::{Value, json};

/// Record a turn input for the caller's session, as the harness does when a turn starts.
async fn record_turn(h: &Harness, card: &str, session: &str, at_ms: i64) {
    sqlx::query(
        "INSERT INTO harness_items \
           (worker_session_id, card_id, track_id, thread_id, method, params, created_at_ms, \
            item_type, input_segments) \
         SELECT ?2, id, track_id, 'watch-thread', 'item/completed', '{}', ?3, 'userMessage', \
                '[{\"presentation\":\"user\",\"text\":\"watch\"}]' \
           FROM cards WHERE id = ?1",
    )
    .bind(card)
    .bind(session)
    .bind(at_ms)
    .execute(h.sql.pool())
    .await
    .unwrap();
}

/// Every persisted `track.wake_requested` as `(actor, track, source, key, text)`, oldest first.
async fn wakes(h: &Harness) -> Vec<(String, String, String, String, String)> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT actor, payload FROM events WHERE kind = 'track.wake_requested' ORDER BY id",
    )
    .fetch_all(h.sql.pool())
    .await
    .unwrap();
    rows.into_iter()
        .map(|(actor, payload)| {
            let v: Value = serde_json::from_str(&payload).unwrap();
            let text = |key: &str| v[key].as_str().unwrap().to_owned();
            (
                actor,
                text("track_id"),
                text("source"),
                text("key"),
                text("text"),
            )
        })
        .collect()
}

async fn report(h: &Harness, token: &str, args: Value) -> Value {
    h.call_with_token(token, "neige_worker_report", args).await
}

#[tokio::test]
async fn each_outcome_wakes_the_planner_with_its_one_kernel_line() {
    let h = Harness::start().await;
    let (card, session, assistant) = agent_token(&h, &h.track, CardRole::Assistant).await;
    record_turn(&h, &card, &session, 7_000).await;
    let w = worker_running(&h, "claude", &h.track, None).await;
    let task_key = h.sql.task_get(&w.task).await.unwrap().unwrap().key;
    let mut expected = Vec::new();
    for outcome in Outcome::ALL {
        let note = (outcome == Outcome::NeedsOwner).then_some("The worker asks to log in.");
        let mut args = json!({"attempt_id":w.task,"outcome":outcome.as_str()});
        if let Some(note) = note {
            args["note"] = json!(note);
        }
        let reply = report(&h, &assistant, args).await;
        assert!(reply.get("error").is_none(), "{reply}");
        let key = format!("{}:{}:7000", w.task, outcome.as_str());
        assert_eq!(
            reply["result"]["structuredContent"],
            json!({"key": key, "replayed": false})
        );
        let verdict = Verdict::new(outcome, note.map(|note| Note::parse(note).unwrap())).unwrap();
        let line = report_line(&task_key, &w.task, &verdict).unwrap();
        assert!(!line.contains('\n'));
        expected.push((
            r#"{"kind":"Kernel"}"#.to_owned(),
            h.track.clone(),
            WORKER_WATCH_WAKE_SOURCE.to_owned(),
            key,
            line,
        ));
    }
    assert_eq!(wakes(&h).await, expected);
}

#[tokio::test]
async fn a_repeated_report_in_the_same_turn_is_replayed_and_a_later_turn_delivers_again() {
    let h = Harness::start().await;
    let (card, session, assistant) = agent_token(&h, &h.track, CardRole::Assistant).await;
    let w = worker_running(&h, "claude", &h.track, None).await;
    let args = json!({"attempt_id":w.task,"outcome":"idle_at_prompt"});

    // No recorded turn: nothing to key the report by, so nothing is written.
    let unrecorded = report(&h, &assistant, args.clone()).await;
    assert_eq!(unrecorded["error"]["code"], -32409, "{unrecorded}");
    assert!(wakes(&h).await.is_empty());

    record_turn(&h, &card, &session, 7_000).await;
    let first = report(&h, &assistant, args.clone()).await;
    assert_eq!(
        first["result"]["structuredContent"]["replayed"], false,
        "{first}"
    );
    let again = report(&h, &assistant, args.clone()).await;
    assert_eq!(
        again["result"]["structuredContent"],
        json!({"key": format!("{}:idle_at_prompt:7000", w.task), "replayed": true})
    );
    assert_eq!(wakes(&h).await.len(), 1);

    // A different outcome in the same turn is a different report.
    let unclear = report(
        &h,
        &assistant,
        json!({"attempt_id":w.task,"outcome":"unclear"}),
    )
    .await;
    assert_eq!(
        unclear["result"]["structuredContent"]["replayed"], false,
        "{unclear}"
    );
    assert_eq!(wakes(&h).await.len(), 2);

    // The next watch message is a new turn: the same outcome is delivered again.
    record_turn(&h, &card, &session, 9_000).await;
    let later = report(&h, &assistant, args).await;
    assert_eq!(
        later["result"]["structuredContent"],
        json!({"key": format!("{}:idle_at_prompt:9000", w.task), "replayed": false})
    );
    assert_eq!(wakes(&h).await.len(), 3);
}

#[tokio::test]
async fn malformed_cross_track_and_planner_reports_write_nothing() {
    let h = Harness::start().await;
    let (card, session, assistant) = agent_token(&h, &h.track, CardRole::Assistant).await;
    record_turn(&h, &card, &session, 7_000).await;
    let w = worker_running(&h, "claude", &h.track, None).await;
    let foreign = foreign_track(&h).await;
    let theirs = worker_running(&h, "claude", &foreign, None).await;

    let invalid = report(
        &h,
        &assistant,
        json!({"attempt_id":w.task,"outcome":"done"}),
    )
    .await;
    assert_eq!(invalid["error"]["code"], -32602, "{invalid}");
    let noteless = report(
        &h,
        &assistant,
        json!({"attempt_id":w.task,"outcome":"needs_owner"}),
    )
    .await;
    assert_eq!(noteless["error"]["code"], -32602, "{noteless}");
    assert!(
        noteless["error"]["message"]
            .as_str()
            .unwrap()
            .contains("needs a note"),
        "{noteless}"
    );
    let cross = report(
        &h,
        &assistant,
        json!({"attempt_id":theirs.task,"outcome":"unclear"}),
    )
    .await;
    assert_eq!(cross["error"]["code"], -32403, "{cross}");
    assert!(
        cross["error"]["message"]
            .as_str()
            .unwrap()
            .contains("outside the caller's Track"),
        "{cross}"
    );
    let planner = h
        .call(
            "neige_worker_report",
            json!({"attempt_id":w.task,"outcome":"unclear"}),
        )
        .await;
    assert_eq!(planner["error"]["code"], -32403, "{planner}");
    assert!(
        planner["error"]["message"]
            .as_str()
            .unwrap()
            .contains("got=Planner"),
        "{planner}"
    );
    assert!(wakes(&h).await.is_empty());
}
