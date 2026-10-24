//! Report → REST approval → Planner MCP → projector → real runtime and supervisor.
use super::objective_approval::{define, definition, request, setup_on};
use super::*;
use calm_server::operation::objective_check_run::{KIND, ObjectiveCheckRunAdapter};
use calm_server::operation::{
    OperationCompletionBus, OperationRuntime, ProviderAdapter, SpawnCtx, SqlxOperationRepo,
};
use calm_truth::objective_host;
use std::path::Path;
use std::time::Duration;

struct World {
    boot: Boot,
    block: Value,
    runtime: Arc<OperationRuntime>,
    repo: PathBuf,
    revision: String,
    checks: PathBuf,
    _execution: crate::support::execution::Fixture,
    _tmp: tempfile::TempDir,
}
fn git(repo: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}
fn commit(repo: &Path, marker: &str) -> String {
    std::fs::write(repo.join("marker"), marker).unwrap();
    git(repo, &["add", "marker"]);
    git(repo, &["commit", "-qm", "marker"]);
    git(repo, &["rev-parse", "HEAD"])
}
async fn world(
    command: &str,
    hook: impl FnOnce(ObjectiveCheckRunAdapter) -> ObjectiveCheckRunAdapter,
) -> World {
    world_with_readiness(command, hook, true).await
}
async fn world_with_readiness(
    command: &str,
    hook: impl FnOnce(ObjectiveCheckRunAdapter) -> ObjectiveCheckRunAdapter,
    bound: bool,
) -> World {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "objective@example.test"]);
    git(&repo, &["config", "user.name", "Objective Test"]);
    let revision = commit(&repo, "1");
    let (boot, block) = setup_on(boot_with_workspace(false, repo.to_str().unwrap()).await).await;
    let db = boot.repo.sqlite_db().unwrap();
    let mut d = definition();
    d["mode"] = json!({"loop":{"plateau":10}});
    d["checks"][0]["role"] = json!("ratchet");
    d["checks"][0]["config"] = json!({"kind":"command","params":{"command":command}});
    define(&boot, Some(block.clone()), d).await;
    let execution = crate::support::execution::Fixture::new(db.clone()).await;
    let checks = tmp.path().join("objective-checks");
    let adapter = hook(ObjectiveCheckRunAdapter::new(
        checks.clone(),
        tmp.path().join("logs"),
    ));
    let runtime = operation_runtime(
        &boot,
        adapter,
        if bound {
            execution.lifecycle.clone()
        } else {
            Default::default()
        },
    );
    assert!(boot.ctx.operation_runtime.set(runtime.clone()).is_ok());
    World {
        boot,
        block,
        runtime,
        repo,
        revision,
        checks,
        _execution: execution,
        _tmp: tmp,
    }
}
fn operation_runtime(
    boot: &Boot,
    adapter: ObjectiveCheckRunAdapter,
    execution: calm_server::execution_quiesce::ExecutionLifecycle,
) -> Arc<OperationRuntime> {
    let op_repo = Arc::new(SqlxOperationRepo::new(boot.repo.sqlite_db().unwrap()));
    let completion = OperationCompletionBus::new();
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = boot.repo.clone();
    Arc::new(OperationRuntime::new_unchecked(
        op_repo.clone(),
        vec![
            Arc::new(adapter) as Arc<dyn ProviderAdapter>,
            Arc::new(calm_server::operation::track_recipe_create_adapter::TrackRecipeCreateAdapter),
        ],
        boot.events.clone(),
        completion.clone(),
        SpawnCtx::new(
            route_repo.clone(),
            op_repo,
            Arc::new(DaemonClient::new_stub()),
            calm_server::terminal_renderer::TerminalRendererRegistry::new_with_repo(route_repo),
            boot.events.clone(),
            completion,
        )
        .with_execution(execution),
    ))
}
async fn approve(w: &World) {
    let id = request(&w.boot).await["ask_id"].as_i64().unwrap();
    assert_eq!(
        answer(&w.boot, id, json!([{"option":0}])).await.0,
        StatusCode::NO_CONTENT
    );
}
async fn evaluate(
    w: &World,
    revision: &str,
) -> Result<Value, calm_server::plugin_host::mcp::RpcError> {
    call_tool(
        &w.boot,
        "neige_objective_evaluate",
        json!({"key":"kr", "revision":revision}),
    )
    .await
}
async fn project(w: &World) {
    let runtime = Arc::new(tokio::sync::OnceCell::new());
    runtime
        .set(w.runtime.clone())
        .unwrap_or_else(|_| panic!("fresh runtime cell"));
    calm_server::track_planner_context::TrackPlannerContextProjector::new(
        w.boot.repo.clone(),
        w.boot.events.clone(),
        w.boot.ctx.write.clone(),
    )
    .unwrap()
    .with_operation_runtime(runtime)
    .recompute_track(w.boot.track_id.as_str())
    .await
    .unwrap();
}
async fn state(w: &World) -> objective::State {
    objective_host::load_state(&w.boot.repo.sqlite_db().unwrap(), &w.boot.track_id)
        .await
        .unwrap()
        .1
}
async fn count(w: &World, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM objective_events WHERE kind=?")
        .bind(kind)
        .fetch_one(w.boot.repo.sqlite_db().unwrap().read_pool())
        .await
        .unwrap()
}
async fn phases(w: &World) -> Vec<String> {
    sqlx::query_scalar("SELECT phase FROM operations WHERE kind=? ORDER BY created_at_ms,id")
        .bind(KIND)
        .fetch_all(w.boot.repo.sqlite_db().unwrap().read_pool())
        .await
        .unwrap()
}
async fn ready(w: &World) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            project(w).await;
            if matches!(
                state(w).await.objectives[&objective::ids::ObjectiveKey::new("kr")].evaluation,
                Some(objective::engine::Evaluation::Ready { .. })
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("command did not finish"));
}
async fn settled(w: &World) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            project(w).await;
            let active = objective_host::unsettled_intents(&w.boot.repo.sqlite_db().unwrap())
                .await
                .unwrap();
            if active.is_empty()
                && phases(w)
                    .await
                    .iter()
                    .all(|p| p == "succeeded" || p == "failed")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("command intents settle");
}
type Hook = Arc<dyn Fn() -> futures::future::BoxFuture<'static, ()> + Send + Sync>;
fn pause() -> (Hook, Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
    let entered = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    let (e, r) = (entered.clone(), resume.clone());
    (
        Arc::new(move || {
            let (e, r) = (e.clone(), r.clone());
            Box::pin(async move {
                e.notify_one();
                r.notified().await;
            })
        }),
        entered,
        resume,
    )
}

