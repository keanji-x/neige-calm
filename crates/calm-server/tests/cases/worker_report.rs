//! #2492: `neige_worker_report` over the production MCP socket. Each call becomes one
//! kernel-written `track.wake_requested` (source `worker_watch`) for the caller's Track under its own
//! key; malformed, cross-Track, non-Assistant and revoked-session reports write nothing.
use super::assistant_terminal::{agent_token, foreign_track};
use super::task_terminal::worker_running;
use super::terminal_support::Harness;
use calm_server::db::prelude::*;
use calm_server::model::CardRole;
use calm_server::worker_watch::{Note, Outcome, Verdict, report_line};
use calm_types::observation::WORKER_WATCH_WAKE_SOURCE;
use serde_json::{Value, json};

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
    let (_, _, assistant) = agent_token(&h, &h.track, CardRole::Assistant).await;
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
        let result = &reply["result"]["structuredContent"];
        assert_eq!(result.as_object().unwrap().len(), 1, "{result}");
        let key = result["key"].as_str().unwrap().to_owned();
        assert!(key.starts_with(&format!("{}:", w.task)), "{key}");
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
async fn every_call_is_delivered_under_its_own_key() {
    let h = Harness::start().await;
    let (_, _, assistant) = agent_token(&h, &h.track, CardRole::Assistant).await;
    let w = worker_running(&h, "claude", &h.track, None).await;
    let args = json!({"attempt_id":w.task,"outcome":"idle_at_prompt"});
    let first = report(&h, &assistant, args.clone()).await;
    let again = report(&h, &assistant, args).await;
    let keys: Vec<String> = wakes(&h).await.into_iter().map(|wake| wake.3).collect();
    assert_eq!(
        keys,
        vec![
            first["result"]["structuredContent"]["key"]
                .as_str()
                .unwrap()
                .to_owned(),
            again["result"]["structuredContent"]["key"]
                .as_str()
                .unwrap()
                .to_owned(),
        ]
    );
    assert_ne!(keys[0], keys[1]);
}

/// The session retires (`/planner/restart`) after the report's caller check and before its write:
/// the check where the wake commits refuses it.
#[tokio::test]
async fn a_report_from_a_session_retired_mid_call_writes_nothing() {
    use calm_server::test_seams::{PausePoint, install_pause_for_test};
    use calm_server::worker_watch::WORKER_REPORT_AUTHORIZED;
    use std::sync::Arc;
    use tokio::sync::Notify;

    let h = Harness::start().await;
    let (card, session, assistant) = agent_token(&h, &h.track, CardRole::Assistant).await;
    let w = worker_running(&h, "claude", &h.track, None).await;
    let args = json!({"attempt_id":w.task,"outcome":"unclear"});

    let paused = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    install_pause_for_test(
        WORKER_REPORT_AUTHORIZED,
        &session,
        PausePoint {
            entered: paused.0.clone(),
            release: paused.1.clone(),
        },
    );
    let call = {
        let (h, token) = (&h, assistant.clone());
        async move { report(h, &token, args).await }
    };
    let retire = async {
        tokio::time::timeout(std::time::Duration::from_secs(10), paused.0.notified())
            .await
            .expect("the report passed its caller check");
        h.sql
            .session_projection_set_status_for_card(
                &card,
                calm_server::session_projection_repo::WorkerSessionState::Exited,
            )
            .await
            .unwrap();
        paused.1.notify_one();
    };
    let (refused, ()) = tokio::join!(call, retire);
    assert_eq!(refused["error"]["code"], -32403, "{refused}");
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("nothing was reported"),
        "{refused}"
    );
    assert!(wakes(&h).await.is_empty());
}

#[tokio::test]
async fn malformed_cross_track_and_planner_reports_write_nothing() {
    let h = Harness::start().await;
    let (_, _, assistant) = agent_token(&h, &h.track, CardRole::Assistant).await;
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
