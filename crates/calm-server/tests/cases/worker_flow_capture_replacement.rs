use std::sync::Arc;

use calm_server::db::RepoRead;
use calm_server::event::EventBus;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::test_seams::{PausePoint, WorkerFlowPoint};
use calm_server::worker_flow::WorkerFlowDriver;
use calm_server::worker_flow::claude_transcript::ClaudeTranscriptFlowSourceOptions;
use calm_server::worker_flow::codex_rollout::CodexRolloutFlowSourceOptions;
use calm_truth::capture_test_seam::{install, install_commit};
use calm_truth::worker_flow_sink::WorkerFlowSink;

use super::Case;
use crate::support::worker_flow as wf;

async fn entered(pause: &PausePoint) {
    tokio::time::timeout(wf::LIVENESS_BUDGET, pause.entered.notified())
        .await
        .unwrap();
}

pub(super) async fn replacement(claude: bool, card: &str) {
    let (case, repo) = Case::new(claude, false, card).await;
    let driver = WorkerFlowDriver::new_with_source_options_for_test(
        repo.clone(),
        SharedCodexAppServer::new_stub(repo.clone()),
        Arc::new(WorkerFlowSink::new(repo.clone())),
        EventBus::new(),
        CodexRolloutFlowSourceOptions {
            path_override: Some(case.path.clone()),
            ..Default::default()
        },
        ClaudeTranscriptFlowSourceOptions {
            path_override: Some(case.path.clone()),
            ..Default::default()
        },
    );
    let mut tx = repo.pool().begin().await.unwrap();
    let replacement = calm_server::db::sqlite::session_supersede_and_start_tx(
        &mut tx,
        &case.seed.runtime.id,
        calm_server::session_projection_repo::WorkerSessionInit {
            id: format!("{}-replacement", case.seed.runtime.id),
            card_id: case.card().to_owned(),
            kind: case.seed.runtime.kind.clone(),
            agent_provider: case.seed.runtime.agent_provider.clone(),
            status: calm_types::worker::WorkerSessionState::Running,
            terminal_run_id: None,
            thread_id: case.seed.runtime.thread_id.clone(),
            session_id: case.seed.runtime.session_id.clone(),
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: calm_server::model::now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    // This hook executes on sqlx's SQLite worker, inside the real queued COMMIT.
    let commit = install_commit(case.card(), case.interrupted);
    driver
        .attach_runtime_for_test(case.seed.runtime.clone())
        .await
        .unwrap();
    tokio::time::timeout(wf::LIVENESS_BUDGET, commit.entered.notified())
        .await
        .unwrap();
    let durable = repo
        .worker_flow_cursor_get(case.card(), case.kind())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        durable.record_index,
        case.interrupted - 1,
        "WAL reader sees old checkpoint while COMMIT holds writer lock"
    );
    let old_token = driver.task_stop_tokens_for_test().await.pop().unwrap();
    let ready = install(case.card(), -1, WorkerFlowPoint::ReplacementReady);
    let loaded = install(case.card(), -1, WorkerFlowPoint::CheckpointLoaded);
    let settling = install(
        case.card(),
        case.interrupted,
        WorkerFlowPoint::CancellationSettling,
    );
    // A new runtime identity for the same source: the read model's original FK
    // remains valid; source rows are checked against their captured identity below.

    let attach = tokio::spawn({
        let driver = driver.clone();
        async move { driver.attach_runtime_for_test(replacement).await }
    });
    tokio::time::timeout(wf::LIVENESS_BUDGET, old_token.cancelled())
        .await
        .unwrap();
    let drained = tokio::time::timeout(wf::LIVENESS_BUDGET, async {
        tokio::select! {
            _ = settling.entered.notified() => true,
            _ = ready.entered.notified() => false,
        }
    })
    .await
    .unwrap();
    if drained {
        // Cancellation retains the in-flight future. COMMIT must finish before
        // the driver reaches replacement-ready or the source reads a checkpoint.
        commit.release.send(()).unwrap();
        settling.release.notify_one();
        entered(&ready).await;
        assert_eq!(
            repo.worker_flow_cursor_get(case.card(), case.kind())
                .await
                .unwrap()
                .unwrap()
                .record_index,
            case.interrupted
        );
        ready.release.notify_one();
        entered(&loaded).await;
    } else {
        // Original implementation: force the new source to read the old durable
        // cursor BEFORE releasing COMMIT. BEGIN IMMEDIATE cannot save that read.
        ready.release.notify_one();
        entered(&loaded).await;
        assert_eq!(
            repo.worker_flow_cursor_get(case.card(), case.kind())
                .await
                .unwrap(),
            Some(durable)
        );
        commit.release.send(()).unwrap();
    }
    // Round-trip actual durability before allowing the replacement's first CAS.
    wf::wait_until(wf::LIVENESS_BUDGET, || async {
        repo.worker_flow_cursor_get(case.card(), case.kind())
            .await
            .unwrap()
            .is_some_and(|c| c.record_index == case.interrupted)
    })
    .await;
    loaded.release.notify_one();
    attach.await.unwrap().unwrap();
    let idle = install(case.card(), case.lines.len() as i64, WorkerFlowPoint::Idle);
    if drained {
        case.wait(&repo).await;
        entered(&idle).await;
        idle.release.notify_one();
    } else {
        // No later Running/Idle event: Stale ends the only active capture task.
        wf::wait_until(wf::LIVENESS_BUDGET, || async {
            driver.tasks_alive_for_test().await == 0
        })
        .await;
        let rows = repo
            .worker_flow_item_list_by_card(case.card(), 0, 1000, false)
            .await
            .unwrap();
        assert_eq!(
            rows.len(),
            case.expected.len(),
            "queued old COMMIT made replacement Stale; active tail was lost"
        );
    }
    driver.stop_and_join_for_test().await;
    let rows = repo
        .worker_flow_item_list_by_card(case.card(), 0, 1000, false)
        .await
        .unwrap();
    let mut expected = case.expected.clone();
    let last = expected.last_mut().unwrap();
    last["session_id"] = serde_json::json!(format!("{}-replacement", case.seed.runtime.id));
    let payloads: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| serde_json::from_str(&r.payload).unwrap())
        .collect();
    assert_eq!(
        payloads, expected,
        "replacement reconstructs seq/turn and preserves every active record"
    );
    let cursor = repo
        .worker_flow_cursor_get(case.card(), case.kind())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cursor.record_index, case.lines.len() as i64);
    assert_eq!(cursor.last_source_uuid.as_deref(), Some("after"));
    drop(driver);
    repo.pool().close().await;
}