#[tokio::test]
async fn objective_command_revision_and_cleanup() {
    let w = world("test \"$(cat marker)\" = 1", |a| a).await;
    commit(&w.repo, "2");
    approve(&w).await;
    evaluate(&w, &w.revision).await.unwrap();
    ready(&w).await;
    assert_eq!(count(&w, "check_finished").await, 1);
    let s = state(&w).await;
    let objective::engine::Evaluation::Ready { results, .. } = s.objectives
        [&objective::ids::ObjectiveKey::new("kr")]
        .evaluation
        .as_ref()
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        results[&objective::ids::CheckId::new("accepted")],
        objective::check::CheckOutput::Command(objective::check::CommandOutput { exit_code: 0 })
    );
    settled(&w).await;
    project(&w).await; // Await cleanup after observing all operations terminal.
    let outputs: Vec<String> =
        sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE kind=?")
            .bind(KIND)
            .fetch_all(w.boot.repo.sqlite_db().unwrap().read_pool())
            .await
            .unwrap();
    for output in outputs {
        let output: Value = serde_json::from_str(&output).unwrap();
        assert!(!Path::new(output["data"]["cwd"].as_str().unwrap()).exists());
    }
    assert!(
        !git(&w.repo, &["worktree", "list", "--porcelain"]).contains(w.checks.to_str().unwrap())
    );
}

#[tokio::test]
async fn objective_command_approval_and_planner_fences() {
    let w = world("true", |a| a).await;
    assert!(
        evaluate(&w, &w.revision)
            .await
            .unwrap_err()
            .message
            .contains("approval_required")
    );
    assert!(phases(&w).await.is_empty());
    approve(&w).await;
    crate::support::mcp::set_persisted_card_role(
        w.boot.repo.as_ref(),
        w.boot.planner_card_id.as_str(),
        CardRole::Worker,
    )
    .await;
    assert_eq!(evaluate(&w, &w.revision).await.unwrap_err().code, -32403);
    assert!(phases(&w).await.is_empty());
}

