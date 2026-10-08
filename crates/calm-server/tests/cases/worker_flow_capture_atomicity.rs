use std::path::PathBuf;
use std::sync::Arc;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::SqlxRepo;
use calm_truth::capture_test_seam::{CapturePoint, install};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::support::worker_flow as wf;

struct Case {
    dir: tempfile::TempDir,
    seed: wf::SeededRuntime,
    path: PathBuf,
    claude: bool,
    lines: Vec<Value>,
    expected: Vec<Value>,
    interrupted: i64,
}

impl Case {
    async fn new(claude: bool, multi: bool, card: &str) -> (Self, Arc<SqlxRepo>) {
        let dir = tempfile::tempdir().unwrap();
        let repo = open(dir.path()).await;
        let seed = if claude {
            wf::seed_claude_card_and_runtime(&repo, card, "capture-session", "/tmp").await
        } else {
            wf::seed_card_and_runtime(&repo, card, Some("capture-thread")).await
        };
        let path = dir.path().join("capture.jsonl");
        let mut lines = if claude {
            vec![
                wf::claude_user_string("before", "same text"),
                if multi {
                    wf::claude_assistant(
                        "middle",
                        "/tmp",
                        vec![wf::claude_text("block one"), wf::claude_text("block two")],
                    )
                } else {
                    wf::claude_user_string("middle", "middle text")
                },
                wf::claude_user_string("after", "same text"),
            ]
        } else {
            vec![
                wf::session_meta("capture-thread"),
                wf::user_message("before", "same text"),
                wf::user_message("middle", "middle text"),
                wf::user_message("after", "same text"),
            ]
        };
        for line in &mut lines {
            line["timestamp"] = json!("1970-01-01T00:00:01Z");
        }
        wf::write_rollout(&path, &lines);
        // Fixed expected vocabulary, not a second implementation of the normalizer.
        let mut expected = Vec::new();
        let provider = if claude { "claude" } else { "codex" };
        let first = if claude { 0 } else { 1 };
        let specs = if multi {
            vec![
                (first, "before", "same text", false, 1),
                (first + 1, "middle", "block one", true, 1),
                (first + 1, "middle", "block two", true, 1),
                (first + 2, "after", "same text", false, 2),
            ]
        } else {
            vec![
                (
                    first,
                    "before",
                    "same text",
                    false,
                    if claude { 1 } else { 0 },
                ),
                (
                    first + 1,
                    "middle",
                    "middle text",
                    false,
                    if claude { 2 } else { 0 },
                ),
                (
                    first + 2,
                    "after",
                    "same text",
                    false,
                    if claude { 3 } else { 0 },
                ),
            ]
        };
        for (seq, (line, uuid, text, agent, turn)) in specs.into_iter().enumerate() {
            let mut item = json!({"type": if agent {"agentMessage"} else {"userMessage"},
                "seq": seq, "turn": turn, "session_id": seed.runtime.id, "provider": provider,
                "timestamp": 1000, "source_uuid": uuid, "provider_extra": null,
                "raw_ref": {"provider": provider, "source_path": path.to_string_lossy(), "line": line,
                    "record_type": if claude {if agent {"assistant"} else {"user"}} else {"message"}}});
            if agent {
                item["text"] = json!(text);
                item["is_final"] = json!(false);
                item["phase"] = Value::Null;
            } else {
                item["content"] = json!([{"type":"text", "text":text}]);
            }
            expected.push(item);
        }
        (
            Self {
                dir,
                seed,
                path,
                claude,
                lines,
                expected,
                interrupted: if claude { 2 } else { 3 },
            },
            repo,
        )
    }

