//! C1a exercises the real clean pass and each preparation owner, without provider E2E.
use super::*;
use calm_server::operation::{
    OperationKey, OperationRepo, Phase, RecoveryItem, RecoveryPlan, SqlxOperationRepo,
};
use calm_server::test_seams::{PausePoint, install_pause_for_test};
use calm_truth::db::RepoRead;
use std::{collections::HashSet, time::Duration};

struct Pause(PausePoint);
impl Pause {
    fn arm(point: &str, track: &str) -> Self {
        let pause = PausePoint {
            entered: Arc::default(),
            release: Arc::default(),
        };
        install_pause_for_test(point, track, pause.clone());
        Self(pause)
    }
    async fn entered(&self) {
        tokio::time::timeout(Duration::from_secs(15), self.0.entered.notified())
            .await
            .unwrap();
    }
}
impl Drop for Pause {
    fn drop(&mut self) {
        self.0.release.notify_one();
    }
}
async fn fixture() -> (Arc<Boot>, String, PathBuf) {
    let b = Arc::new(boot().await);
    let up = upstream(b.tmp.path());
    let track = b
        .historical_at(&up.clone, "c1-history", "prepare historical fence")
        .await
        .unwrap();
    // Settle the real historical first turn before making a closed-idle fixture.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let turns = b.state.shared_codex_appserver.started_turns_for_test();
            if let Some((thread, _)) = turns.first()
                && let Some(turn) = b.state.shared_codex_appserver.active_turn_for_test(thread) {
                b.state.shared_codex_appserver.emit_notification_for_test(
                    calm_server::codex_appserver::Notification::TurnCompleted {
                        thread_id:thread.clone(), turn:json!({"id":turn,"status":"completed"}),
                    });
            }
            let busy: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM worker_sessions WHERE track_id=? AND state IN ('starting','running','turn_pending')")
                .bind(&track).fetch_one(b.repo.pool()).await.unwrap();
            if !turns.is_empty() && busy == 0 { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("historical first turn settles before closed-idle clean");
    b.shutdown_harnesses().await;
    let path = expected_worktree(&up.clone, &track);
    run_git(&path, ["config", "core.excludesFile", "/dev/null"]);
    std::fs::write(up.clone.join(".git/info/exclude"), "target/\n").unwrap();
    std::fs::create_dir_all(path.join("target")).unwrap();
    std::fs::write(path.join("target/ignored"), "build output").unwrap();
    sqlx::query("UPDATE tracks SET closed_at=123 WHERE id=?")
        .bind(&track)
        .execute(b.repo.pool())
        .await
        .unwrap();
    (b, track, path)
}
fn clean(b: Arc<Boot>) -> tokio::task::JoinHandle<(usize, HashSet<(String, i64)>)> {
    tokio::spawn(async move {
        let mut history = HashSet::new();
        let count = calm_server::test_seams::clean_closed_worktrees_with_history_for_test(
            b.repo.db(),
            &b.state.operation_runtime,
            &mut history,
        )
        .await
        .unwrap();
        (count, history)
    })
}
fn terminal(track: &str) -> Value {
    json!({"actor":calm_server::ids::ActorId::Kernel, "source":"manual", "track_id":track,
        "program":"/bin/true", "cwd":"", "env":{},
        "theme":{"fg":[255,255,255],"bg":[0,0,0]}})
}
async fn pending(b: &Boot, kind: &str, payload: Value) -> String {
    SqlxOperationRepo::new(b.repo.db().clone())
        .insert_operation(
            kind,
            OperationKey {
                operation_key: calm_server::model::new_id(),
                idempotency_key: None,
                payload_hash: "c1".into(),
            },
            payload,
        )
        .await
        .unwrap()
}
async fn phase(b: &Boot, op: &str) -> String {
    sqlx::query_scalar("SELECT phase FROM operations WHERE id=?")
        .bind(op)
        .fetch_one(b.repo.pool())
        .await
        .unwrap()
}
#[tokio::test]
async fn c1_fence_sample_then_reopen_and_launch_refuses_without_cleaned() {
    let (b, track, path) = fixture().await;
    let pause = Pause::arm("c1-clean-sampled", &track);
    let cleaning = clean(b.clone());
    pause.entered().await;
    let (status, body) = b
        .as_user(
            "PATCH",
            &format!("/api/tracks/{track}"),
            json!({"closed":false}),
        )
        .await;
    assert!(status.is_success(), "{body}");
    let op = pending(&b, "terminal-create", terminal(&track)).await;
    b.state
        .operation_runtime
        .prepare_once_for_test(&op, false)
        .await
        .unwrap();
    assert_eq!(phase(&b, &op).await, "tx_committed");
    drop(pause);
    let (count, history) = cleaning.await.unwrap();
    assert_eq!(count, 0);
    assert!(history.is_empty());
    assert!(path.join("target/ignored").exists());
}
#[tokio::test]
async fn c1_fence_reopen_waits_for_clean() {
    let (b, track, path) = fixture().await;
    let pause = Pause::arm("c1-clean-fenced", &track);
    let cleaning = clean(b.clone());
    pause.entered().await;
    let endpoint = format!("/api/tracks/{track}");
    let arrival = Pause::arm("key-lock-waiting", &track);
    let mut reopening = Box::pin(b.as_user("PATCH", &endpoint, json!({"closed":false})));
    tokio::select! {
        result = &mut reopening => panic!("reopen passed held track: {result:?}"),
        _ = arrival.entered() => {}
    }
    let closed: Option<i64> = sqlx::query_scalar("SELECT closed_at FROM tracks WHERE id=?")
        .bind(&track)
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(closed, Some(123));
    drop(arrival);
    drop(pause);
    assert_eq!(cleaning.await.unwrap().0, 1);
    assert!(!path.join("target/ignored").exists());
    assert!(reopening.await.0.is_success());
}
async fn preparation(kind: &str, boot: bool) {
    let (b, track, path) = fixture().await;
    // A new terminal task prepares only an op-owned Pipe execution, so it needs a live Engine.
    let _execution = if kind == "terminal-worker" {
        let execution = crate::support::execution::Fixture::new(b.repo.db().clone()).await;
        b.state
            .operation_runtime
            .execution()
            .bind(Arc::new(execution.lifecycle.engine().unwrap().clone()));
        Some(execution)
    } else {
        None
    };
    let payload = match kind {
        "terminal-create" => terminal(&track),
        "planner-harness-start" => sqlx::query_scalar::<_, String>(
            "SELECT payload_json FROM operations WHERE kind='planner-harness-start' \
             AND target_id IN (SELECT id FROM cards WHERE track_id=?) LIMIT 1",
        )
        .bind(&track)
        .fetch_one(b.repo.pool())
        .await
        .map(|s| serde_json::from_str(&s).unwrap())
        .unwrap(),
        _ => {
            let task = format!("{track}:c1");
            sqlx::query("INSERT INTO tasks (id,track_id,key,kind,goal,context_json,depends_on_json,\
                 status,created_at_ms,updated_at_ms) VALUES (?,?,'c1','terminal','test','null','[]','dispatched',1,1)")
                .bind(&task).bind(&track).execute(b.repo.pool()).await.unwrap();
            json!({"actor":calm_server::ids::ActorId::KernelDispatcher,"track_id":track,"idempotency_key":task,"cmd":"true","goal":"test","cwd":path})
        }
    };
    // A dispatched task already prevents cleaning; reserve the task only after clean's recheck.
    sqlx::query("UPDATE tasks SET status='pending' WHERE track_id=?")
        .bind(&track)
        .execute(b.repo.pool())
        .await
        .unwrap();
    let pause = Pause::arm("c1-clean-fenced", &track);
    let cleaning = clean(b.clone());
    pause.entered().await;
    sqlx::query("UPDATE tasks SET status='dispatched' WHERE track_id=?")
        .bind(&track)
        .execute(b.repo.pool())
        .await
        .unwrap();
    let op = pending(&b, kind, payload).await;
    if boot {
        b.state
            .operation_runtime
            .apply_recovery(RecoveryPlan {
                items: vec![RecoveryItem::Recover {
                    op_id: op.clone(),
                    from_phase: Phase::Pending,
                    action: "prepare".into(),
                }],
            })
            .await
            .unwrap();
    } else {
        b.state
            .operation_runtime
            .prepare_once_for_test(&op, false)
            .await
            .unwrap();
    }
    assert_eq!(
        phase(&b, &op).await,
        "pending",
        "contention must remain retryable"
    );
    // Clean's filesystem work must not hold drive.
    let drive = tokio::time::timeout(
        Duration::from_secs(1),
        b.state.operation_runtime.lock_for_track_delete_for_test(),
    )
    .await
    .unwrap();
    drop(drive);
    drop(pause);
    assert_eq!(cleaning.await.unwrap().0, 1);
    // No HTTP waiter and no forced recovery claim: the Busy owner must wake drive.
    tokio::time::timeout(Duration::from_secs(5), async {
        while phase(&b, &op).await == "pending" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Busy operation must retry promptly after clean releases");
    assert_ne!(phase(&b, &op).await, "stuck");
    let output: Option<String> =
        sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE id=?")
            .bind(&op)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    let output: Value = serde_json::from_str(
        &output.expect("retry must commit preparation, not merely fail Pending"),
    )
    .unwrap();
    if kind == "terminal-worker" {
        assert!(
            output["data"].get("terminal_worker_pipe").is_some(),
            "retry must commit the Pipe preparation: {output}"
        );
    }
    if boot {
        tokio::time::timeout(Duration::from_secs(5), async {
            while phase(&b, &op).await != "succeeded" {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("boot terminal operation must actually start");
    }
}
#[tokio::test]
async fn c1_fence_terminal_create_busy_retries() {
    preparation("terminal-create", false).await;
}
#[tokio::test]
async fn c1_fence_terminal_prepare_busy_retries() {
    preparation("terminal-worker", false).await;
}
#[tokio::test]
async fn c1_fence_planner_start_busy_retries() {
    preparation("planner-harness-start", false).await;
}
#[tokio::test]
async fn c1_fence_boot_recovery_busy_retries() {
    preparation("terminal-create", true).await;
}

#[tokio::test]
async fn c1_fence_codex_worker_busy_retries() {
    preparation("codex-worker", false).await;
}
#[tokio::test]
async fn c1_fence_claude_worker_busy_retries() {
    preparation("claude-worker", false).await;
}

#[path = "c1_planner_fence.rs"]
mod planner;

#[tokio::test]
async fn c3_forge_action_after_clean_check_waits_for_clean() {
    use calm_server::operation::forge_action_adapter::{FORGE_ACTION_KIND, ForgeActionPayload};
    let (b, track, path) = fixture().await;
    let execution = crate::support::execution::Fixture::new(b.repo.db().clone()).await;
    b.state
        .operation_runtime
        .execution()
        .bind(Arc::new(execution.lifecycle.engine().unwrap().clone()));
    let pause = Pause::arm("c1-clean-fenced", &track);
    let cleaning = clean(b.clone());
    pause.entered().await;
    let released = path.join("forge-released");
    let payload = ForgeActionPayload {
        track_id: track.clone(),
        card_id: b.repo.cards_by_track(&track).await.unwrap()[0]
            .id
            .to_string(),
        subject: None,
        argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            ": > \"$1\"".into(),
            "forge".into(),
            released.display().to_string(),
        ],
        idem_key: "c3-clean".into(),
        event_spec: None,
        context: Default::default(),
        probe: None,
        cwd_lease: path.clone(),
        result_path: b.tmp.path().join("forge-result"),
        deadline_ms: calm_server::model::now_ms() + 60_000,
    };
    let op = SqlxOperationRepo::new(b.repo.db().clone())
        .insert_operation(
            FORGE_ACTION_KIND,
            OperationKey {
                operation_key: calm_server::model::new_id(),
                idempotency_key: Some(payload.idem_key.clone()),
                payload_hash: "c3-clean".into(),
            },
            serde_json::to_value(payload).unwrap(),
        )
        .await
        .unwrap();
    b.state.operation_runtime.drive().await.unwrap();
    let (phase_tag, admitted, spawned): (String, bool, bool) = sqlx::query_as(
        "SELECT phase, tx_output_json IS NOT NULL, spawn_artifacts_json IS NOT NULL FROM operations WHERE id=?"
    ).bind(&op).fetch_one(b.repo.pool()).await.unwrap();
    assert_eq!(phase_tag, "pending", "clean must fence forge preparation");
    assert!(!admitted, "no admission while clean holds track");
    assert!(!spawned, "no native spawn while clean holds track");
    assert!(!released.exists(), "no go-token release during clean");
    assert!(path.join("target/ignored").exists());
    drop(pause);
    assert_eq!(cleaning.await.unwrap().0, 1);
    assert!(!path.join("target/ignored").exists());
    tokio::time::timeout(Duration::from_secs(10), async {
        while phase(&b, &op).await != "succeeded" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Busy retry must complete forge after clean");
    assert!(released.exists());
}

#[tokio::test]
async fn c5_location_clean_foreign_and_malformed_roots_defer() {
    for root_kind in 0..4 {
        let (b, _track, path) = fixture().await;
        let source = match root_kind {
            0 => json!({"root": path}),
            1 => json!({}),
            2 => json!({"root":null}),
            _ => json!({"root":42}),
        };
        let id = calm_server::model::new_id();
        sqlx::query("INSERT INTO workspace_executions \
            (execution_id,track_id,alias,intent_json,secrets_json,desired) VALUES (?,?,?,?,?,'run')")
            .bind(&id).bind("foreign-owner").bind(format!("foreign:{id}"))
            .bind(json!({"source":source}).to_string()).bind("{}").execute(b.repo.pool()).await.unwrap();
        let (count, history) = clean(b.clone()).await.unwrap();
        assert_eq!(count, 0);
        assert!(history.is_empty());
        assert!(path.join("target/ignored").is_file());
        let desired: String =
            sqlx::query_scalar("SELECT desired FROM workspace_executions WHERE execution_id=?")
                .bind(&id)
                .fetch_one(b.repo.pool())
                .await
                .unwrap();
        assert_eq!(desired, "run");
    }
}

#[tokio::test]
async fn c5_location_foreign_worktree_delete_preserves_branch_and_rows() {
    let (b, track, path) = fixture().await;
    let id = calm_server::model::new_id();
    sqlx::query("INSERT INTO workspace_executions \
            (execution_id,track_id,alias,intent_json,secrets_json,desired) VALUES (?,?,?,?,?,'run')")
        .bind(&id).bind("foreign-owner").bind(format!("foreign:{id}"))
        .bind(json!({"source":{"root":path}}).to_string()).bind("{}").execute(b.repo.pool()).await.unwrap();
    let (status, body) = b.delete_track(&track).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(path.join("target/ignored").is_file());
    assert!(b.repo.track_get(&track).await.unwrap().is_some());
    assert!(git_ref_exists(
        &path,
        &format!("refs/heads/neige/track-{track}")
    ));
    let desired: String =
        sqlx::query_scalar("SELECT desired FROM workspace_executions WHERE execution_id=?")
            .bind(&id)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(desired, "run");
}
