//! Today's report Reset over a report that holds data blocks (#2295): what it clears, what it does
//! to the tasks the report declared, and that no dropped data block lends its id to the reset report.

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use calm_server::track_report::TrackReportPayload;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

use crate::today_launchpad::{Boot, boot, count, ensure, post, resolve};

/// The canonical report's text before its first section heading: the contract header and the prose
/// contract, which Reset writes back as prose blocks.
fn initial_preamble() -> String {
    TrackReportPayload::initial()
        .body
        .split("# 概要")
        .next()
        .unwrap()
        .to_string()
}

/// A launchpad whose report is empty but for one data block, so nothing but that block stands
/// where Reset's initial prose goes. Returns the track id and the data block's id.
async fn launchpad_with_only(b: &Boot, kind: &str, payload: Value) -> (String, String) {
    b.dispatcher.abort_event_listener_for_test();
    let (_, ensured) = ensure(b.app.clone()).await;
    let track_id = ensured["track_id"].as_str().unwrap().to_string();
    for block in report(b, &track_id).await["blocks"].as_array().unwrap() {
        let response = b
            .app
            .clone()
            .oneshot(
                Request::delete(format!(
                    "/api/tracks/{track_id}/report/blocks/{}",
                    block["id"].as_str().unwrap()
                ))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "ifBlockRev": block["rev"] }).to_string(),
                ))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "delete {block}");
    }
    let id = add_data_block(b, &track_id, kind, payload).await;
    (track_id, id)
}