    fn kind(&self) -> &'static str {
        if self.claude {
            "claude_transcript"
        } else {
            "codex_rollout"
        }
    }
    fn card(&self) -> &str {
        self.seed.card.id.as_str()
    }
    fn spawn(
        &self,
        repo: Arc<SqlxRepo>,
    ) -> (
        CancellationToken,
        tokio::task::JoinHandle<Result<(), calm_types::error::CoreError>>,
    ) {
        if self.claude {
            wf::spawn_claude_source_with_path(
                repo,
                self.seed.runtime.clone(),
                &self.seed,
                &self.path,
            )
        } else {
            wf::spawn_source_with_path(repo, self.seed.runtime.clone(), &self.seed, &self.path)
        }
    }
    async fn wait(&self, repo: &SqlxRepo) {
        wf::wait_until(wf::LIVENESS_BUDGET, || async {
            repo.worker_flow_cursor_get(self.card(), self.kind())
                .await
                .unwrap()
                .is_some_and(|c| c.record_index == self.lines.len() as i64)
        })
        .await;
    }
    async fn assert_rows(&self, repo: &SqlxRepo) -> Vec<(i64, String)> {
        let rows = repo
            .worker_flow_item_list_by_card(self.card(), 0, 1000, false)
            .await
            .unwrap();
        let payloads: Vec<Value> = rows
            .iter()
            .map(|r| serde_json::from_str(&r.payload).unwrap())
            .collect();
        assert_eq!(
            payloads, self.expected,
            "every source item exactly once, full payload and order"
        );
        for row in &rows {
            assert_eq!(row.card_id.as_deref(), Some(self.card()));
            assert_eq!(
                row.track_id.as_deref(),
                Some(self.seed.card.track_id.as_str())
            );
            assert_eq!(
                row.worker_session_id.as_deref(),
                Some(self.seed.runtime.id.as_str())
            );
            assert_eq!(
                row.captured_session_id.as_deref(),
                Some(self.seed.runtime.id.as_str())
            );
            assert_eq!(
                row.kind,
                serde_json::from_str::<Value>(&row.payload).unwrap()["type"]
                    .as_str()
                    .unwrap()
            );
        }
        let cursor = repo
            .worker_flow_cursor_get(self.card(), self.kind())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cursor.source_path, self.path.to_string_lossy());
        assert_eq!(cursor.record_index, self.lines.len() as i64);
        assert_eq!(
            cursor.byte_offset,
            if self.claude {
                std::fs::metadata(&self.path).unwrap().len() as i64
            } else {
                0
            }
        );
        assert_eq!(cursor.last_source_uuid.as_deref(), Some("after"));
        let raw = serde_json::to_string(self.lines.last().unwrap()).unwrap();
        assert_eq!(
            cursor.last_line_hash.as_deref(),
            Some(&blake3::hash(raw.as_bytes()).to_hex()[..16])
        );
        rows.into_iter().map(|r| (r.id, r.payload)).collect()
    }
    async fn recover(&self, repo: Arc<SqlxRepo>) {
        let (token, task) = self.spawn(repo.clone());
        self.wait(&repo).await;
        token.cancel();
        task.await.unwrap().unwrap();
        let first = self.assert_rows(&repo).await;
        repo.pool().close().await;
        drop(repo);
        let repo = open(self.dir.path()).await;
        let idle = install(self.card(), self.lines.len() as i64, CapturePoint::Idle);
        let (token, task) = self.spawn(repo.clone());
        tokio::time::timeout(wf::LIVENESS_BUDGET, idle.entered.notified())
            .await
            .unwrap();
        idle.release.notify_one();
        token.cancel();
        task.await.unwrap().unwrap();
        assert_eq!(
            self.assert_rows(&repo).await,
            first,
            "second recovery keeps row ids/payloads"
        );
        repo.pool().close().await;
    }
}

async fn open(dir: &std::path::Path) -> Arc<SqlxRepo> {
    Arc::new(
        SqlxRepo::open(&format!(
            "sqlite://{}?mode=rwc",
            dir.join("truth.db").display()
        ))
        .await
        .unwrap(),
    )
}

async fn crash(claude: bool, multi: bool, card: &str) {
    let (case, repo) = Case::new(claude, multi, card).await;
    let pause = install(case.card(), case.interrupted, CapturePoint::ItemInserted);
    let (_, task) = case.spawn(repo.clone());
    tokio::time::timeout(wf::LIVENESS_BUDGET, pause.entered.notified())
        .await
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    repo.pool().close().await;
    drop(repo);
    case.recover(open(case.dir.path()).await).await;
}

#[tokio::test]
async fn capture_atomicity_codex_crash_before_checkpoint_no_duplicates_or_loss() {
    crash(false, false, "atomic-codex-crash").await;
}
#[tokio::test]
async fn capture_atomicity_claude_crash_before_checkpoint_no_duplicates_or_loss() {
    crash(true, false, "atomic-claude-crash").await;
}
#[tokio::test]
async fn capture_atomicity_claude_partial_record_crash_no_duplicates_or_loss() {
    crash(true, true, "atomic-claude-multi-crash").await;
}

