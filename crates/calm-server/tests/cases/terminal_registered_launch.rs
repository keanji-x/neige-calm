//! Actual cookie-authenticated HTTP → operation → renderer → typed supervisor.
//! No spawn hook, model process, synthetic Operation, or fixture registration.
#![cfg(target_os = "linux")]

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, ensure};
use axum::{body::Body, http::Request};
use calm_server::{
    auth::{self, AuthConfig, AuthState},
    db::{prelude::*, sqlite::SqlxRepo},
    event::EventBus,
    model::{NewArea, NewTrack},
    plugin_host::{PluginHost, PluginRegistry},
    state::{AppState, CodexClient, DaemonClient, WriteContext},
};
use calm_session::control::execution::{
    CapabilityToken, ExecutionCapability, ExecutionControl, ExecutionControlPurpose,
    ExecutionReceipt, ExecutionReply, ExecutionRequest, ExecutionScope, PrepareExecution,
    ProcessAction, RegisteredExecutorConnection,
};
use calm_types::organization::LEGACY_OWNER_USER_ID;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

struct Supervisor {
    child: Child,
}

impl Supervisor {
    async fn stop(&mut self) {
        // The concrete child is still owned/unreaped: this cannot signal a
        // replacement PID. Let the production shutdown path stop its children.
        unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) };
        let completed = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if self.child.try_wait().unwrap().is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        if completed.is_err() {
            self.child.kill().unwrap();
            self.child.wait().unwrap();
            panic!("supervisor did not finish its production shutdown");
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            // An early fixture/setup failure still owns this exact process.
            unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) };
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while self.child.try_wait().ok().flatten().is_none()
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            if self.child.try_wait().ok().flatten().is_none() {
                let _ = self.child.kill();
            }
            let _ = self.child.wait();
        }
    }
}

struct Fixture {
    app: axum::Router,
    state: AppState,
    sql: Arc<SqlxRepo>,
    track: String,
    cwd: PathBuf,
    journal: PathBuf,
    output: PathBuf,
    program: PathBuf,
    cookie: String,
    client: RegisteredExecutorConnection,
    supervisor: Supervisor,
    _root: tempfile::TempDir,
    _sockets: tempfile::TempDir,
}

