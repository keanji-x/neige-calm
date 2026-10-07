use crate::support::worker_flow as wf;
use std::sync::{Arc, Mutex};

use calm_exec::flow::WorkerFlowSource;
use calm_server::db::RepoRead;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::worker_flow::codex_normalizer::{RolloutItem, RolloutLine};
use calm_server::worker_flow::codex_rollout::{
    CodexRolloutFlowSource, CodexRolloutFlowSourceOptions,
};
use calm_server::worker_flow::cursor::CODEX_ROLLOUT_SOURCE_KIND;
use calm_truth::worker_flow_sink::WorkerFlowSink;
use calm_types::worker::WorkerSessionState;
use calm_types::worker_flow::WorkerFlowItem;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tracing::instrument::WithSubscriber;

fn production_records() -> Vec<Value> {
    include_str!("../fixtures/codex_rollout/production_unknown.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn codex_unknown_top_level_payload_contract() {
    for payload in [
        None,
        Some(Value::Null),
        Some(json!(42)),
        Some(json!("text")),
        Some(json!([1, {"nested": true}])),
        Some(json!({"nested": [null]})),
    ] {
        let mut value = json!({"timestamp": "2026-10-07T01:29:47.104Z", "type": "future_record"});
        if let Some(payload) = payload {
            value["payload"] = payload;
        }
        let line: RolloutLine = serde_json::from_value(value).unwrap();
        assert_eq!(line.item, RolloutItem::Other);
    }
    for kind in [
        "session_meta",
        "response_item",
        "compacted",
        "turn_context",
        "event_msg",
    ] {
        for payload in [json!(42), Value::Null] {
            assert!(
                serde_json::from_value::<RolloutLine>(json!({
                    "timestamp": "now", "type": kind, "payload": payload
                }))
                .is_err(),
                "known {kind} must remain strict"
            );
        }
        assert!(
            serde_json::from_value::<RolloutLine>(json!({
                "timestamp": "now", "type": kind
            }))
            .is_err()
        );
    }
    for raw in [
        "{",
        r#"{"timestamp":"now"}"#,
        r#"{"type":"future_record"}"#,
        r#"{"timestamp":"now","type":"future_record","payload":[}"#,
        r#"{"timestamp":"now","type":"session_meta","payload":{"id":"one","id":"two"}}"#,
        r#"{"timestamp":"now","type":"session_meta","type":"compacted","payload":{}}"#,
    ] {
        assert!(serde_json::from_str::<RolloutLine>(raw).is_err(), "{raw}");
    }
}

#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn codex_unknown_top_level_production_tail_and_recovery() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card = "card-unknown-top-level";
    let seed = wf::seed_card_and_runtime_with_status(
        &repo,
        card,
        Some("thread-unknown"),
        WorkerSessionState::Exited,
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout.jsonl");
    let mut records = vec![
        wf::session_meta("thread-unknown"),
        wf::turn_context("turn-1"),
    ];
    records.extend(production_records());
    wf::write_rollout(&path, &records);
    let logs = Logs::default();
    let log_writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || log_writer.clone())
        .finish();
    let dispatch = tracing::Dispatch::new(subscriber);
    let source = CodexRolloutFlowSource::new_with_options(
        repo.clone(),
        seed.runtime.clone(),
        dir.path().to_path_buf(),
        CancellationToken::new(),
        CodexRolloutFlowSourceOptions {
            path_override: Some(path.clone()),
            ..Default::default()
        },
    );
    let session = wf::worker_session(&seed);
    let sink = WorkerFlowSink::new(repo.clone());
    source
        .capture(&session, &sink)
        .with_subscriber(dispatch.clone())
        .await
        .unwrap();
    let rows = repo
        .worker_flow_item_list_by_card(card, 0, 100, false)
        .await
        .unwrap();
    assert_eq!(
        rows.len(),
        2,
        "production unknown records must not silently disappear"
    );
    for (seq, row) in rows.iter().enumerate() {
        let item: WorkerFlowItem = serde_json::from_str(&row.payload).unwrap();
        assert!(
            matches!(&item, WorkerFlowItem::Unknown { raw_type, .. } if raw_type == "rollout_item")
        );
        assert_eq!(item.env().seq, seq as u64);
        assert_eq!(item.env().turn, 1);
        assert_eq!(
            item.env().raw_ref.as_ref().unwrap().line,
            Some(seq as u64 + 2)
        );
    }
    let cursor = repo
        .worker_flow_cursor_get(card, CODEX_ROLLOUT_SOURCE_KIND)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cursor.record_index, 4);
    assert_eq!(cursor.last_source_uuid, None);
    assert!(cursor.last_line_hash.is_some());

    // Cold restart reconstructs seq across unknown records and validates their hash.
    wf::append_rollout(
        &path,
        &[wf::assistant_message("after-unknown", "after restart")],
    );
    source
        .capture(&session, &sink)
        .with_subscriber(dispatch.clone())
        .await
        .unwrap();
    let rows = repo
        .worker_flow_item_list_by_card(card, 0, 100, false)
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);
    let item: WorkerFlowItem = serde_json::from_str(&rows[2].payload).unwrap();
    assert_eq!(item.env().seq, 2);
    assert_eq!(item.env().turn, 1);
    let cursor = repo
        .worker_flow_cursor_get(card, CODEX_ROLLOUT_SOURCE_KIND)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cursor.record_index, 5);
    assert_eq!(cursor.last_source_uuid.as_deref(), Some("after-unknown"));
    source
        .capture(&session, &sink)
        .with_subscriber(dispatch)
        .await
        .unwrap();
    assert_eq!(
        repo.worker_flow_item_list_by_card(card, 0, 100, false)
            .await
            .unwrap()
            .len(),
        3
    );
    let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(!logs.contains("malformed"), "{logs}");
    assert!(!logs.contains("prefix mismatch"), "{logs}");
}