async fn cursor_error(claude: bool, card: &str) {
    let (case, repo) = Case::new(claude, false, card).await;
    sqlx::query(&format!(
        r#"CREATE TRIGGER fail_capture_cursor BEFORE UPDATE ON worker_flow_cursors
           WHEN NEW.record_index = {}
           BEGIN SELECT RAISE(ABORT, 'capture cursor rejected'); END"#,
        case.interrupted,
    ))
    .execute(repo.pool())
    .await
    .unwrap();
    let (_, task) = case.spawn(repo.clone());
    let err = tokio::time::timeout(wf::LIVENESS_BUDGET, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(err.to_string().contains("capture cursor rejected"), "{err}");
    assert_eq!(
        repo.worker_flow_item_list_by_card(case.card(), 0, 1000, false)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        repo.worker_flow_cursor_get(case.card(), case.kind())
            .await
            .unwrap()
            .unwrap()
            .record_index,
        case.interrupted - 1
    );
    sqlx::query("DROP TRIGGER fail_capture_cursor")
        .execute(repo.pool())
        .await
        .unwrap();
    repo.pool().close().await;
    drop(repo);
    case.recover(open(case.dir.path()).await).await;
}

#[tokio::test]
async fn capture_atomicity_codex_cursor_error_restart_no_duplicates_or_loss() {
    cursor_error(false, "atomic-codex-sql").await;
}
#[tokio::test]
async fn capture_atomicity_claude_cursor_error_restart_no_duplicates_or_loss() {
    cursor_error(true, "atomic-claude-sql").await;
}

async fn ack_lost(claude: bool, card: &str) {
    let (case, repo) = Case::new(claude, false, card).await;
    let pause = install(case.card(), case.interrupted, CapturePoint::Committed);
    let (_, task) = case.spawn(repo.clone());
    tokio::time::timeout(wf::LIVENESS_BUDGET, pause.entered.notified())
        .await
        .unwrap();
    assert_eq!(
        repo.worker_flow_cursor_get(case.card(), case.kind())
            .await
            .unwrap()
            .unwrap()
            .record_index,
        case.interrupted
    );
    assert_eq!(
        repo.worker_flow_item_list_by_card(case.card(), 0, 1000, false)
            .await
            .unwrap()
            .len(),
        2
    );
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    repo.pool().close().await;
    drop(repo);
    case.recover(open(case.dir.path()).await).await;
}
#[tokio::test]
async fn capture_atomicity_codex_commit_ack_lost_restart_no_duplicates_or_loss() {
    ack_lost(false, "atomic-codex-ack").await;
}
#[tokio::test]
async fn capture_atomicity_claude_commit_ack_lost_restart_no_duplicates_or_loss() {
    ack_lost(true, "atomic-claude-ack").await;
}

async fn busy_cancel(claude: bool, card: &str) {
    use sqlx::Connection;
    let (case, repo) = Case::new(claude, false, card).await;
    let before = install(
        case.card(),
        case.interrupted,
        CapturePoint::BeforeTransaction,
    );
    let busy = install(case.card(), case.interrupted, CapturePoint::Busy);
    let (token, task) = case.spawn(repo.clone());
    tokio::time::timeout(wf::LIVENESS_BUDGET, before.entered.notified())
        .await
        .unwrap();
    let url = format!("sqlite://{}", case.dir.path().join("truth.db").display());
    let mut locker = sqlx::SqliteConnection::connect(&url).await.unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut locker)
        .await
        .unwrap();
    before.release.notify_one();
    tokio::time::timeout(wf::LIVENESS_BUDGET, busy.entered.notified())
        .await
        .unwrap();
    token.cancel();
    tokio::time::timeout(wf::LIVENESS_BUDGET, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        repo.worker_flow_cursor_get(case.card(), case.kind())
            .await
            .unwrap()
            .unwrap()
            .record_index,
        case.interrupted - 1
    );
    assert_eq!(
        repo.worker_flow_item_list_by_card(case.card(), 0, 1000, false)
            .await
            .unwrap()
            .len(),
        1
    );
    sqlx::query("ROLLBACK").execute(&mut locker).await.unwrap();
    drop(locker);
    repo.pool().close().await;
    drop(repo);
    case.recover(open(case.dir.path()).await).await;
}
#[tokio::test]
async fn capture_atomicity_codex_busy_cancel_restart_no_duplicates_or_loss() {
    busy_cancel(false, "atomic-codex-busy").await;
}
#[tokio::test]
async fn capture_atomicity_claude_busy_cancel_restart_no_duplicates_or_loss() {
    busy_cancel(true, "atomic-claude-busy").await;
}