impl Fixture {
    async fn start() -> anyhow::Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir()?;
        let sockets = calm_test_sockets::try_socket_dir("terminal-vertical")?;
        let socket = sockets.path().join("control.sock");
        let journal = root.path().join("executor");
        let secret = CapabilityToken::fresh()?;
        let key = root.path().join("kernel-key.json");
        std::fs::write(&key, serde_json::to_vec(&secret)?)?;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600))?;
        let mut supervisor = Supervisor {
            child: Command::new(env!("CARGO_BIN_EXE_terminal-vertical-supervisor-fixture"))
                .arg("--control-sock")
                .arg(&socket)
                .arg("--execution-state-dir")
                .arg(&journal)
                .arg("--kernel-client-key-file")
                .arg(&key)
                .env("TOKIO_WORKER_THREADS", "2")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()?,
        };
        // A real authenticated RegisteredV3 handshake proves fixture setup is
        // usable. The fixture does NOT attach or prepare an execution for HTTP.
        let client = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(client) =
                    RegisteredExecutorConnection::connect(&socket, secret.clone()).await
                {
                    break Ok::<_, anyhow::Error>(client);
                }
                ensure!(
                    supervisor.child.try_wait()?.is_none(),
                    "supervisor exited during setup"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        let canonical = root.path().join("canonical");
        std::fs::create_dir(&canonical)?;
        git(&canonical, &["init", "-q"])?;
        git(
            &canonical,
            &[
                "-c",
                "user.name=Terminal fixture",
                "-c",
                "user.email=fixture@neige.invalid",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "base",
            ],
        )?;
        let sql = Arc::new(SqlxRepo::open("sqlite::memory:").await?);
        let area = sql
            .area_create(
                LEGACY_OWNER_USER_ID,
                NewArea {
                    name: "Terminal vertical".into(),
                    color: "#000".into(),
                    sort: None,
                },
            )
            .await?;
        let areas = calm_server::track_area_cache::TrackAreaCache::new();
        let track = calm_server::test_seams::create_track_with_workspace_for_test(
            sql.pool(),
            NewTrack {
                template_input: None,
                area_id: area.id,
                title: "Registered terminal".into(),
                sort: None,
                cwd: canonical.to_string_lossy().into_owned(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: calm_server::routes::theme::RequestTheme::default_dark(),
            },
            &calm_server::db::sqlite::TrackWorkspacePlan::AttachedWithTrackWorktree(canonical),
            &areas,
        )
        .await?;
        let cwd = PathBuf::from(track.workspace.agent_cwd());
        // This is the existing-directory case: the actual workspace owner above
        // created its worktree. Missing managed creation gets its own HTTP case.
        ensure!(
            cwd.is_dir(),
            "workspace owner did not prepare its declared worktree"
        );
        let events = EventBus::new();
        let route_repo: Arc<dyn Repo> = sql.clone();
        let state = AppState::from_parts(
            route_repo.clone(),
            events.clone(),
            Arc::new(DaemonClient {
                data_dir: root.path().join("terminals"),
                proc_supervisor_sock: Some(socket),
            }),
            Arc::new(PluginHost::new_full(
                Arc::new(PluginRegistry::empty()),
                route_repo.clone(),
                PathBuf::new(),
                root.path().join("plugins"),
                Vec::new(),
                events,
                WriteContext::new(
                    calm_server::card_role_cache::CardRoleCache::new(),
                    areas.clone(),
                ),
            )),
            Arc::new(CodexClient::new_stub()),
            None,
            Some(areas),
        );
        let auth_state = AuthState::new(AuthConfig {
            username: Some("owner".into()),
            password: Some("terminal-password".into()),
            dev_autologin: false,
            display_name: "Owner".into(),
        });
        let protected = calm_server::routes::protected_router()
            .layer(axum::middleware::from_fn(
                calm_server::actor::actor_middleware,
            ))
            .layer(axum::middleware::from_fn_with_state(
                auth_state.clone(),
                auth::require_session,
            ))
            .with_state(state.clone());
        let app = protected.merge(auth::router().with_state(auth_state));
        let login = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/login")
                    .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                        [127, 0, 0, 1],
                        4444,
                    ))))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"username":"owner","password":"terminal-password"}).to_string(),
                    ))?,
            )
            .await?;
        ensure!(
            login.status().is_success(),
            "actual login failed: {}",
            login.status()
        );
        let cookie = login
            .headers()
            .get("set-cookie")
            .context("login supplied no cookie")?
            .to_str()?
            .split(';')
            .next()
            .context("empty cookie")?
            .to_owned();
        let program = root.path().join("report-cwd.sh");
        let output = root.path().join("cwd-report");
        std::fs::write(
            &program,
            "#!/bin/sh\npwd -P > \"$NEIGE_TERMINAL_VERTICAL_OUTPUT\"\nprintf 'terminal-vertical-complete\\n'\n",
        )?;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))?;
        Ok(Self {
            app,
            state,
            sql,
            track: track.id.to_string(),
            cwd,
            journal,
            output,
            program,
            cookie,
            client,
            supervisor,
            _root: root,
            _sockets: sockets,
        })
    }

    async fn exercise(&mut self) -> anyhow::Result<()> {
        let response = self.app.clone().oneshot(
            Request::builder().method("POST")
                .uri(format!("/api/tracks/{}/terminal-cards", self.track))
                .header("content-type", "application/json").header("cookie", &self.cookie)
                .header("Idempotency-Key", "terminal-vertical-existing-worktree")
                .body(Body::from(json!({
                    "program": format!("'{}'", self.program.to_str().unwrap().replace('\'', "'\\''")),
                    "env":{"NEIGE_TERMINAL_VERTICAL_OUTPUT":self.output},
                    "theme":calm_server::routes::theme::RequestTheme::default_dark(),
                }).to_string()))?,
        ).await?;
        let status = response.status();
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await?.to_bytes())?;
        ensure!(
            status == axum::http::StatusCode::CREATED,
            "terminal HTTP failed before launch: {status} {body}"
        );
        let card = body["id"].as_str().context("created card has no id")?;
        let term = self
            .sql
            .terminal_get_by_card(card)
            .await?
            .context("no created terminal")?;
        let reported = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match std::fs::read_to_string(&self.output) {
                    Ok(text) if text.ends_with('\n') => break Ok::<_, anyhow::Error>(text),
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .context("real terminal program never reported cwd")??;
        ensure!(
            reported.trim() == self.cwd.canonicalize()?.to_str().unwrap(),
            "real terminal used a different cwd"
        );
        let operation: String = sqlx::query_scalar(
            "SELECT id FROM operations WHERE kind='terminal-create' AND idempotency_key=?1",
        )
        .bind("terminal-vertical-existing-worktree")
        .fetch_one(self.sql.pool())
        .await?;
        // This is Neige's own executor journal, not a guessed provider layout.
        // Read the actual request/receipt and verify it through the typed RPC.
        let paths = std::fs::read_dir(&self.journal)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        let records = paths
            .iter()
            .filter(|path| path.extension().is_some_and(|e| e == "execution"))
            .map(|path| -> anyhow::Result<Value> {
                Ok(serde_json::from_slice(&std::fs::read(path)?)?)
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let records = records
            .iter()
            .filter(|record| record["request"]["identity"]["operation_id"] == operation)
            .collect::<Vec<_>>();
        ensure!(
            records.len() == 1,
            "Terminal HTTP must own one real typed execution journal for its accepted operation; found {} (legacy Ensure is not typed launch)",
            records.len()
        );
        let request: PrepareExecution = serde_json::from_value(records[0]["request"].clone())?;
        ensure!(
            request.identity.process_alias == term.id,
            "typed execution belongs to another terminal"
        );
        ensure!(
            matches!(&request.identity.scope, ExecutionScope::Worktree { cwd, .. }
                if cwd == &workspace::identity::WorkspaceCwd::root()),
            "selected Git worktree was relabeled as a weaker external directory"
        );
        let phase = &records[0]["phase"];
        let receipt = phase
            .get("Spawned")
            .or_else(|| phase.get("Exited").and_then(|v| v.get("receipt")))
            .or_else(|| phase.get("Drained").and_then(|v| v.get("receipt")))
            .context("typed execution has no original fork receipt")?;
        let receipt: ExecutionReceipt = serde_json::from_value(receipt.clone())?;
        let reply = self
            .client
            .request(ExecutionRequest::Control(ExecutionControl {
                capability: ExecutionCapability {
                    identity: request.identity.clone(),
                    slot_version: receipt.slot_version,
                    purpose: ExecutionControlPurpose::Observe,
                    token: request.control_tokens.observe.clone(),
                },
                action: ProcessAction::Observe { from_cursor: None },
            }))
            .await?;
        ensure!(
            matches!(reply,ExecutionReply::Observation {receipt:Some(ref observed),..} if observed.identity==request.identity),
            "typed executor did not observe the original accepted execution"
        );
        Ok(())
    }

    async fn stop(&mut self) {
        // Complete cleanup before the red assertion; a panic must not abandon
        // the actual supervisor/renderer or race the next test.
        if let Ok(terminals) = sqlx::query_scalar::<_, String>("SELECT id FROM terminals")
            .fetch_all(self.sql.pool())
            .await
        {
            for terminal in terminals {
                self.state.terminal_renderer.drop_entry(&terminal).await;
            }
        }
        self.supervisor.stop().await;
    }
}

fn git(root: &Path, args: &[&str]) -> anyhow::Result<()> {
    let output = workspace::command::neige_git_command()
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?;
    ensure!(
        output.status.success(),
        "git fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[tokio::test]
async fn terminal_http_selected_workspace_uses_registered_launch() -> anyhow::Result<()> {
    let mut fixture = Fixture::start().await?;
    let result = fixture.exercise().await;
    fixture.stop().await;
    result
}
