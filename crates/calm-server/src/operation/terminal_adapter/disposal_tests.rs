use super::*;
use crate::db::prelude::*;
use crate::event::EventBus;
use crate::plugin_host::{PluginHost, PluginRegistry};
use crate::shared_codex_appserver::SharedCodexAppServer;
use crate::state::{AppState, CodexClient, DaemonClient, WriteContext};
use crate::track_report::{ReportDocOp, ReportEditTarget};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use calm_session::control::{ControlMsg, ControlReply};
use calm_session::{read_frame, write_frame};
use http_body_util::BodyExt;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tower::ServiceExt;

#[derive(Clone, Copy, Debug)]
enum Disposal {
    Card,
    Track,
    Area,
    Repoint,
    Sweep,
}
#[tokio::test]
async fn unresolved_launch_delete_card_retains_rows() {
    exercise(History::Unknown, Disposal::Card).await;
}
#[tokio::test]
async fn unresolved_launch_delete_track_retains_workspace() {
    exercise(History::Unknown, Disposal::Track).await;
}
#[tokio::test]
async fn unresolved_launch_delete_area_retains_workspace() {
    exercise(History::Unknown, Disposal::Area).await;
}
#[tokio::test]
async fn unresolved_launch_repoint_retains_workspace() {
    exercise(History::Unknown, Disposal::Repoint).await;
}

#[derive(Clone, Copy, Debug)]
enum History {
    Unknown,
    OldUnknown,
    OldCompensating,
    Healthy,
    OldSuccessful,
    OldPrestart,
    PidWriteFailure,
}
impl History {
    fn successful(self) -> bool {
        matches!(self, Self::Healthy | Self::OldSuccessful)
    }
}
#[tokio::test]
async fn old_prestart_launch_delete_card_uses_normal_cleanup() {
    exercise(History::OldPrestart, Disposal::Card).await;
}
#[tokio::test]
async fn pid_write_failure_does_not_publish_handoff_or_allow_delete() {
    exercise(History::PidWriteFailure, Disposal::Card).await;
}

#[tokio::test]
async fn unresolved_launch_sweep_retains_terminal() {
    exercise(History::Unknown, Disposal::Sweep).await;
}
#[tokio::test]
async fn unresolved_old_launch_delete_card_retains_rows() {
    exercise(History::OldUnknown, Disposal::Card).await;
}
#[tokio::test]
async fn unresolved_compensating_launch_delete_card_retains_rows() {
    exercise(History::OldCompensating, Disposal::Card).await;
}
#[tokio::test]
async fn handed_off_launch_delete_card_uses_normal_cleanup() {
    exercise(History::Healthy, Disposal::Card).await;
}
#[tokio::test]
async fn handed_off_launch_delete_track_uses_normal_cleanup() {
    exercise(History::Healthy, Disposal::Track).await;
}
#[tokio::test]
async fn handed_off_launch_delete_area_uses_normal_cleanup() {
    exercise(History::Healthy, Disposal::Area).await;
}
#[tokio::test]
async fn old_successful_launch_delete_card_uses_normal_cleanup() {
    exercise(History::OldSuccessful, Disposal::Card).await;
}

