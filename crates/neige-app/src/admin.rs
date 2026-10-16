//! Existing normal service boot and authenticated admin operations.
use crate::{
    apply,
    cli::SystemServeArgs,
    config::{AppConfig, ServeOverrides},
    identity::{self, SpawnIdentity},
    manifest::UnitName,
    supervisor::*,
    tailnet,
};
use anyhow::Context;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) cfg: Arc<AppConfig>,
    pub(crate) supervisor: Arc<Supervisor>,
    pub(crate) proc_supervisor: Arc<Supervisor>,
    pub(crate) tailnet: Option<Arc<tailnet::TailnetManager>>,
    pub(crate) apply_lock: Arc<Mutex<()>>,
    pub(crate) admin_token: Option<Arc<str>>,
}

impl AppState {
    pub(crate) async fn status_snapshot(&self) -> StatusSnapshot {
        let calm_server = self.supervisor.process_status().await;
        let proc_supervisor = self.proc_supervisor.process_status().await;
        StatusSnapshot {
            desired_running: calm_server.desired_running,
            child_state: calm_server.child_state.clone(),
            child_pid: calm_server.child_pid,
            restart_count: calm_server.restart_count,
            last_exit: calm_server.last_exit.clone(),
            calm_listen: self.supervisor.cfg.calm_listen.clone().unwrap_or_default(),
            calm_server,
            proc_supervisor,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatusSnapshot {
    /// Deprecated compatibility field; mirrors `calmServer.desiredRunning`.
    pub(crate) desired_running: bool,
    /// Deprecated compatibility field; mirrors `calmServer.childState`.
    pub(crate) child_state: String,
    /// Deprecated compatibility field; mirrors `calmServer.childPid`.
    pub(crate) child_pid: Option<u32>,
    /// Deprecated compatibility field; mirrors `calmServer.restartCount`.
    pub(crate) restart_count: u64,
    /// Deprecated compatibility field; mirrors `calmServer.lastExit`.
    pub(crate) last_exit: Option<String>,
    pub(crate) calm_listen: String,
    pub(crate) calm_server: ProcessStatus,
    pub(crate) proc_supervisor: ProcessStatus,
}

pub(crate) async fn serve_system(args: SystemServeArgs) -> anyhow::Result<()> {
    let admin_token_override = args.admin_token.clone();
    let config_path = args.config.clone();
    let mut cfg = AppConfig::load(config_path.as_deref())?;
    cfg.apply_serve_overrides(ServeOverrides {
        admin_listen: args.admin_listen,
        admin_token_file: args.admin_token_file,
        child_bin: args.child_bin,
        proc_supervisor_bin: args.proc_supervisor_bin,
        calm_listen: args.calm_listen,
        calm_web_dist: args.calm_web_dist,
        calm_fe_dist: args.calm_fe_dist,
        calm_db_url: args.calm_db_url,
        calm_data_dir: args.calm_data_dir,
        calm_mcp_stdio_shim_bin: args.calm_mcp_stdio_shim_bin,
        child_cwd: args.child_cwd,
        child_args: args.child_args,
        restart_delay_ms: args.restart_delay_ms,
        stop_grace_ms: args.stop_grace_ms,
    });
    let admin_listen = cfg.admin.listen;
    let admin_token = load_admin_token(
        admin_token_override.as_deref(),
        cfg.admin.token_file.as_ref(),
    )?;
    let listener = tokio::net::TcpListener::bind(admin_listen)
        .await
        .with_context(|| format!("bind admin API on {admin_listen}"))?;

    let tailnet = if let Some(config) = &cfg.tailnet {
        // Conflicting declared providers are a configuration error; other tailnet
        // failures must not stop kernel/proc startup.
        config.validate_provider_conflicts(&cfg.child.extra_args)?;
        match config
            .validate(&cfg.child.extra_args)
            .and_then(|()| tailnet::TailnetManager::start(config.clone()))
        {
            Ok(manager) => Some(manager),
            Err(error) => {
                tracing::warn!(%error,"private Tailnet initialization failed; local Neige remains available");
                cfg.tailnet_unavailable = true;
                None
            }
        }
    } else {
        None
    };
    let proc_control_sock = cfg.proc_supervisor_sock();
    let cfg = Arc::new(cfg);
    let proc_supervisor = Supervisor::new(proc_supervisor_config(&cfg));
    let supervisor = Supervisor::new(calm_server_supervisor_config(&cfg));
    let app_state = AppState {
        cfg: cfg.clone(),
        supervisor: supervisor.clone(),
        proc_supervisor: proc_supervisor.clone(),
        tailnet: tailnet.clone(),
        apply_lock: Arc::new(Mutex::new(())),
        admin_token,
    };
    let app = admin_router(app_state);

    tracing::info!(addr = %admin_listen, "neige-app system admin API listening");
    let proc_supervisor_task = if proc_supervisor_is_adoptable(&proc_control_sock).await {
        let child_pid = peer_pid_for_unix_socket(&proc_control_sock).await;
        let identity = read_supervisor_identity(&cfg.calm_data_dir_resolved());
        proc_supervisor.adopt_identity(identity, child_pid).await;
        None
    } else {
        let task = tokio::spawn(proc_supervisor.clone().run());
        wait_for_proc_supervisor(&proc_control_sock).await?;
        Some(task)
    };
    kill_orphan_calm_server_socket_holder(&cfg).await?;
    let supervisor_task = tokio::spawn(supervisor.clone().run());

    let server_result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(supervisor.clone(), proc_supervisor.clone()))
        .await;

    supervisor.shutdown().await;
    proc_supervisor.shutdown().await;
    if let Some(tailnet) = tailnet {
        tailnet.shutdown().await?;
    }
    supervisor_task.await?;
    if let Some(task) = proc_supervisor_task {
        task.await?;
    }
    server_result?;
    Ok(())
}

pub(crate) fn admin_router(app_state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/restart", post(restart))
        .route("/upgrade/apply", post(upgrade_apply))
        .route("/upgrade/full-reboot", post(upgrade_full_reboot))
        .route("/upgrade/history", get(upgrade_history))
        .route("/upgrade/rollback", post(upgrade_rollback))
        .route("/upgrade/applied-id", get(upgrade_applied_id))
        .with_state(app_state)
}

pub(crate) async fn proc_supervisor_is_adoptable(sock: &Path) -> bool {
    sock.exists() && tokio::net::UnixStream::connect(sock).await.is_ok()
}

#[cfg(unix)]
pub(crate) async fn peer_pid_for_unix_socket(sock: &Path) -> Option<u32> {
    let stream = match tokio::net::UnixStream::connect(sock).await {
        Ok(stream) => stream,
        Err(err) => {
            tracing::warn!(socket = %sock.display(), error = %err, "failed to connect for peer credentials");
            return None;
        }
    };
    match unix_stream_peer_pid(&stream) {
        Ok(pid) => Some(pid),
        Err(err) => {
            tracing::warn!(socket = %sock.display(), error = %err, "failed to read peer credentials");
            None
        }
    }
}

#[cfg(not(unix))]
pub(crate) async fn peer_pid_for_unix_socket(_sock: &Path) -> Option<u32> {
    None
}

pub(crate) async fn kill_orphan_calm_server_socket_holder(cfg: &AppConfig) -> anyhow::Result<()> {
    let sock = cfg.calm_mcp_kernel_sock();
    if !sock.exists() {
        return Ok(());
    }
    let Some(pid) = peer_pid_for_unix_socket(&sock).await else {
        return Ok(());
    };
    tracing::warn!(
        pid,
        socket = %sock.display(),
        "killing orphan calm-server socket holder before spawn"
    );
    kill_pid(pid)?;
    wait_for_unix_socket_peer_to_change(&sock, pid, cfg.timing.stop_grace).await;
    Ok(())
}

pub(crate) async fn wait_for_unix_socket_peer_to_change(
    sock: &Path,
    old_pid: u32,
    grace: Duration,
) {
    let deadline = tokio::time::Instant::now() + grace;
    while tokio::time::Instant::now() < deadline {
        if peer_pid_for_unix_socket(sock).await != Some(old_pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub(crate) fn read_supervisor_identity(data_dir: &Path) -> Option<SpawnIdentity> {
    let path = identity::supervisor_identity_path(data_dir);
    match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(identity) => Some(identity),
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "failed to parse supervisor identity");
                None
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "failed to read supervisor identity");
            None
        }
    }
}

pub(crate) async fn wait_for_proc_supervisor(sock: &Path) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if tokio::net::UnixStream::connect(sock).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    anyhow::bail!("calm-proc-supervisor did not listen on {}", sock.display())
}