async fn stale(claude: bool, card: &str) {
    let (case, repo) = Case::new(claude, false, card).await;
    let before = install(
        case.card(),
        case.interrupted,
        CapturePoint::BeforeTransaction,
    );
    let (_, old) = case.spawn(repo.clone());
    tokio::time::timeout(wf::LIVENESS_BUDGET, before.entered.notified())
        .await
        .unwrap();
    let (token, new) = case.spawn(repo.clone());
    case.wait(&repo).await;
    token.cancel();
    new.await.unwrap().unwrap();
    let rows = case.assert_rows(&repo).await;
    let checkpoint = repo
        .worker_flow_cursor_get(case.card(), case.kind())
        .await
        .unwrap();
    before.release.notify_one();
    tokio::time::timeout(wf::LIVENESS_BUDGET, old)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(case.assert_rows(&repo).await, rows);
    assert_eq!(
        repo.worker_flow_cursor_get(case.card(), case.kind())
            .await
            .unwrap(),
        checkpoint
    );
    repo.pool().close().await;
    drop(repo);
    case.recover(open(case.dir.path()).await).await;
}
#[tokio::test]
async fn capture_atomicity_codex_stale_source_cannot_overwrite_replacement_checkpoint() {
    stale(false, "atomic-codex-stale").await;
}
#[tokio::test]
async fn capture_atomicity_claude_stale_source_cannot_overwrite_replacement_checkpoint() {
    stale(true, "atomic-claude-stale").await;
}

async fn boot(claude: bool, card: &str) {
    use calm_server::event::EventBus;
    use calm_server::shared_codex_appserver::SharedCodexAppServer;
    use calm_server::worker_flow::WorkerFlowDriver;
    use calm_server::worker_flow::claude_transcript::ClaudeTranscriptFlowSourceOptions;
    use calm_server::worker_flow::codex_rollout::CodexRolloutFlowSourceOptions;
    use calm_truth::worker_flow_sink::WorkerFlowSink;
    let (case, repo) = Case::new(claude, false, card).await;
    let pause = install(case.card(), case.interrupted, CapturePoint::ItemInserted);
    let (_, task) = case.spawn(repo.clone());
    tokio::time::timeout(wf::LIVENESS_BUDGET, pause.entered.notified())
        .await
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    repo.pool().close().await;
    drop(repo);
    let repo = open(case.dir.path()).await;
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
    driver.start_on_boot().await.unwrap();
    case.wait(&repo).await;
    driver.stop_and_join_for_test().await;
    case.assert_rows(&repo).await;
    drop(driver);
    repo.pool().close().await;
    drop(repo);
    case.recover(open(case.dir.path()).await).await;
}
#[tokio::test]
async fn capture_atomicity_codex_boot_restart_preserves_every_record() {
    boot(false, "atomic-codex-boot").await;
}
#[tokio::test]
async fn capture_atomicity_claude_boot_restart_preserves_every_record() {
    boot(true, "atomic-claude-boot").await;
}

#[tokio::test]
async fn capture_atomicity_claude_reconstructs_pending_tool_after_committed_batch() {
    let (mut case, repo) = Case::new(true, true, "atomic-claude-tool-state").await;
    case.lines[1]["message"]["content"]
        .as_array_mut()
        .unwrap()
        .push(wf::claude_tool_use(
            "tool-mid",
            "mcp__capture__lookup",
            json!({"key":"x"}),
        ));
    let mut result = wf::claude_user_blocks(
        "result",
        vec![wf::claude_tool_result("tool-mid", "done", false)],
    );
    result["timestamp"] = json!("1970-01-01T00:00:01Z");
    case.lines.insert(2, result);
    wf::write_rollout(&case.path, &case.lines);
    let mut pending = case.expected[1].clone();
    for key in ["text", "is_final", "phase"] {
        pending.as_object_mut().unwrap().remove(key);
    }
    pending["type"] = json!("mcpToolCall");
    pending["seq"] = json!(3);
    for (key, value) in [
        ("call_id", json!("tool-mid")),
        ("server", json!("capture")),
        ("tool", json!("lookup")),
        ("arguments", json!({"key":"x"})),
        ("status", json!("inProgress")),
        ("result", Value::Null),
        ("error", Value::Null),
        ("duration_ms", Value::Null),
    ] {
        pending[key] = value;
    }
    let mut completed = pending.clone();
    completed["seq"] = json!(4);
    completed["status"] = json!("completed");
    completed["source_uuid"] = json!("result");
    completed["raw_ref"]["line"] = json!(2);
    completed["raw_ref"]["record_type"] = json!("user");
    completed["result"] = json!("done");
    case.expected[3]["seq"] = json!(5);
    case.expected[3]["raw_ref"]["line"] = json!(3);
    case.expected.insert(3, pending);
    case.expected.insert(4, completed);
    let ack = install(case.card(), case.interrupted, CapturePoint::Committed);
    let (_, task) = case.spawn(repo.clone());
    tokio::time::timeout(wf::LIVENESS_BUDGET, ack.entered.notified())
        .await
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    repo.pool().close().await;
    drop(repo);
    case.recover(open(case.dir.path()).await).await;
}

