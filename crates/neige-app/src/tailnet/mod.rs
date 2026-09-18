//! App-owned private Tailnet child. Kernel restart never owns or signals it.
pub(crate) mod config;
mod storage;
#[cfg(test)]
mod tests;

use calm_tailnet_control::{MAX_MESSAGE, TailnetClient, VERSION};
use calm_types::tailnet::{TailnetAction, TailnetRequest, TailnetResponse, TailnetStatus};
use config::TailnetConfig;
use std::collections::VecDeque;
use std::fs::File;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use storage::DesiredState;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

pub(crate) struct TailnetManager {
    cfg: TailnetConfig,
    pinned_binary: Option<std::path::PathBuf>,
    state: Mutex<State>,
    _lock: File,
}
struct State {
    desired: DesiredState,
    child: Option<Child>,
    crashes: VecDeque<Instant>,
    next_start: Instant,
    failed: bool,
    shutdown: bool,
    next_cleanup: Instant,
}
impl TailnetManager {
    /// Configuration/UDS setup is local only. Node startup is asynchronous and
    /// failures are surfaced in Settings without stopping the local kernel.
    pub fn start(cfg: TailnetConfig) -> anyhow::Result<Arc<Self>> {
        let lock = storage::lock_directory(&cfg.state_dir)?;
        let desired = storage::load(&cfg.state_dir)?;
        storage::atomic_json(
            &cfg.ingress_config(),
            &serde_json::json!({"provider":"private-tailnet","controlSocket":cfg.socket(),"ingressSocket":cfg.ingress_socket()}),
        )?;
        match std::fs::remove_file(cfg.socket()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let listener = UnixListener::bind(cfg.socket())?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(cfg.socket(), std::fs::Permissions::from_mode(0o600))?;
        let pinned_binary = std::fs::canonicalize(&cfg.binary).ok();
        let manager = Arc::new(Self {
            cfg,
            pinned_binary,
            state: Mutex::new(State {
                desired,
                child: None,
                crashes: VecDeque::new(),
                next_start: Instant::now(),
                failed: false,
                shutdown: false,
                next_cleanup: Instant::now(),
            }),
            _lock: lock,
        });
        let task = manager.clone();
        tokio::spawn(async move {
            task.run(listener).await;
        });
        Ok(manager)
    }
    async fn run(self: Arc<Self>, listener: UnixListener) {
        let mut interval = tokio::time::interval(Duration::from_millis(250));
        loop {
            tokio::select! {
                _=interval.tick()=>{ let mut state=self.state.lock().await; if state.shutdown { break } self.reconcile(&mut state).await; },
                accepted=listener.accept()=>{ if let Ok((stream,_))=accepted { let manager=self.clone();tokio::spawn(async move{manager.handle(stream).await;}); } }
            }
        }
    }
    async fn reconcile(&self, state: &mut State) {
        if !state.desired.desired_enabled {
            let _ = Self::stop_child(state).await;
            if self.cfg.enrollment_config.is_some() && Instant::now() >= state.next_cleanup {
                state.next_cleanup = Instant::now() + Duration::from_secs(60);
                if let Ok(mut child) = self.spawn_mode(true) {
                    if tokio::time::timeout(Duration::from_secs(10), child.wait())
                        .await
                        .is_err()
                    {
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                    }
                }
            }
            return;
        }
        let exited = match state.child.as_mut() {
            Some(child) => !matches!(child.try_wait(), Ok(None)),
            None => false,
        };
        if exited {
            state.child = None;
            let now = Instant::now();
            state.crashes.push_back(now);
            while state
                .crashes
                .front()
                .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(300))
            {
                state.crashes.pop_front();
            }
            state.failed = state.crashes.len() >= 5;
            let delay = 1u64 << state.crashes.len().min(5);
            let jitter = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_millis()
                % 500;
            state.next_start =
                now + Duration::from_secs(delay) + Duration::from_millis(u64::from(jitter));
        }
        if state.desired.desired_enabled
            && !state.failed
            && state.child.is_none()
            && Instant::now() >= state.next_start
        {
            match self.spawn() {
                Ok(child) => state.child = Some(child),
                Err(_) => {
                    state.failed = true;
                }
            }
        }
    }
    fn spawn(&self) -> anyhow::Result<Child> {
        self.spawn_mode(false)
    }
    fn spawn_mode(&self, cleanup_only: bool) -> anyhow::Result<Child> {
        // Explicit allowlist: no tokens, proxy settings, TS_AUTHKEY, cloud
        // credentials, system tailscaled socket or parent HOME reach tsnet.
        let binary=self.pinned_binary.as_ref().ok_or_else(||anyhow::anyhow!("Tailnet helper is not installed; complete the release installation and restart Neige"))?;
        if !cleanup_only {
            storage::backup_for_binary(&self.cfg.state_dir, binary)?;
        }
        let mut command = Command::new(binary);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .env("HOME", &self.cfg.state_dir)
            .arg("--state-dir")
            .arg(&self.cfg.state_dir)
            .arg("--control-socket")
            .arg(self.cfg.helper_socket())
            .arg("--upstream-socket")
            .arg(self.cfg.ingress_socket())
            .arg("--hostname")
            .arg(&self.cfg.hostname)
            .current_dir(&self.cfg.state_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .process_group(0);
        if let Some(path) = &self.cfg.enrollment_config {
            command.arg("--enrollment-config").arg(path);
        }
        if cleanup_only {
            command.arg("--cleanup-only");
        }
        #[cfg(target_os = "linux")]
        unsafe {
            let parent = libc::getpid();
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    return Err(std::io::Error::from_raw_os_error(libc::ECHILD));
                }
                Ok(())
            });
        }
        Ok(command.spawn()?)
    }
    async fn snapshot(&self, state: &State) -> TailnetStatus {
        let mut status = TailnetStatus::stopped(state.desired.desired_enabled, state.failed);
        if let Some(child) = &state.child {
            status.process_running = true;
            status.child_pid = child.id();
            if let Ok(response) = TailnetClient::new(self.cfg.helper_socket())
                .request(TailnetAction::Status)
                .await
                && response.status.child_pid == child.id()
            {
                status = response.status;
                status.desired_enabled = state.desired.desired_enabled;
            }
        }
        status
    }
    async fn action(&self, action: TailnetAction) -> anyhow::Result<TailnetResponse> {
        let mut state = self.state.lock().await;
        anyhow::ensure!(!state.shutdown, "Tailnet manager is shutting down");
        let mut login_url = None;
        match action {
            TailnetAction::Enable => {
                self.persist(&mut state, true)?;
                state.failed = false;
                state.crashes.clear();
                state.next_start = Instant::now();
            }
            TailnetAction::Disable => {
                self.persist(&mut state, false)?;
                Self::stop_child(&mut state).await?;
                state.next_cleanup = Instant::now();
            }
            TailnetAction::Login => {
                anyhow::ensure!(
                    state.desired.desired_enabled && state.child.is_some(),
                    "Enable remote access before signing in"
                );
                login_url = TailnetClient::new(self.cfg.helper_socket())
                    .request(action)
                    .await?
                    .login_url;
            }
            TailnetAction::Logout => {
                anyhow::ensure!(
                    state.child.is_some(),
                    "Enable remote access before signing out"
                );
                // First persist closure, so an interrupted logout never reopens ingress.
                self.persist(&mut state, false)?;
                let logout = TailnetClient::new(self.cfg.helper_socket())
                    .request(action)
                    .await;
                Self::stop_child(&mut state).await?;
                logout?;
                // tsnet has stopped its only writer. Explicit sign-out discards
                // only this app's identity, never the desired-state revocation.
                let node = self.cfg.state_dir.join("node");
                if node.exists() {
                    std::fs::remove_dir_all(node)?;
                }
                let backups = self.cfg.state_dir.join("backups");
                if backups.exists() {
                    std::fs::remove_dir_all(backups)?;
                }
            }
            TailnetAction::Status => {}
        }
        self.reconcile(&mut state).await;
        Ok(TailnetResponse {
            version: VERSION,
            status: self.snapshot(&state).await,
            login_url,
            error: None,
        })
    }
    fn persist(&self, state: &mut State, enabled: bool) -> anyhow::Result<()> {
        if state.desired.desired_enabled == enabled {
            return Ok(());
        }
        let desired = DesiredState {
            schema_version: 1,
            config_revision: state
                .desired
                .config_revision
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("Tailnet revision exhausted"))?,
            desired_enabled: enabled,
        };
        storage::atomic_json(&self.cfg.state_dir.join("desired.json"), &desired)?;
        state.desired = desired;
        Ok(())
    }
    async fn stop_child(state: &mut State) -> anyhow::Result<()> {
        if let Some(mut child) = state.child.take() {
            if let Some(pid) = child.id() {
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
            }
            match tokio::time::timeout(Duration::from_secs(3), child.wait()).await {
                Ok(status) => {
                    status?;
                }
                Err(_) => {
                    child.kill().await?;
                    child.wait().await?;
                }
            }
        }
        Ok(())
    }
    pub async fn shutdown(&self) -> anyhow::Result<()> {
        let mut state = self.state.lock().await;
        state.shutdown = true;
        Self::stop_child(&mut state).await
    }
    async fn handle(&self, mut stream: UnixStream) {
        let operation = async {
            let mut line = Vec::new();
            BufReader::new((&mut stream).take(MAX_MESSAGE + 1))
                .read_until(b'\n', &mut line)
                .await?;
            anyhow::ensure!(
                line.len() <= MAX_MESSAGE as usize && line.last() == Some(&b'\n'),
                "invalid message size"
            );
            if serde_json::from_slice::<serde_json::Value>(&line)?["version"] == 2 {
                use calm_types::enrollment::{EnrollmentRequest, EnrollmentResponse};
                let request: EnrollmentRequest = serde_json::from_slice(&line)?;
                let state = self.state.lock().await;
                let result = if !state.shutdown
                    && state.desired.desired_enabled
                    && state.child.is_some()
                {
                    TailnetClient::new(self.cfg.helper_socket())
                        .enrollment(request.command)
                        .await
                } else {
                    Err(anyhow::anyhow!(
                        "setup-required: enable private access first; stopped-node cleanup runs independently"
                    ))
                };
                let response = match result {
                    Ok(result) => EnrollmentResponse {
                        version: 2,
                        result: Some(result),
                        error: None,
                    },
                    Err(error) => EnrollmentResponse {
                        version: 2,
                        result: None,
                        error: Some(error.to_string()),
                    },
                };
                let mut bytes = serde_json::to_vec(&response)?;
                bytes.push(b'\n');
                stream.write_all(&bytes).await?;
                return Ok(());
            }
            let req: TailnetRequest = serde_json::from_slice(&line)?;
            anyhow::ensure!(req.version == VERSION, "invalid protocol version");
            let response = match self.action(req.action).await {
                Ok(response) => response,
                Err(_) => TailnetResponse {
                    version: VERSION,
                    status: TailnetStatus::stopped(false, true),
                    login_url: None,
                    error: Some("Tailnet operation failed; check status and retry".into()),
                },
            };
            let mut bytes = serde_json::to_vec(&response)?;
            bytes.push(b'\n');
            stream.write_all(&bytes).await?;
            Ok::<(), anyhow::Error>(())
        };
        let _ = tokio::time::timeout(Duration::from_secs(15), operation).await;
    }
}