pub(crate) async fn shutdown_signal(supervisor: Arc<Supervisor>, proc_supervisor: Arc<Supervisor>) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = sigterm.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    tracing::info!("shutdown requested");
    supervisor.shutdown().await;
    proc_supervisor.shutdown().await;
}

pub(crate) async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true, "service": "neige-app" }))
}

pub(crate) async fn status(State(state): State<AppState>) -> Json<StatusSnapshot> {
    Json(state.status_snapshot().await)
}

pub(crate) async fn restart(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_bearer(&headers, state.admin_token.as_deref())?;
    state.supervisor.restart().await?;
    Ok((StatusCode::ACCEPTED, Json(state.status_snapshot().await)).into_response())
}

/// Applies are serialized with `try_lock` so a concurrent request receives an
/// immediate 409 instead of queueing behind a potentially minutes-long upgrade.
pub(crate) async fn upgrade_apply(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<apply::UpgradeRequest>,
) -> Result<Response, ApiError> {
    let _guard = state.apply_lock.try_lock().map_err(|_| {
        ApiError::new(
            StatusCode::CONFLICT,
            "apply_in_progress",
            "another upgrade is in progress",
        )
    })?;
    require_bearer(&headers, state.admin_token.as_deref())?;
    let response =
        apply::apply_upgrade(&state.cfg, &state.supervisor, &state.proc_supervisor, req).await?;
    let status = match response.result {
        apply::UpgradeResult::Committed
            if response.release_history_entry.executed_breaking_self_exec =>
        {
            let kill_proc_supervisor = response
                .release_history_entry
                .units_changed
                .contains(&UnitName::CalmProcSupervisor);
            schedule_exec_self(
                state.cfg.clone(),
                state.supervisor.clone(),
                state.proc_supervisor.clone(),
                kill_proc_supervisor,
                state.tailnet.clone(),
            );
            StatusCode::ACCEPTED
        }
        apply::UpgradeResult::Committed | apply::UpgradeResult::DryRun => StatusCode::OK,
        apply::UpgradeResult::Rejected => StatusCode::BAD_REQUEST,
        apply::UpgradeResult::RolledBack => StatusCode::BAD_GATEWAY,
    };
    Ok((status, Json(response)).into_response())
}