pub(super) async fn path_change(claude: bool, card: &str) {
    let (mut case, repo) = Case::new(claude, false, card).await;
    let (token, task) = case.spawn(repo.clone());
    case.wait(&repo).await;
    token.cancel();
    task.await.unwrap().unwrap();
    let old_rows = case.assert_rows(&repo).await;
    let old_expected = case.expected.clone();
    case.path = case.dir.path().join("replacement.jsonl");
    // B contains distinct records, but keeps the same provider runtime/card.
    for line in &mut case.lines {
        let uuid = if claude {
            line.get_mut("uuid")
        } else if line["type"] == "response_item" {
            line["payload"].get_mut("id")
        } else {
            None
        };
        if let Some(uuid) = uuid
            && let Some(text) = uuid.as_str()
        {
            *uuid = serde_json::json!(format!("b-{text}"));
        }
    }
    wf::write_rollout(&case.path, &case.lines);
    for item in &mut case.expected {
        item["raw_ref"]["source_path"] = serde_json::json!(case.path.to_string_lossy());
        item["source_uuid"] =
            serde_json::json!(format!("b-{}", item["source_uuid"].as_str().unwrap()));
    }
    let idle = install(case.card(), case.lines.len() as i64, WorkerFlowPoint::Idle);
    let (token, task) = case.spawn(repo.clone());
    tokio::time::timeout(wf::LIVENESS_BUDGET, idle.entered.notified())
        .await
        .unwrap();
    idle.release.notify_one();
    token.cancel();
    task.await.unwrap().unwrap();
    let cursor = repo
        .worker_flow_cursor_get(case.card(), case.kind())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cursor.source_path, case.path.to_string_lossy());
    assert_eq!(cursor.record_index, case.lines.len() as i64);
    assert_eq!(
        cursor.byte_offset,
        if claude {
            std::fs::metadata(&case.path).unwrap().len() as i64
        } else {
            0
        }
    );
    assert_eq!(cursor.last_source_uuid.as_deref(), Some("b-after"));
    let raw = serde_json::to_string(case.lines.last().unwrap()).unwrap();
    assert_eq!(
        cursor.last_line_hash.as_deref(),
        Some(&blake3::hash(raw.as_bytes()).to_hex()[..16])
    );
    let rows = repo
        .worker_flow_item_list_by_card(case.card(), 0, 1000, false)
        .await
        .unwrap();
    assert_eq!(
        rows[..old_rows.len()]
            .iter()
            .map(|r| (r.id, r.payload.clone()))
            .collect::<Vec<_>>(),
        old_rows
    );
    let payloads: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| serde_json::from_str(&r.payload).unwrap())
        .collect();
    assert_eq!(
        payloads,
        old_expected
            .into_iter()
            .chain(case.expected)
            .collect::<Vec<_>>(),
        "path B resets seq/turn and CAS compares the real path A durable checkpoint"
    );
    repo.pool().close().await;
}