async fn call(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn exercise(history: History, disposal: Disposal) {
    let temp = tempfile::tempdir().unwrap();
    let execution = temp.path().join("execution");
    std::fs::create_dir(&execution).unwrap();
    let supervisor = calm_proc_supervisor::test_support::InProcessProcSupervisor::start()
        .await
        .unwrap();
    let proxy = PendingProxy::start(supervisor.sock()).await;
    let repo = Arc::new(
        crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap(),
    );
    let roles = CardRoleCache::new();
    let areas = TrackAreaCache::new();
    repo.seed_card_role_cache(&roles).await.unwrap();
    repo.seed_track_area_cache(&areas).await.unwrap();
    let events = EventBus::new();
    let write = WriteContext::new(roles.clone(), areas.clone());
    let plugin = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        repo.clone(),
        PathBuf::new(),
        temp.path().join("plugins"),
        vec![],
        events.clone(),
        write.clone(),
    ));
    let mut daemon = DaemonClient::new_stub();
    daemon.proc_supervisor_sock = Some(
        if history.successful() || matches!(history, History::PidWriteFailure) {
            supervisor.sock().to_path_buf()
        } else {
            proxy.sock.clone()
        },
    );
    daemon.data_dir = temp.path().join("data");
    let shared = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let state = AppState::from_parts(
        repo.clone(),
        events,
        Arc::new(daemon),
        plugin,
        Arc::new(CodexClient::new_stub()),
        Some(roles.clone()),
        Some(areas.clone()),
    )
    .with_shared_codex_appserver(shared)
    .with_workspace_root(temp.path().join("workspaces"));
    let runtime = state.operation_runtime.clone();
    let app = crate::routes::router()
        .layer(axum::middleware::from_fn(crate::actor::actor_middleware))
        .with_state(state.clone());
    let (status, body) = call(
        app.clone(),
        "POST",
        "/api/areas",
        Some(json!({"name":"disposal","color":"#abc"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let area: Value = serde_json::from_str(&body).unwrap();
    let area_id = area["id"].as_str().unwrap();
    let (status,body)=call(app.clone(),"POST","/api/tracks",Some(json!({"area_id":area_id,"title":"owned workspace","theme":{"fg":[255,255,255],"bg":[0,0,0]}}))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let track: Value = serde_json::from_str(&body).unwrap();
    let track_id = track["id"].as_str().unwrap().to_string();
    let before = repo.track_get(&track_id).await.unwrap().unwrap();
    let workspace = PathBuf::from(&before.workspace.path);
    assert!(workspace.join(".git").is_dir());
    repo.track_update(
        &track_id,
        crate::model::TrackPatch {
            lifecycle: Some(crate::model::TrackLifecycle::Working),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (_, _, report) = crate::track_report::resolve_report_for_track(repo.as_ref(), &track_id)
        .await
        .unwrap();
    let declaration = json!({"key":"launch","kind":"terminal","command":"printf late-start > started; sleep 120","cwd":execution,"ready":true,"declared_by":"user"});
    // Use the production authoring and claim paths; a separate bus avoids
    // racing this controlled manual driver with the fixture's dispatcher.
    let quiet = EventBus::new();
    let target = ReportEditTarget::resolve(repo.as_ref(), &track_id)
        .await
        .unwrap();
    crate::track_report::write::rest_user_block_op(
        repo.as_ref(),
        &quiet,
        &write,
        target,
        ReportDocOp::UpsertBlock {
            id: None,
            kind: "task".into(),
            content: calm_types::report_blocks::render_fence("task", &declaration),
            if_rev: None,
            if_doc_rev: Some(report.doc_rev),
            position: None,
        },
    )
    .await
    .unwrap();
    let task = repo
        .task_current_get(&track_id, "launch")
        .await
        .unwrap()
        .unwrap();
    let monitor = crate::task_context::TaskContextMonitor::new(repo.clone(), quiet, write);
    let closure = monitor
        .resolve_task_closure(&track_id, "launch")
        .await
        .unwrap();
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    assert_eq!(
        crate::db::sqlite::task_claim_pending_tx(
            &mut tx,
            &task.id,
            crate::model::now_ms(),
            &closure.refs,
            closure.closure_truncated
        )
        .await
        .unwrap(),
        1
    );
    tx.commit().await.unwrap();
    let (kind, payload) = crate::scheduler::build_worker_payload(&task).unwrap();
    let key = OperationKey {
        operation_key: new_id(),
        idempotency_key: Some(task.id),
        payload_hash: crate::routes::terminal_cards::stable_payload_hash(&payload).unwrap(),
    };
    let operations = SqlxOperationRepo::new(repo.pool().clone());
    if matches!(history, History::PidWriteFailure) {
        sqlx::query("CREATE TRIGGER reject_terminal_pid BEFORE UPDATE OF pid ON terminals WHEN NEW.pid IS NOT NULL BEGIN SELECT RAISE(FAIL,'fixture pid write failure'); END").execute(repo.pool()).await.unwrap();
    }
    if matches!(history, History::OldPrestart) {
        let adapter = TerminalWorkerAdapter::new(repo.clone(), roles, areas);
        let id = operations
            .insert_operation(kind, key.clone(), payload.clone())
            .await
            .unwrap();
        let claimed = operations
            .claim_drive_batch(10)
            .await
            .unwrap()
            .into_iter()
            .find(|op| op.id == id)
            .unwrap();
        operations
            .prepare_tx_and_advance(&claimed, &adapter)
            .await
            .unwrap()
            .unwrap();
    } else {
        let submitted = key.clone();
        let drive = runtime.clone();
        let run = tokio::spawn(async move { drive.submit(kind, submitted, payload).await });
        struct Abort(tokio::task::AbortHandle);
        impl Drop for Abort {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _abort = Abort(run.abort_handle());
        if history.successful() || matches!(history, History::PidWriteFailure) {
            tokio::time::timeout(Duration::from_secs(15), run)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        } else {
            tokio::time::timeout(Duration::from_secs(15), proxy.received.notified())
                .await
                .unwrap();
            run.abort();
            assert!(run.await.unwrap_err().is_cancelled());
        }
    }
    let op = operations
        .find_by_idempotency_key(kind, &key)
        .await
        .unwrap()
        .unwrap();
    let output = op.tx_output.as_ref().unwrap();
    let card_id = output.output_string("card_id", "test").unwrap();
    let term_id = output.output_string("terminal_id", "test").unwrap();
    let terminal = repo.terminal_get(&term_id).await.unwrap().unwrap();
    if history.successful() {
        assert!(terminal.pid.is_some());
        assert!(state.terminal_renderer.get(&term_id).is_some());
        assert_eq!(output.data["terminal_launch"]["state"], "handed_off");
    } else {
        assert!(terminal.pid.is_none());
    }
    if matches!(
        history,
        History::OldUnknown
            | History::OldCompensating
            | History::OldSuccessful
            | History::OldPrestart
    ) {
        // Model exact released records: normal preparation/launch happened via
        // the actual adapter above; only the later-added private field is absent.
        sqlx::query("UPDATE operations SET tx_output_json=json_remove(tx_output_json,'$.data.terminal_launch') WHERE id=?1")
            .bind(&op.id).execute(repo.pool()).await.unwrap();
    }
    if matches!(history, History::OldCompensating) {
        let state = CompensationStateVersioned {
            version: 1,
            from_phase: PhaseTag::SpawnStarted,
            reason: "lost launch".into(),
            steps: vec![],
        };
        operations
            .set_compensating(&op, &state, output)
            .await
            .unwrap()
            .unwrap();
        sqlx::query("UPDATE operations SET tx_output_json=json_remove(tx_output_json,'$.data.terminal_launch') WHERE id=?1").bind(&op.id).execute(repo.pool()).await.unwrap();
    }
    if matches!(disposal, Disposal::Sweep) {
        sqlx::query("UPDATE terminals SET created_at=0 WHERE id=?1")
            .bind(&term_id)
            .execute(repo.pool())
            .await
            .unwrap();
        sqlx::query("UPDATE worker_sessions SET state='failed' WHERE card_id=?1")
            .bind(&card_id)
            .execute(repo.pool())
            .await
            .unwrap();
        assert!(
            repo.terminals_orphaned(60)
                .await
                .unwrap()
                .iter()
                .any(|t| t.id == term_id)
        );
        crate::terminal_sweeper::sweep(&state).await.unwrap();
        release_late_process(&proxy, &execution).await;
        assert!(
            repo.terminal_get(&term_id).await.unwrap().is_some(),
            "sweeper discarded unknown launch"
        );
        return;
    }
    let (method, uri, body) = match disposal {
        Disposal::Card => ("DELETE", format!("/api/cards/{card_id}"), None),
        Disposal::Track => ("DELETE", format!("/api/tracks/{track_id}"), None),
        Disposal::Area => ("DELETE", format!("/api/areas/{area_id}"), None),
        Disposal::Sweep => unreachable!(),
        Disposal::Repoint => {
            let target = temp.path().join("target");
            std::fs::create_dir(&target).unwrap();
            assert!(
                std::process::Command::new("git")
                    .args(["init", "-q"])
                    .current_dir(&target)
                    .status()
                    .unwrap()
                    .success()
            );
            (
                "PATCH",
                format!("/api/tracks/{track_id}"),
                Some(json!({"workspace":{"kind":"attached","path":target,"attach_folder":true}})),
            )
        }
    };
    let (status, body) =
        tokio::time::timeout(Duration::from_secs(15), call(app, method, &uri, body))
            .await
            .unwrap();
    if history.successful() || matches!(history, History::OldPrestart) {
        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "normal cleanup rejected: {body}"
        );
        assert!(repo.card_get(&card_id).await.unwrap().is_none());
        assert!(repo.terminal_get(&term_id).await.unwrap().is_none());
        return;
    }
    // The original request can still launch after a truthful Probe(false).
    if matches!(history, History::PidWriteFailure) {
        assert_eq!(output.data["terminal_launch"]["state"], "requested");
        assert!(
            state.terminal_renderer.get(&term_id).is_some(),
            "renderer installed but PID write failed"
        );
    } else {
        release_late_process(&proxy, &execution).await;
    }
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "{disposal:?} must retain unresolved owned launch: {body}"
    );
    assert!(repo.card_get(&card_id).await.unwrap().is_some());
    assert!(repo.terminal_get(&term_id).await.unwrap().is_some());
    assert!(repo.track_get(&track_id).await.unwrap().is_some());
    assert!(repo.area_get(area_id).await.unwrap().is_some());
    assert!(
        workspace.join(".git").is_dir(),
        "managed workspace must not move"
    );
}

async fn release_late_process(proxy: &PendingProxy, execution: &Path) {
    proxy.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), proxy.spawned.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !execution.join("started").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

struct PendingProxy {
    sock: PathBuf,
    _dir: tempfile::TempDir,
    task: tokio::task::JoinHandle<()>,
    received: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    spawned: Arc<tokio::sync::Notify>,
}
impl PendingProxy {
    async fn start(upstream: &Path) -> Self {
        let dir = calm_test_sockets::socket_dir("dispose");
        let sock = dir.path().join("proxy.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let upstream = upstream.to_path_buf();
        let received = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let spawned = Arc::new(tokio::sync::Notify::new());
        let rec = received.clone();
        let rel = release.clone();
        let spawn = spawned.clone();
        let task = tokio::spawn(async move {
            let mut clients = tokio::task::JoinSet::new();
            loop {
                let (mut client, _) = listener.accept().await.unwrap();
                let upstream = upstream.clone();
                let rec = rec.clone();
                let rel = rel.clone();
                let spawn = spawn.clone();
                clients.spawn(async move {
                    let Ok(message) = read_frame::<ControlMsg, _>(&mut client).await else {
                        return;
                    };
                    if matches!(message, ControlMsg::EnsureProc(_)) {
                        rec.notify_one();
                        rel.notified().await;
                        let mut actual = tokio::net::UnixStream::connect(upstream).await.unwrap();
                        write_frame(&mut actual, &message).await.unwrap();
                        assert!(matches!(
                            read_frame::<ControlReply, _>(&mut actual).await.unwrap(),
                            ControlReply::Spawned { .. }
                        ));
                        spawn.notify_one();
                    } else {
                        let mut actual = tokio::net::UnixStream::connect(upstream).await.unwrap();
                        write_frame(&mut actual, &message).await.unwrap();
                        let reply: ControlReply = read_frame(&mut actual).await.unwrap();
                        let _ = write_frame(&mut client, &reply).await;
                    }
                });
            }
        });
        Self {
            sock,
            _dir: dir,
            task,
            received,
            release,
            spawned,
        }
    }
}
impl Drop for PendingProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}