/// Full reboot shares the upgrade serialization lock and rejects concurrent
/// apply/rollback work immediately instead of queueing.
pub(crate) async fn upgrade_full_reboot(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let _guard = state.apply_lock.try_lock().map_err(|_| {
        ApiError::new(
            StatusCode::CONFLICT,
            "apply_in_progress",
            "another upgrade is in progress",
        )
    })?;
    require_bearer(&headers, state.admin_token.as_deref())?;
    let supervisor = state.supervisor.clone();
    let proc_supervisor = state.proc_supervisor.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Err(err) = supervisor.force_stop_and_wait().await {
            tracing::warn!(error = %err, "failed to stop calm-server before full reboot");
        }
        if let Err(err) = proc_supervisor.force_stop_and_wait().await {
            tracing::warn!(error = %err, "failed to stop proc-supervisor before full reboot");
        }
        std::process::exit(0);
    });
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({"status": "full-reboot-scheduled"})),
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
pub(crate) struct HistoryQuery {
    pub(crate) limit: Option<usize>,
}

pub(crate) async fn upgrade_history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HistoryQuery>,
) -> Result<Response, ApiError> {
    require_bearer(&headers, state.admin_token.as_deref())?;
    let limit = query.limit.unwrap_or(50).max(1);
    let entries = apply::read_release_history_blocking(&state.cfg, limit).await?;
    Ok((StatusCode::OK, Json(entries)).into_response())
}

#[derive(Debug, Deserialize)]
pub(crate) struct RollbackRequest {
    pub(crate) to: String,
}

/// Rollback mutates the same symlinks, installed state, history, and DB backup
/// paths as apply, so it uses the same non-queueing upgrade serialization lock.
pub(crate) async fn upgrade_rollback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RollbackRequest>,
) -> Result<Response, ApiError> {
    let _guard = state.apply_lock.try_lock().map_err(|_| {
        ApiError::new(
            StatusCode::CONFLICT,
            "apply_in_progress",
            "another upgrade is in progress",
        )
    })?;
    require_bearer(&headers, state.admin_token.as_deref())?;
    let response = apply::rollback_last_preserving(&state.cfg, &state.supervisor, &req.to).await?;
    Ok((StatusCode::OK, Json(response)).into_response())
}