#[tokio::test]
async fn objective_command_nonzero_regression() {
    let w = world("test \"$(cat marker)\" = 1", |a| a).await;
    approve(&w).await;
    evaluate(&w, &w.revision).await.unwrap();
    ready(&w).await;
    call_tool(
        &w.boot,
        "neige_objective_settle",
        json!({"key":"kr","evaluation":0,"decision":"keep","note":"baseline"}),
    )
    .await
    .unwrap();
    let next = commit(&w.repo, "2");
    evaluate(&w, &next).await.unwrap();
    ready(&w).await;
    call_tool(
        &w.boot,
        "neige_objective_settle",
        json!({"key":"kr","evaluation":1,"decision":"keep","note":"refuse"}),
    )
    .await
    .unwrap();
    assert_eq!(count(&w, "check_finished").await, 2);
    assert_eq!(
        state(&w).await.objectives[&objective::ids::ObjectiveKey::new("kr")]
            .ledger
            .entries
            .len(),
        1
    );
    project(&w).await;
    assert!(
        rows(&w.boot, "track.wake_requested")
            .await
            .iter()
            .any(|(_, r)| r.payload_value()["text"]
                .as_str()
                .unwrap()
                .contains("SettleRefused"))
    );
}

#[tokio::test]
async fn objective_command_environment_allowlist() {
    // nextest owns one process per test. Set before starting any execution threads.
    unsafe {
        std::env::set_var("NEIGE_TEST_SENTINEL", "must-not-reach-command");
    }
    let w = world(
        "test -n \"$PATH\" && test -z \"${NEIGE_TEST_SENTINEL+x}\"",
        |a| a,
    )
    .await;
    approve(&w).await;
    evaluate(&w, &w.revision).await.unwrap();
    ready(&w).await;
    let s = state(&w).await;
    let objective::engine::Evaluation::Ready { results, .. } = s.objectives
        [&objective::ids::ObjectiveKey::new("kr")]
        .evaluation
        .as_ref()
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        results[&objective::ids::CheckId::new("accepted")],
        objective::check::CheckOutput::Command(objective::check::CommandOutput { exit_code: 0 })
    );
}

#[tokio::test]
async fn objective_command_release_rechecks_standard() {
    let (hook, entered, resume) = pause();
    let w = world("true", |a| a.with_before_release(hook)).await;
    approve(&w).await;
    evaluate(&w, &w.revision).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    let mut d = definition();
    d["checks"][0]["config"] = json!({"kind":"command","params":{"command":"false"}});
    define(&w.boot, Some(w.block.clone()), d).await;
    resume.notify_one();
    settled(&w).await;
    assert_eq!(count(&w, "check_finished").await, 0);
    let records: Vec<String> =
        sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE kind=?")
            .bind(KIND)
            .fetch_all(w.boot.repo.sqlite_db().unwrap().read_pool())
            .await
            .unwrap();
    assert_eq!(records.len(), 1);
    let record: Value = serde_json::from_str(&records[0]).unwrap();
    assert_eq!(
        record["data"]["gate_execution"]["decision"]["Stopping"]["reason"],
        "ReleaseRefused"
    );
}