async fn empty_records(claude: bool, card: &str) {
    let (case, repo) = Case::new(claude, false, card).await;
    case.recover(repo).await;
    let repo = open(case.dir.path()).await;
    // Both a malformed record and a valid metadata/empty record have no items.
    let empty = if claude {
        json!({"type":"assistant", "uuid":"empty", "message":{"content":[]}})
    } else {
        wf::turn_context("empty-turn")
    };
    let raw = serde_json::to_string(&empty).unwrap();
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&case.path)
        .unwrap();
    writeln!(file, "not-json\n{raw}").unwrap();
    drop(file);
    let next = case.lines.len() as i64 + 2;
    let before = repo
        .worker_flow_item_list_by_card(case.card(), 0, 1000, false)
        .await
        .unwrap();
    let (token, task) = case.spawn(repo.clone());
    wf::wait_until(wf::LIVENESS_BUDGET, || async {
        repo.worker_flow_cursor_get(case.card(), case.kind())
            .await
            .unwrap()
            .is_some_and(|c| c.record_index == next)
    })
    .await;
    token.cancel();
    task.await.unwrap().unwrap();
    let cursor = repo
        .worker_flow_cursor_get(case.card(), case.kind())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        cursor.last_line_hash.as_deref(),
        Some(&blake3::hash(raw.as_bytes()).to_hex()[..16])
    );
    assert_eq!(
        cursor.last_source_uuid.as_deref(),
        if claude { Some("empty") } else { None }
    );
    assert_eq!(
        cursor.byte_offset,
        if claude {
            std::fs::metadata(&case.path).unwrap().len() as i64
        } else {
            0
        }
    );
    let after = repo
        .worker_flow_item_list_by_card(case.card(), 0, 1000, false)
        .await
        .unwrap();
    assert_eq!(
        before
            .iter()
            .map(|r| (r.id, &r.payload))
            .collect::<Vec<_>>(),
        after.iter().map(|r| (r.id, &r.payload)).collect::<Vec<_>>()
    );
    repo.pool().close().await;
    drop(repo);
    let repo = open(case.dir.path()).await;
    let idle = install(case.card(), next, CapturePoint::Idle);
    let (token, task) = case.spawn(repo.clone());
    tokio::time::timeout(wf::LIVENESS_BUDGET, idle.entered.notified())
        .await
        .unwrap();
    idle.release.notify_one();
    token.cancel();
    task.await.unwrap().unwrap();
    assert_eq!(
        repo.worker_flow_cursor_get(case.card(), case.kind())
            .await
            .unwrap(),
        Some(cursor)
    );
    let again = repo
        .worker_flow_item_list_by_card(case.card(), 0, 1000, false)
        .await
        .unwrap();
    assert_eq!(
        after.iter().map(|r| (r.id, &r.payload)).collect::<Vec<_>>(),
        again.iter().map(|r| (r.id, &r.payload)).collect::<Vec<_>>()
    );
    repo.pool().close().await;
}
#[tokio::test]
async fn capture_atomicity_codex_empty_records_advance_checkpoint_and_idle_is_stable() {
    empty_records(false, "atomic-codex-empty").await;
}
#[tokio::test]
async fn capture_atomicity_claude_empty_records_advance_checkpoint_and_idle_is_stable() {
    empty_records(true, "atomic-claude-empty").await;
}