/// Reset aligns the initial prose onto nothing that was data: a data block whose text resembles that
/// prose is still deleted, and its id comes back on no block of the reset report.
async fn assert_reset_drops(b: &Boot, track_id: &str, data_id: &str) {
    let (status, body) = post(
        b.app.clone(),
        "/api/today/launchpad/report/reset",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reset={body}");
    let after = report(b, track_id).await;
    assert_eq!(
        after["body"],
        Value::String(TrackReportPayload::initial().body),
        "{after}"
    );
    for block in after["blocks"].as_array().unwrap() {
        assert_eq!(block["kind"], "prose", "{after}");
        assert_ne!(
            block["id"], data_id,
            "a reset block took the data block's id: {after}"
        );
    }
}

/// A task whose goal is the initial prose: matched by similarity, its id would turn into a prose
/// block's, which the task guard refuses as a kind change (a 400 that resets nothing).
#[tokio::test]
async fn resetting_drops_a_task_whose_text_resembles_the_initial_prose() {
    let b = boot().await;
    let (track_id, task_id) = launchpad_with_only(
        &b,
        "task",
        serde_json::json!({
            "key": "maintain-report", "kind": "codex", "goal": initial_preamble(),
            "declared_by": "user", "ready": false,
        }),
    )
    .await;
    assert_reset_drops(&b, &track_id, &task_id).await;
}

/// The same for a table: no guard notices, but the deleted table's id must not live on as prose.
#[tokio::test]
async fn resetting_drops_a_table_whose_text_resembles_the_initial_prose() {
    let b = boot().await;
    let mut table = fixture_table();
    table["caption"] = Value::String(initial_preamble());
    let (track_id, table_id) = launchpad_with_only(&b, "table", table).await;
    assert_reset_drops(&b, &track_id, &table_id).await;
}

/// The one 400 the reset route still answers: the actor middleware refuses a malformed header first.
#[tokio::test]
async fn resetting_with_a_malformed_actor_header_is_a_400_and_writes_nothing() {
    let b = boot().await;
    let (_, ensured) = ensure(b.app.clone()).await;
    let track_id = ensured["track_id"].as_str().unwrap().to_string();
    add_data_block(&b, &track_id, "table", fixture_table()).await;
    let before = report(&b, &track_id).await;

    let (status, body) = post(
        b.app.clone(),
        "/api/today/launchpad/report/reset",
        Some("kernel"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert_eq!(report(&b, &track_id).await, before);
}

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn report(b: &Boot, track_id: &str) -> Value {
    let (status, report) = get_json(b.app.clone(), &format!("/api/tracks/{track_id}/report")).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    report
}

/// Adds one data block through the block-level route the report editor uses; returns its id.
async fn add_data_block(b: &Boot, track_id: &str, kind: &str, payload: Value) -> String {
    let doc_rev = report(b, track_id).await["docRev"].clone();
    let (status, created) = post(
        b.app.clone(),
        &format!("/api/tracks/{track_id}/report/blocks"),
        None,
        Some(serde_json::json!({ "kind": kind, "payload": payload, "ifDocRev": doc_rev })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create {kind}: {created}");
    created["id"].as_str().unwrap().to_string()
}

/// A ready user task declaration: it projects into a pending task row.
fn user_task(key: &str) -> Value {
    serde_json::json!({
        "key": key, "kind": "terminal", "command": "true", "declared_by": "user", "ready": true,
    })
}

fn fixture_table() -> Value {
    serde_json::json!({
        "columns": [{ "key": "name", "label": "Name", "align": "left" }],
        "rows": [{ "name": "alpha" }],
    })
}

/// Today's report holding a pending task `queued`, a claimed task `claimed` and a table.
/// Returns the launchpad track id and the two task block ids. The dispatcher's event listener is
/// stopped first, so only this fixture claims a task and every later row state is the write's own.
async fn launchpad_with_data_blocks(b: &Boot) -> (String, Vec<String>) {
    b.dispatcher.abort_event_listener_for_test();
    let (_, ensured) = ensure(b.app.clone()).await;
    let track_id = ensured["track_id"].as_str().unwrap().to_string();
    let queued = add_data_block(b, &track_id, "task", user_task("queued")).await;
    let claimed = add_data_block(b, &track_id, "task", user_task("claimed")).await;
    add_data_block(b, &track_id, "table", fixture_table()).await;
    let claimed_id: String =
        sqlx::query_scalar("SELECT id FROM current_tasks WHERE track_id=?1 AND key='claimed'")
            .bind(&track_id)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(b.repo.pool())
        .await
        .unwrap();
    let claimed_rows =
        calm_server::db::sqlite::task_claim_pending_tx(&mut tx, &claimed_id, 1, &[], false)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(claimed_rows, 1, "the fixture must hold a claimed task");
    (track_id, vec![queued, claimed])
}

/// Each task row of the track as `key|status|stale`, sorted.
async fn task_rows(b: &Boot, track_id: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT key || '|' || status || '|' || (context_stale_at_ms IS NOT NULL) \
         FROM tasks WHERE track_id=?1 ORDER BY key",
    )
    .bind(track_id)
    .fetch_all(b.repo.pool())
    .await
    .unwrap()
}

/// The distinct kinds of the events persisted after event `after`, sorted.
async fn event_kinds_after(b: &Boot, after: i64) -> Vec<String> {
    sqlx::query_scalar("SELECT DISTINCT kind FROM events WHERE id > ?1 ORDER BY kind")
        .bind(after)
        .fetch_all(b.repo.pool())
        .await
        .unwrap()
}

async fn last_event_id(b: &Boot) -> i64 {
    count(b, "SELECT COALESCE(MAX(id), 0) FROM events").await
}

/// Reset is the owner asking for the canonical empty document, so the data blocks go too,
/// read back through the same `GET` the report view uses.
#[tokio::test]
async fn resetting_todays_report_clears_data_blocks() {
    let b = boot().await;
    let (track_id, _) = launchpad_with_data_blocks(&b).await;
    let before = report(&b, &track_id).await;
    let kinds: Vec<&str> = before["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|block| block["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"task") && kinds.contains(&"table"),
        "the fixture must hold data blocks: {before}"
    );

    let (status, reset) = post(
        b.app.clone(),
        "/api/today/launchpad/report/reset",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reset={reset}");

    let after = report(&b, &track_id).await;
    let initial = TrackReportPayload::initial();
    assert_eq!(after["summary"], Value::String(initial.summary));
    assert_eq!(after["body"], Value::String(initial.body), "{after}");
    assert!(
        after["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|block| block["kind"] == "prose"),
        "{after}"
    );
    let (_, resolved) = resolve(b.app.clone()).await;
    assert_eq!(
        resolved["report_has_noninitial_content"],
        Value::Bool(false),
        "{resolved}"
    );
}

/// What Reset does to the tasks is what the block-level DELETE does: the pending row goes, the
/// claimed row is marked stale, and the same kinds of events are emitted. The DELETE leaves a
/// tombstone block; Reset leaves none, since it puts back the initial document.
#[tokio::test]
async fn resetting_todays_report_has_the_task_side_effects_of_a_block_delete() {
    let deleted = boot().await;
    let (track_id, task_blocks) = launchpad_with_data_blocks(&deleted).await;
    let fixture_end = last_event_id(&deleted).await;
    for block_id in &task_blocks {
        let rev = report(&deleted, &track_id).await["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|block| block["id"] == block_id.as_str())
            .unwrap()["rev"]
            .clone();
        let response = deleted
            .app
            .clone()
            .oneshot(
                Request::delete(format!("/api/tracks/{track_id}/report/blocks/{block_id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "ifBlockRev": rev }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "delete {block_id}");
    }
    let delete_rows = task_rows(&deleted, &track_id).await;
    let delete_events = event_kinds_after(&deleted, fixture_end).await;

    let reset = boot().await;
    let (track_id, _) = launchpad_with_data_blocks(&reset).await;
    let fixture_end = last_event_id(&reset).await;
    let (status, body) = post(
        reset.app.clone(),
        "/api/today/launchpad/report/reset",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reset={body}");

    assert_eq!(
        delete_rows,
        vec!["claimed|dispatched|1".to_string()],
        "the block DELETE baseline"
    );
    assert_eq!(task_rows(&reset, &track_id).await, delete_rows);
    assert_eq!(
        event_kinds_after(&reset, fixture_end).await,
        delete_events,
        "the same kinds of events as the block DELETE"
    );
    for kind in ["card.updated", "track.report_edited", "plan.updated"] {
        assert!(
            delete_events.iter().any(|emitted| emitted == kind),
            "{kind} missing from {delete_events:?}"
        );
    }
}