#[tokio::test]
async fn objective_command_missing_revision_retries_and_wakes_once() {
    let w = world("true", |a| a).await;
    approve(&w).await;
    evaluate(&w, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .await
        .unwrap();
    settled(&w).await;
    project(&w).await;
    assert_eq!(phases(&w).await.len(), 3);
    assert_eq!(count(&w, "check_finished").await, 0);
    let wakes = rows(&w.boot, "track.wake_requested").await;
    assert_eq!(
        wakes
            .iter()
            .filter(|(_, r)| r.payload_value()["source"] == KIND)
            .count(),
        1
    );
}

#[tokio::test]
async fn objective_command_timeout_retries_and_wakes_once() {
    let w = world("sleep 60", |a| a.with_timeout(1)).await;
    approve(&w).await;
    evaluate(&w, &w.revision).await.unwrap();
    settled(&w).await;
    project(&w).await;
    assert_eq!(phases(&w).await.len(), 3);
    assert_eq!(count(&w, "check_finished").await, 0);
    assert_eq!(
        rows(&w.boot, "track.wake_requested")
            .await
            .iter()
            .filter(|(_, r)| r.payload_value()["source"] == KIND)
            .count(),
        1
    );
}

#[tokio::test]
async fn objective_command_atomic_finalize_and_recovery() {
    let w = world("true", |a| a).await;
    approve(&w).await;
    let db = w.boot.repo.sqlite_db().unwrap();
    sqlx::query("CREATE TRIGGER reject_command BEFORE INSERT ON objective_events WHEN NEW.kind='check_finished' \
         BEGIN SELECT RAISE(ABORT,'crash before result commit'); END")
        .execute(db.writer_pool()).await.unwrap();
    evaluate(&w, &w.revision).await.unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let error: Option<String> =
                sqlx::query_scalar("SELECT last_error FROM operations WHERE kind=?")
                    .bind(KIND)
                    .fetch_one(db.read_pool())
                    .await
                    .unwrap();
            if error.is_some_and(|e| e.contains("crash before result commit")) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("completion attempted the production result transaction");
    assert_eq!(phases(&w).await, ["parked"]);
    assert_eq!(count(&w, "check_finished").await, 0);
    let status: String =
        sqlx::query_scalar("SELECT status FROM objective_intents WHERE kind='run_check'")
            .fetch_one(db.read_pool())
            .await
            .unwrap();
    assert_eq!(status, "dispatched");
    sqlx::query("DROP TRIGGER reject_command")
        .execute(db.writer_pool())
        .await
        .unwrap();
    sqlx::query("UPDATE operations SET lease_until_ms=0 WHERE kind=? AND phase='parked'")
        .bind(KIND)
        .execute(db.writer_pool())
        .await
        .unwrap();
    ready(&w).await;
    project(&w).await;
    assert_eq!(phases(&w).await, ["succeeded"]);
    assert_eq!(count(&w, "check_finished").await, 1);
    let source: String =
        sqlx::query_scalar("SELECT source_kind FROM objective_events WHERE kind='check_finished'")
            .fetch_one(db.read_pool())
            .await
            .unwrap();
    assert_eq!(source, "intent_job");
}

#[tokio::test]
async fn objective_command_prepare_restart_is_idempotent() {
    let (hook, entered, resume) = pause();
    let once = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let w = Arc::new(
        world("true", |a| {
            a.with_before_spawn(Arc::new(move || {
                let (once, hook) = (once.clone(), hook.clone());
                Box::pin(async move {
                    if !once.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        hook().await;
                    }
                })
            }))
        })
        .await,
    );
    approve(&w).await;
    let launched = w.clone();
    let request = tokio::spawn(async move { evaluate(&launched, &launched.revision).await });
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    request.abort();
    let _ = request.await;
    resume.notify_one();
    let db = w.boot.repo.sqlite_db().unwrap();
    sqlx::query("UPDATE operations SET lease_until_ms=0 WHERE kind=?")
        .bind(KIND)
        .execute(db.writer_pool())
        .await
        .unwrap();
    let plan = w.runtime.recover_on_boot().await.unwrap();
    w.runtime.apply_recovery(plan).await.unwrap();
    ready(&w).await;
    project(&w).await;
    evaluate(&w, &w.revision).await.unwrap();
    assert_eq!(phases(&w).await, ["succeeded"]);
    assert_eq!(count(&w, "check_finished").await, 1);
}

#[tokio::test]
async fn objective_command_release_rechecks_intent_hash() {
    let (hook, entered, resume) = pause();
    let w = world("true", |a| a.with_before_release(hook)).await;
    approve(&w).await;
    evaluate(&w, &w.revision).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    // Isolate the hash fence from Open/current-evaluation/approval checks by
    // injecting a stale host snapshot. Immutable intent identity remains intact.
    let db = w.boot.repo.sqlite_db().unwrap();
    let mut stale = state(&w).await;
    let o = stale
        .objectives
        .get_mut(&objective::ids::ObjectiveKey::new("kr"))
        .unwrap();
    o.definition.checks[0].config =
        objective::check::CheckConfig::Command(objective::check::CommandParams {
            command: "false".into(),
        });
    o.approval.as_mut().unwrap().standard_hash = objective::standard::standard_hash(&o.definition);
    sqlx::query("UPDATE objective_states SET state_json=? WHERE track_id=?")
        .bind(serde_json::to_string(&stale).unwrap())
        .bind(w.boot.track_id.as_str())
        .execute(db.writer_pool())
        .await
        .unwrap();
    resume.notify_one();
    settled(&w).await;
    assert_eq!(count(&w, "check_finished").await, 0);
    let raw: String = sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE kind=?")
        .bind(KIND)
        .fetch_one(db.read_pool())
        .await
        .unwrap();
    let record: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        record["data"]["gate_execution"]["decision"]["Stopping"]["reason"],
        "ReleaseRefused"
    );
}

