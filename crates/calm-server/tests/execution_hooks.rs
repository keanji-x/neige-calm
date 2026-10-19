//! E2b-1: exercise lifecycle callers without a Terminal consumer.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use calm_server::{
    db::{
        prelude::*,
        sqlite::{self, SqlxRepo},
    },
    event::EventBus,
    execution_store::SqliteExecutionStore,
    model::*,
    plugin_host::{PluginHost, PluginRegistry},
    state::{AppState, CodexClient, DaemonClient, WriteContext},
};
use calm_truth::db::sqlite::workspace_execution::{self as sql, Scope};
use http_body_util::BodyExt;
use serde_json::json;
use std::{path::Path, sync::Arc, time::Duration};
use tower::ServiceExt;
use workspace_runtime::execution::{self as exec, *};

struct Fixture {
    repo: Arc<SqlxRepo>,
    state: AppState,
    store: SqliteExecutionStore,
    engine: Arc<calm_server::execution_boot::ExecutionEngine>,
    secret: CapabilityToken,
    area: String,
    track: String,
    root: std::path::PathBuf,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    server: tokio::task::JoinHandle<anyhow::Result<()>>,
    _temp: tempfile::TempDir,
    _socket: tempfile::TempDir,
}
impl Fixture {
    async fn new() -> Self {
        Self::with_engine(true).await
    }
    async fn with_engine(bind: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let socket_dir = calm_test_sockets::socket_dir("typed-hooks");
        let socket = socket_dir.path().join("control.sock");
        let secret = CapabilityToken::fresh().unwrap();
        let registry = calm_proc_supervisor::ProcRegistry::new()
            .with_executor(calm_proc_supervisor::execution::ExecutorConfig {
                journal_directory: temp.path().join("journal"),
                helper_program: env!("CARGO_BIN_EXE_osc-probe-child").into(),
                kernel_client_secret: secret.clone(),
            })
            .unwrap();
        let listener = calm_proc_supervisor::bind_control_listener(&socket).unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(calm_proc_supervisor::serve_with_listener(
            listener,
            socket.clone(),
            registry,
            stopped,
        ));
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let store = SqliteExecutionStore::new(repo.db().clone())
            .with_projection(
                calm_server::operation::terminal_execution_create::CONSUMER.into(),
                Arc::new(calm_server::operation::terminal_execution_create::TerminalProjection),
            )
            .with_projection(
                exec::test_support::TEST_CONSUMER.into(),
                Arc::new(NoopProjection),
            );
        let engine = Arc::new(Engine::new(
            store.clone(),
            session_executor::SessionExecutor::new(socket.clone(), secret.clone()),
        ));
        let events = EventBus::new();
        let roles = calm_server::card_role_cache::CardRoleCache::new();
        let tracks = calm_server::track_area_cache::TrackAreaCache::new();
        let write = WriteContext::new(roles.clone(), tracks.clone());
        let plugin = Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            temp.path().join("plugins"),
            temp.path().join("plugin-data"),
            vec![],
            events.clone(),
            write,
        ));
        let state = AppState::from_parts(
            repo.clone(),
            events,
            Arc::new(DaemonClient {
                data_dir: temp.path().join("data"),
                proc_supervisor_sock: Some(socket),
            }),
            plugin,
            Arc::new(CodexClient::new_stub()),
            Some(roles),
            Some(tracks.clone()),
        )
        .with_workspace_root(temp.path().join("workspaces"));
        let mut state = state;
        if bind {
            state.execution = Some(engine.clone());
            state.operation_runtime.execution().bind(engine.clone());
            state.plugin.execution.bind(engine.clone());
        }
        let area = repo
            .area_create(
                calm_types::organization::LEGACY_OWNER_USER_ID,
                NewArea {
                    name: "typed hooks".into(),
                    color: "#000000".into(),
                    sort: None,
                },
            )
            .await
            .unwrap();
        let track = calm_server::test_seams::create_track_with_workspace_for_test(
            repo.db(),
            NewTrack {
                area_id: area.id.clone(),
                title: "typed hooks".into(),
                cwd: String::new(),
                sort: None,
                template_id: None,
                plugin_scope: None,
                template_input: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            },
            &sqlite::TrackWorkspacePlan::ManagedUnder(temp.path().join("workspaces")),
            &tracks,
        )
        .await
        .unwrap();
        let root = std::path::PathBuf::from(&track.workspace.path);
        workspace_runtime::selection::materialize_workspace(
            &track.workspace,
            &temp.path().join("workspaces"),
            track.id.as_str(),
        )
        .unwrap();
        Self {
            repo,
            state,
            store,
            engine,
            secret,
            area: area.id.to_string(),
            track: track.id.to_string(),
            root,
            stop: Some(stop),
            server,
            _temp: temp,
            _socket: socket_dir,
        }
    }
    async fn terminal(&self) -> (Card, Terminal) {
        let card = self
            .repo
            .card_create(NewCard {
                track_id: self.track.clone().into(),
                kind: "terminal".into(),
                title: None,
                sort: None,
                payload: json!({"terminal_presentation": {"source": "unrecorded", "in_workbench": false}}),
            })
            .await
            .unwrap();
        let terminal = self
            .repo
            .terminal_create(NewTerminal {
                card_id: card.id.clone(),
                program: "/bin/sh".into(),
                cwd: self.root.display().to_string(),
                env: json!({}),
                theme: RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        (card, terminal)
    }
    async fn admit(&self, terminal: &Terminal, live: bool) -> ExecutionId {
        let mut intent = exec::test_support::intent(self.root.clone());
        intent.metadata.track = exec::TrackId(self.track.clone());
        intent.metadata.alias = ProcessAlias(sql::terminal_alias(&terminal.id));
        intent.metadata.program.program = "/bin/sh".into();
        intent.metadata.program.args = vec!["-c".into(), "printf ready; exec sleep 120".into()];
        let id = intent.metadata.id.clone();
        let mut tx = sqlite::begin_immediate_tx(self.repo.db()).await.unwrap();
        let ticket = self.engine.admit(&mut tx, intent).await.unwrap();
        tx.commit().await.unwrap();
        if live {
            self.engine.drive(ticket.committed());
            wait(|| async {
                matches!(
                    self.engine.output(&id, None).await,
                    Ok(ExecutionReply::Observation { state: ExecutionState::Spawned, ref output, .. }) if output == b"ready"
                )
            })
            .await;
        }
        id
    }
    async fn request(
        &self,
        method: &str,
        url: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, String) {
        self.request_keyed(method, url, body, None).await
    }
    async fn request_keyed(
        &self,
        method: &str,
        url: &str,
        body: Option<serde_json::Value>,
        key: Option<&str>,
    ) -> (StatusCode, String) {
        let app = calm_server::routes::router()
            .layer(axum::middleware::from_fn_with_state(
                calm_server::auth::AuthState::new(calm_server::auth::AuthConfig {
                    username: None,
                    password: None,
                    dev_autologin: true,
                    display_name: "Owner".into(),
                }),
                calm_server::auth::require_session,
            ))
            .layer(axum::middleware::from_fn(
                calm_server::actor::actor_middleware,
            ))
            .with_state(self.state.clone());
        let mut req = Request::builder().method(method).uri(url);
        if let Some(key) = key {
            req = req.header("idempotency-key", key);
        }
        let body = if let Some(body) = body {
            req = req.header("content-type", "application/json");
            Body::from(body.to_string())
        } else {
            Body::empty()
        };
        let response = app.oneshot(req.body(body).unwrap()).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }
    async fn desired(&self, id: &ExecutionId) -> String {
        sqlite::workspace_execution::load(self.repo.pool(), id.as_str())
            .await
            .unwrap()
            .unwrap()
            .desired
    }
    async fn retired(&self, id: &ExecutionId) {
        wait(|| async {
            matches!(
                self.store.load(id).await.unwrap(),
                Some(Stored::Finished { .. })
            )
        })
        .await;
        assert_eq!(self.desired(id).await, "retire");
    }
    async fn stop(&self, id: &ExecutionId) {
        let mut tx = sqlite::begin_immediate_tx(self.repo.db()).await.unwrap();
        let ticket = self
            .engine
            .request(&mut tx, id, Desired::Retire)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        self.engine.drive(ticket.committed());
        self.retired(id).await;
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.server.abort();
    }
}
fn git(root: &Path, args: &[&str]) {
    assert!(
        std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
}
async fn wait<F, Fut>(mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if check().await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn unsettled_executions_scopes() {
    let f = Fixture::new().await;
    let (_, terminal) = f.terminal().await;
    let id = f.admit(&terminal, false).await;
    for scope in [
        Scope::Track(&f.track),
        Scope::Area(&f.area),
        Scope::Alias(&sql::terminal_alias(&terminal.id)),
    ] {
        assert_eq!(
            sql::unsettled_executions(&mut f.repo.pool().acquire().await.unwrap(), scope)
                .await
                .unwrap(),
            vec![id.clone()]
        );
    }
    assert!(
        sql::unsettled_executions(
            &mut f.repo.pool().acquire().await.unwrap(),
            Scope::Track("other")
        )
        .await
        .unwrap()
        .is_empty()
    );
    let mut tx = sqlite::begin_immediate_tx(f.repo.db()).await.unwrap();
    let _ticket = f
        .engine
        .request(&mut tx, &id, Desired::Retire)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        sql::unsettled_executions(
            &mut f.repo.pool().acquire().await.unwrap(),
            Scope::Alias(&sql::terminal_alias(&terminal.id))
        )
        .await
        .unwrap(),
        vec![id.clone()],
        "Retire desire alone does not release occupancy"
    );
    f.stop(&id).await;
    assert!(
        sql::unsettled_executions(
            &mut f.repo.pool().acquire().await.unwrap(),
            Scope::Track(&f.track)
        )
        .await
        .unwrap()
        .is_empty()
    );
}

#[tokio::test]
async fn terminal_delete_tx_returns_retire_and_rolls_back() {
    let f = Fixture::new().await;
    let (_, terminal) = f.terminal().await;
    let id = f.admit(&terminal, true).await;
    let mut tx = sqlite::begin_immediate_tx(f.repo.db()).await.unwrap();
    let deleted = sqlite::terminal_delete_tx(&mut tx, &terminal.id)
        .await
        .unwrap();
    assert_eq!(deleted.retire, Some(id.clone()));
    tx.rollback().await.unwrap();
    assert_eq!(f.desired(&id).await, "run");
    assert!(f.repo.terminal_get(&terminal.id).await.unwrap().is_some());
    assert!(f.engine.output(&id, None).await.is_ok());
    f.stop(&id).await;
}

#[tokio::test]
async fn card_delete_quiesces_typed_then_retires() {
    let f = Fixture::new().await;
    let (card, terminal) = f.terminal().await;
    let id = f.admit(&terminal, true).await;
    let (status, body) = f
        .request("DELETE", &format!("/api/cards/{}", card.id), None)
        .await;
    if status != StatusCode::NO_CONTENT {
        f.stop(&id).await;
    }
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    f.retired(&id).await;
    assert!(f.repo.terminal_get(&terminal.id).await.unwrap().is_none());
}

#[tokio::test]
async fn track_delete_preflight_quiesce_recycle() {
    let f = Fixture::new().await;
    let (_, terminal) = f.terminal().await;
    // E2b-2 supplies the Legacy entry branch for launched typed terminal aliases.
    let id = f.admit(&terminal, false).await;
    let (status, body) = f
        .request("DELETE", &format!("/api/tracks/{}", f.track), None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    f.retired(&id).await;
    assert!(!f.root.exists());
}

#[tokio::test]
async fn track_delete_preflight_refused_keeps_typed_running() {
    let f = Fixture::new().await;
    let (_, terminal) = f.terminal().await;
    let id = f.admit(&terminal, true).await;
    // A descendant is rejected before the Legacy stop point.
    let mut tx = sqlite::begin_immediate_tx(f.repo.db()).await.unwrap();
    let child = sqlite::track_create_tx(
        &mut tx,
        NewTrack {
            area_id: f.area.clone().into(),
            title: "child".into(),
            cwd: f.root.display().to_string(),
            sort: None,
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        None,
        &sqlite::TrackWorkspacePlan::AttachedFromCwd,
        None,
        &calm_server::track_area_cache::TrackAreaCache::new(),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE tracks SET parent_track_id = ? WHERE id = ?")
        .bind(&f.track)
        .bind(child.id.as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (status, _) = f
        .request("DELETE", &format!("/api/tracks/{}", f.track), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(f.desired(&id).await, "run");
    assert!(f.engine.output(&id, None).await.is_ok());
    f.stop(&id).await;
}

#[tokio::test]
async fn quiesce_timeout_is_conflict_and_keeps_rows() {
    let f = Fixture::new().await;
    let (card, terminal) = f.terminal().await;
    let id = f.admit(&terminal, false).await;
    f.server.abort(); // Transport loss cannot establish settlement.
    let (status, body) = f
        .request("DELETE", &format!("/api/cards/{}", card.id), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(f.desired(&id).await, "stop");
    assert!(f.root.is_dir());
    assert!(f.repo.terminal_get(&terminal.id).await.unwrap().is_some());
    assert!(matches!(
        f.store.load(&id).await.unwrap(),
        Some(Stored::Active {
            settlement: None,
            ..
        })
    ));
}

#[tokio::test]
async fn repoint_rejects_unsettled_typed_without_stop() {
    let f = Fixture::new().await;
    // Avoid a Terminal row, whose Legacy workspace freeze would mask this fence.
    let mut intent = exec::test_support::intent(f.root.clone());
    intent.metadata.track = exec::TrackId(f.track.clone());
    let id = intent.metadata.id.clone();
    let mut tx = sqlite::begin_immediate_tx(f.repo.db()).await.unwrap();
    let _ticket = f.engine.admit(&mut tx, intent).await.unwrap();
    tx.commit().await.unwrap();
    let target = f._temp.path().join("attached");
    std::fs::create_dir(&target).unwrap();
    git(&target, &["init", "-q"]);
    let (status, body) = f
        .request(
            "PATCH",
            &format!("/api/tracks/{}", f.track),
            Some(json!({"workspace": {"kind": "attached", "path": target, "attach_folder": true}})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("unsettled"), "{body}");
    assert_eq!(f.desired(&id).await, "run");
    f.stop(&id).await;
}

#[tokio::test]
async fn boot_reconcile_skips_typed_rows() {
    let f = Fixture::new().await;
    let (_, typed) = f.terminal().await;
    let (_, legacy) = f.terminal().await;
    let id = f.admit(&typed, false).await;
    f.repo
        .terminal_set_pid(&typed.id, Some(999999))
        .await
        .unwrap();
    f.repo
        .terminal_set_pid(&legacy.id, Some(999998))
        .await
        .unwrap();
    let stale = calm_server::reconcile_supervisor_on_boot(&f.state).await;
    assert_eq!(stale, vec![legacy.id]);
    assert_eq!(
        f.repo
            .terminal_get(&typed.id)
            .await
            .unwrap()
            .unwrap()
            .exit_code,
        None
    );
    f.stop(&id).await;
    // Mode remains typed after settlement and retirement.
    assert!(
        calm_server::reconcile_supervisor_on_boot(&f.state)
            .await
            .is_empty()
    );
}

async fn worker(
    f: &Fixture,
) -> (
    ExecutionId,
    calm_server::terminal_sweeper::SettledWorkerSession,
) {
    use calm_server::session_projection_repo::{
        WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    let (card, terminal) = f.terminal().await;
    sqlx::query("UPDATE cards SET role = 'worker' WHERE id = ?")
        .bind(card.id.as_str())
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    let id = f.admit(&terminal, true).await;
    let session = new_id();
    let mut tx = sqlite::begin_immediate_tx(f.repo.db()).await.unwrap();
    sqlite::session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: session.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::Terminal,
            agent_provider: None,
            status: WorkerSessionState::Running,
            terminal_run_id: Some(terminal.id.clone()),
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms() - 180_000,
        },
    )
    .await
    .unwrap();
    sqlx::query("UPDATE tracks SET closed_at = ? WHERE id = ?")
        .bind(now_ms())
        .bind(&f.track)
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    (
        id,
        calm_server::terminal_sweeper::SettledWorkerSession {
            id: session,
            provider: "terminal".into(),
            card_id: card.id.to_string(),
            terminal_id: terminal.id,
            thread_id: None,
        },
    )
}

#[tokio::test]
async fn settled_worker_reap_stops_typed_keeps_row() {
    let f = Fixture::new().await;
    let (id, session) = worker(&f).await;
    calm_server::terminal_sweeper::end_settled_worker_session(&f.state, &session)
        .await
        .unwrap();
    f.engine.wait_settled(&id).await.unwrap();
    assert_eq!(f.desired(&id).await, "stop");
    let state: String = sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id = ?")
        .bind(&session.id)
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
    assert_eq!(state, "exited");
    assert!(
        f.repo
            .terminal_get(&session.terminal_id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        f.store.load(&id).await.unwrap(),
        Some(Stored::Active {
            settlement: Some(_),
            ..
        })
    ));
    f.stop(&id).await;
}

#[tokio::test]
async fn settled_worker_reopen_during_claim_does_not_stop() {
    let f = Fixture::new().await;
    let (id, session) = worker(&f).await;
    calm_server::terminal_sweeper::end_settled_worker_session_before_write_for_test(
        &f.state,
        &session,
        || async {
            sqlx::query("UPDATE tracks SET closed_at = NULL WHERE id = ?")
                .bind(&f.track)
                .execute(f.repo.db().writer_pool())
                .await
                .unwrap();
        },
    )
    .await
    .unwrap();
    assert_eq!(f.desired(&id).await, "run");
    let state: String = sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id = ?")
        .bind(&session.id)
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
    assert_eq!(state, "running");
    f.stop(&id).await;
}

#[tokio::test]
async fn worktree_clean_skips_until_settled() {
    let f = Fixture::new().await;
    std::fs::write(f.root.join(".gitignore"), "target/\n").unwrap();
    git(&f.root, &["add", ".gitignore"]);
    git(
        &f.root,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            "init",
        ],
    );
    let worktree = workspace::worktree::track_worktree_path_for(&f.root, &f.track);
    git(
        &f.root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &workspace::worktree::track_branch_for(&f.track).unwrap(),
            worktree.to_str().unwrap(),
        ],
    );
    std::fs::create_dir(worktree.join("target")).unwrap();
    std::fs::write(worktree.join("target/build"), "build").unwrap();
    // This test only needs an admitted execution. It does not create a Terminal/session.
    let mut intent = exec::test_support::intent(f.root.clone());
    intent.metadata.track = exec::TrackId(f.track.clone());
    let id = intent.metadata.id.clone();
    let mut tx = sqlite::begin_immediate_tx(f.repo.db()).await.unwrap();
    let _ticket = f.engine.admit(&mut tx, intent).await.unwrap();
    let mut workspace = sqlite::track_workspace_read_tx(&mut tx, &f.track)
        .await
        .unwrap();
    workspace.kind = calm_types::model::TrackWorkspaceKind::Attached;
    workspace.worktree = Some(worktree.to_str().unwrap().to_owned());
    sqlite::track_workspace_write_tx(&mut tx, &f.track, &workspace)
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET closed_at = ? WHERE id = ?")
        .bind(now_ms())
        .bind(&f.track)
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        calm_server::test_seams::clean_idle_closed_track_worktrees_for_test(
            f.repo.pool(),
            &f.state.operation_runtime
        )
        .await
        .unwrap(),
        0
    );
    assert!(worktree.join("target/build").is_file());
    f.stop(&id).await;
    assert_eq!(
        calm_server::test_seams::clean_idle_closed_track_worktrees_for_test(
            f.repo.pool(),
            &f.state.operation_runtime
        )
        .await
        .unwrap(),
        1
    );
    assert!(!worktree.join("target").exists());
}

#[tokio::test]
async fn quiesce_without_engine_is_conflict_and_keeps_rows() {
    let f = Fixture::with_engine(false).await;
    let (card, terminal) = f.terminal().await;
    let id = f.admit(&terminal, false).await;
    let (status, body) = f
        .request("DELETE", &format!("/api/cards/{}", card.id), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(f.desired(&id).await, "stop");
    assert!(f.root.is_dir());
    assert!(f.repo.terminal_get(&terminal.id).await.unwrap().is_some());
    f.stop(&id).await;
}

#[tokio::test]
async fn area_delete_quiesces_typed_then_retires() {
    let f = Fixture::new().await;
    let (_, terminal) = f.terminal().await;
    // E2b-2 supplies the Legacy entry branch for launched typed terminal aliases.
    let id = f.admit(&terminal, false).await;
    let (status, body) = f
        .request("DELETE", &format!("/api/areas/{}", f.area), None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    f.retired(&id).await;
    assert!(!f.root.exists());
}

#[tokio::test]
async fn orphan_sweep_quiesces_typed_then_retires() {
    let f = Fixture::new().await;
    let (_, terminal) = f.terminal().await;
    let id = f.admit(&terminal, true).await;
    sqlx::query("UPDATE terminals SET created_at = ? WHERE id = ?")
        .bind(now_ms() - 180_000)
        .bind(&terminal.id)
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    calm_server::terminal_sweeper::sweep(&f.state)
        .await
        .unwrap();
    f.retired(&id).await;
    assert!(f.repo.terminal_get(&terminal.id).await.unwrap().is_none());
    assert!(f.root.is_dir());
}

#[path = "execution_hooks/terminal_create.rs"]
mod terminal_execution_create;

struct NoopProjection;
#[async_trait::async_trait]
impl calm_server::execution_store::ExecutionProjection for NoopProjection {
    async fn spawned(
        &self,
        _: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        _: &str,
        _: i64,
    ) -> Result<(), sqlx::Error> {
        Ok(())
    }
    async fn settled(
        &self,
        _: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        _: &str,
        _: &Settlement,
        _: i64,
    ) -> Result<(), sqlx::Error> {
        Ok(())
    }
}

#[path = "execution_hooks/terminal_io.rs"]
mod terminal_io;

#[path = "execution_hooks/terminal_recovery.rs"]
mod terminal_recovery;

#[path = "execution_hooks/refusal_order.rs"]
mod refusal_order;