pub(crate) async fn upgrade_applied_id(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    // TODO: this sentinel is admin-authenticated, which makes unauthenticated browser polling awkward.
    require_bearer(&headers, state.admin_token.as_deref())?;
    let release_id = apply::read_last_upgrade_id_blocking(&state.cfg).await?;
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({ "releaseId": release_id })),
    )
        .into_response())
}

pub(crate) fn schedule_exec_self(
    cfg: Arc<AppConfig>,
    supervisor: Arc<Supervisor>,
    proc_supervisor: Arc<Supervisor>,
    kill_proc_supervisor: bool,
    tailnet: Option<Arc<tailnet::TailnetManager>>,
) {
    tokio::spawn(async move {
        // Give hyper time to write the 202 body before this process image is replaced.
        tokio::time::sleep(Duration::from_millis(2000)).await;
        if let Err(err) = supervisor.force_stop_and_wait().await {
            tracing::warn!(error = %err, "failed to stop calm-server before exec-self");
        }
        if kill_proc_supervisor && let Err(err) = proc_supervisor.force_stop_and_wait().await {
            tracing::warn!(error = %err, "failed to stop proc-supervisor before exec-self");
        }
        if let Some(tailnet) = tailnet
            && let Err(error) = tailnet.shutdown().await
        {
            tracing::error!(%error,"failed to stop private Tailnet before exec-self");
            return;
        }
        exec_self(&cfg);
    });
}

#[cfg(unix)]
pub(crate) fn exec_self(cfg: &AppConfig) -> ! {
    let mut args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if args.is_empty() {
        args.push("neige-app".into());
    }
    if args.len() == 1 {
        args.extend(["system".into(), "serve".into()]);
    }
    if !args
        .iter()
        .any(|arg| arg == std::ffi::OsStr::new("--config"))
    {
        args.extend(["--config".into(), cfg.config_path.clone().into_os_string()]);
    }
    let program = cfg.release.current_server.join("bin").join("neige-app");
    let err = std::process::Command::new(&program).args(&args[1..]).exec();
    tracing::error!(program = %program.display(), error = %err, "exec self failed");
    std::process::exit(127);
}

#[cfg(not(unix))]
pub(crate) fn exec_self(_cfg: &AppConfig) -> ! {
    tracing::error!("exec self is only implemented on Unix");
    std::process::exit(127);
}

pub(crate) fn load_admin_token(
    token: Option<&str>,
    token_file: Option<&PathBuf>,
) -> anyhow::Result<Option<Arc<str>>> {
    if let Some(token) = token {
        let token = token.trim();
        if !token.is_empty() {
            return Ok(Some(Arc::from(token)));
        }
    }
    if let Some(path) = token_file {
        let token = std::fs::read_to_string(path)
            .with_context(|| format!("read admin token file {}", path.display()))?;
        let token = token.trim();
        if !token.is_empty() {
            return Ok(Some(Arc::from(token)));
        }
    }
    Ok(None)
}

pub(crate) fn require_bearer(
    headers: &HeaderMap,
    configured_token: Option<&str>,
) -> Result<(), ApiError> {
    let Some(configured_token) = configured_token else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "admin_token_not_configured",
            "M1 refuses state-changing admin API calls unless NEIGE_APP_ADMIN_TOKEN or NEIGE_APP_ADMIN_TOKEN_FILE is configured.",
        ));
    };

    let Some(value) = headers.get(axum::http::header::AUTHORIZATION) else {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "missing_bearer_token",
            "state-changing admin API calls require Authorization: Bearer <token>.",
        ));
    };
    let Ok(value) = value.to_str() else {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_bearer_token",
            "authorization header is not valid UTF-8.",
        ));
    };
    let Some(token) = value.strip_prefix("Bearer ") else {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_bearer_token",
            "state-changing admin API calls require Authorization: Bearer <token>.",
        ));
    };
    if token != configured_token {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_bearer_token",
            "bearer token did not match the configured admin token.",
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct ApiError {
    pub(crate) status: StatusCode,
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl ApiError {
    pub(crate) fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(value: anyhow::Error) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            value.to_string(),
        )
    }
}

impl From<apply::ApplyError> for ApiError {
    fn from(value: apply::ApplyError) -> Self {
        Self::new(value.status, value.code, value.message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        tracing::warn!(
            status = self.status.as_u16(),
            error = self.code,
            message = %self.message,
            "admin API request failed"
        );
        (
            self.status,
            Json(serde_json::json!({
                "error": self.code,
                "message": self.message,
            })),
        )
            .into_response()
    }
}