#[test]
fn objective_command_boot_cleans_settled_orphan_checkout() {
    let boot_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (mut w, writer) = boot_runtime.block_on(async {
        let w = world("true", |a| a).await;
        approve(&w).await;
        evaluate(&w, &w.revision).await.unwrap();
        ready(&w).await;
        settled(&w).await;
        // Drain submitted ACK statements and fence new ones at the single writer.
        let writer = w
            .boot
            .repo
            .sqlite_db()
            .unwrap()
            .writer_pool()
            .acquire()
            .await
            .unwrap();
        (w, writer)
    });
    // This is the crash boundary, before changing any durable cleanup state.
    // Runtime::drop cancels and drops every async task (including completion
    // cleanup and projectors), and waits for spawn_blocking Git removal to end.
    // The writer lease also fences SQLite's independent worker: earlier ACKs
    // finished before acquisition; blocked ACK tasks are dropped at shutdown.
    drop(boot_runtime);

    let restarted = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    restarted.block_on(async {
        drop(writer); // Return the drained connection on the new, live runtime.
        w._execution = crate::support::execution::Fixture::new(w.boot.repo.sqlite_db().unwrap()).await;
        w.runtime = operation_runtime(
            &w.boot,
            ObjectiveCheckRunAdapter::new(w.checks.clone(), w._tmp.path().join("logs")),
            w._execution.lifecycle.clone(),
        );
        let db = w.boot.repo.sqlite_db().unwrap();
        let raw: String = sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE kind=?")
            .bind(KIND)
            .fetch_one(db.read_pool())
            .await
            .unwrap();
        let output: Value = serde_json::from_str(&raw).unwrap();
        let path = PathBuf::from(output["data"]["cwd"].as_str().unwrap());
        // Shutdown may cancel cleanup before unlink; normalize that old checkout
        // only after its tasks are gone, then inject the crash's durable orphan.
        workspace::worktree::remove_worktree(&w.repo, &path).unwrap();
        sqlx::query("UPDATE operations SET tx_output_json=json_remove(tx_output_json,'$.data.checkout_removed') WHERE kind=?")
            .bind(KIND).execute(db.writer_pool()).await.unwrap();
        workspace::worktree::detached_worktree(&w.repo, &path, &w.revision).unwrap();
        assert!(path.exists());
        project(&w).await; // Fresh operation runtime and projector reconcile startup.
        assert!(!path.exists());
        assert_eq!(count(&w, "check_finished").await, 1);
    });
}

#[tokio::test]
async fn objective_command_one_active_operation_per_track() {
    let (hook, entered, resume) = pause();
    let once = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let w = world("true", |a| {
        a.with_before_release(Arc::new(move || {
            let (once, hook) = (once.clone(), hook.clone());
            Box::pin(async move {
                if !once.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    hook().await;
                }
            })
        }))
    })
    .await;
    let mut d = definition();
    d["mode"] = json!({"loop":{"plateau":10}});
    d["checks"][0]["role"] = json!("ratchet");
    d["checks"][0]["config"] = json!({"kind":"command","params":{"command":"true"}});
    let mut second = d["checks"][0].clone();
    second["id"] = json!("second");
    d["checks"].as_array_mut().unwrap().push(second);
    define(&w.boot, Some(w.block.clone()), d).await;
    approve(&w).await;
    evaluate(&w, &w.revision).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    tokio::join!(project(&w), project(&w));
    assert_eq!(phases(&w).await, ["parked"]);
    let statuses: Vec<String> = sqlx::query_scalar(
        "SELECT status FROM objective_intents WHERE kind='run_check' ORDER BY ordinal",
    )
    .fetch_all(w.boot.repo.sqlite_db().unwrap().read_pool())
    .await
    .unwrap();
    assert_eq!(statuses, ["dispatched", "pending"]);
    resume.notify_one();
    ready(&w).await;
    assert_eq!(phases(&w).await, ["succeeded", "succeeded"]);
    assert_eq!(count(&w, "check_finished").await, 2);
}

#[tokio::test]
async fn objective_command_infrastructure_failure_retries_without_cleanup_blocking() {
    let w = world("true", |a| a).await;
    approve(&w).await;
    let moved = w.repo.with_file_name("temporarily-unavailable");
    std::fs::rename(&w.repo, &moved).unwrap();
    evaluate(&w, &w.revision).await.unwrap();
    // Tick reconciliation must keep retrying even when Git cleanup has the same
    // temporary infrastructure failure as admission.
    for _ in 0..4 {
        project(&w).await;
    }
    assert_eq!(phases(&w).await.len(), 3);
    assert_eq!(count(&w, "check_finished").await, 0);
    assert_eq!(
        rows(&w.boot, "track.wake_requested")
            .await
            .iter()
            .filter(|(_, r)| r.payload_value()["source"] == KIND)
            .count(),
        1
    );
    std::fs::rename(&moved, &w.repo).unwrap();
    project(&w).await;
}

#[tokio::test]
async fn objective_command_waits_for_execution_readiness() {
    let w = world_with_readiness("true", |a| a, false).await;
    approve(&w).await;
    evaluate(&w, &w.revision).await.unwrap();
    for _ in 0..4 {
        project(&w).await;
    }
    assert!(
        phases(&w).await.is_empty(),
        "unready execution consumed a durable attempt"
    );
    assert_eq!(count(&w, "check_finished").await, 0);
    assert!(
        !objective_host::unsettled_intents(&w.boot.repo.sqlite_db().unwrap())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        rows(&w.boot, "track.wake_requested")
            .await
            .iter()
            .filter(|(_, r)| r.payload_value()["source"] == KIND)
            .count(),
        0
    );
    w.runtime
        .execution()
        .bind(Arc::new(w._execution.lifecycle.engine().unwrap().clone()));
    ready(&w).await;
    project(&w).await;
    assert_eq!(phases(&w).await, ["succeeded"]);
    assert_eq!(count(&w, "check_finished").await, 1);
}

#[tokio::test]
async fn objective_command_checkout_filter_is_bounded() {
    let mut w = world("true", |a| a.with_timeout(1)).await;
    std::fs::write(w.repo.join(".gitattributes"), "marker filter=stall\n").unwrap();
    git(&w.repo, &["add", ".gitattributes"]);
    git(&w.repo, &["commit", "-qm", "filter attributes"]);
    w.revision = git(&w.repo, &["rev-parse", "HEAD"]);
    let child_pid = w._tmp.path().join("filter-child.pid");
    git(
        &w.repo,
        &[
            "config",
            "filter.stall.smudge",
            &format!("sleep 8 & echo $! >> '{}'; wait; cat", child_pid.display()),
        ],
    );
    approve(&w).await;
    let started = std::time::Instant::now();
    // Hold the real global drive mutex in checkout while an unrelated operation
    // queues behind it. A finite checkout failure must let that operation run.
    let payload = json!({"title":"unrelated", "body":"independent operation"});
    let key = calm_server::operation::OperationKey {
        operation_key: "unrelated".into(),
        idempotency_key: Some("unrelated".into()),
        payload_hash: "unrelated".into(),
    };
    let (request, other) = tokio::join!(evaluate(&w, &w.revision), async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        tokio::time::timeout(
            Duration::from_secs(4),
            w.runtime.submit("track-recipe-create", key, payload),
        )
        .await
        .expect("checkout must release drive for unrelated operations")
    });
    request.unwrap();
    let other = other.unwrap();
    let other_phase: String = sqlx::query_scalar("SELECT phase FROM operations WHERE id=?")
        .bind(other)
        .fetch_one(w.boot.repo.sqlite_db().unwrap().read_pool())
        .await
        .unwrap();
    assert_eq!(other_phase, "succeeded");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "checkout blocked dispatch beyond its deadline"
    );
    settled(&w).await;
    project(&w).await;
    assert_eq!(phases(&w).await.len(), 3);
    assert_eq!(count(&w, "check_finished").await, 0);
    // Timed-out worktree initialization can leave a locked registration. Cleanup
    // must remove it as well, and all recorded descendants must be dead.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            project(&w).await;
            let removed: i64 = sqlx::query_scalar("SELECT count(*) FROM operations WHERE kind=? AND json_extract(tx_output_json,'$.data.checkout_removed') IS 1")
                .bind(KIND).fetch_one(w.boot.repo.sqlite_db().unwrap().read_pool()).await.unwrap();
            if removed == 3 { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("partial checkouts are recoverably removed");
    let pids = std::fs::read_to_string(child_pid).unwrap();
    assert_eq!(pids.lines().count(), 3);
    #[cfg(target_os = "linux")]
    for pid in pids.lines() {
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            assert!(
                stat.split_once(") ").unwrap().1.starts_with('Z'),
                "filter descendant still running: {stat}"
            );
        }
    }

    assert_eq!(
        rows(&w.boot, "track.wake_requested")
            .await
            .iter()
            .filter(|(_, r)| r.payload_value()["source"] == KIND)
            .count(),
        1
    );
}

#[path = "objective_metric.rs"]
mod objective_metric;
