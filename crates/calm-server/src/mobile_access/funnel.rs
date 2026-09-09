use super::ingress::RevocableListener;
use super::pairing::PairingState;
use crate::auth::SessionStore;
use crate::error::{CalmError, Result};
use axum::Router;
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FunnelConfig {
    pub executable: PathBuf,
    pub socket: PathBuf,
    pub https_port: u16,
}

impl FunnelConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        anyhow::ensure!(
            cfg!(target_os = "linux"),
            "Managed mobile ingress currently requires a Linux server"
        );
        let config: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        anyhow::ensure!(
            config.executable.is_absolute() && config.socket.is_absolute(),
            "Mobile access requires absolute executable and socket paths"
        );
        anyhow::ensure!(
            [443, 8443, 10000].contains(&config.https_port),
            "Unsupported Funnel HTTPS port"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let binary = std::fs::metadata(&config.executable)?;
            anyhow::ensure!(
                binary.is_file() && binary.permissions().mode() & 0o6000 == 0,
                "Use an unprivileged Tailscale executable, not a setuid/setgid wrapper"
            );
        }
        Ok(config)
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.executable);
        cmd.env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .arg(format!("--socket={}", self.socket.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(target_os = "linux")]
        {
            let parent = nix::unistd::getpid();
            // SAFETY: only async-signal-safe prctl/getppid syscalls are used
            // between fork and exec. Close the parent-death race explicitly.
            unsafe {
                cmd.pre_exec(move || {
                    nix::sys::prctl::set_pdeathsig(nix::sys::signal::Signal::SIGKILL)
                        .map_err(std::io::Error::from)?;
                    if nix::unistd::getppid() != parent {
                        return Err(std::io::Error::from_raw_os_error(nix::libc::ECHILD));
                    }
                    Ok(())
                });
            }
        }
        cmd
    }

    async fn json(&self, args: &[&str]) -> Result<Value> {
        let mut child = self
            .command()
            .args(args)
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|_| unavailable())?;
        let mut stdout = child.stdout.take().ok_or_else(unavailable)?.take(262_145);
        let outcome = tokio::time::timeout(Duration::from_secs(5), async {
            let mut output = Vec::new();
            stdout
                .read_to_end(&mut output)
                .await
                .map_err(|_| unavailable())?;
            if output.len() > 262_144 {
                return Err(unavailable());
            }
            let status = child.wait().await.map_err(|_| unavailable())?;
            if !status.success() {
                return Err(unavailable());
            }
            serde_json::from_slice(&output).map_err(|_| unavailable())
        })
        .await;
        outcome.map_err(|_| unavailable())?
    }
}

fn unavailable() -> CalmError {
    CalmError::BadRequest("Unable to start mobile access. Check that the server's Tailscale daemon is signed in, HTTPS/Funnel is authorized, and the configured socket is accessible.".into())
}

fn configurations(value: &Value) -> Vec<&Value> {
    let mut configs = vec![value];
    if let Some(foreground) = value.get("Foreground").and_then(Value::as_object) {
        configs.extend(foreground.values());
    }
    configs
}

fn port_configurations(value: &Value, port: u16) -> Vec<&Value> {
    let key = port.to_string();
    configurations(value)
        .into_iter()
        .filter(|config| config.get("TCP").and_then(|tcp| tcp.get(&key)).is_some())
        .collect()
}

fn verified_mapping(value: &Value, host: &str, port: u16, target: &str) -> bool {
    let configs = port_configurations(value, port);
    if configs.len() != 1 {
        return false;
    }
    let config = configs[0];
    let host_port = format!("{host}:{port}");
    config
        .get("TCP")
        .and_then(|tcp| tcp.get(port.to_string()))
        .and_then(|tcp| tcp.get("HTTPS"))
        .and_then(Value::as_bool)
        == Some(true)
        && config
            .get("AllowFunnel")
            .and_then(|allow| allow.get(&host_port))
            .and_then(Value::as_bool)
            == Some(true)
        && config
            .get("Web")
            .and_then(|web| web.get(&host_port))
            .and_then(|web| web.get("Handlers"))
            .and_then(|handlers| handlers.get("/"))
            .and_then(|handler| handler.get("Proxy"))
            .and_then(Value::as_str)
            == Some(target)
}

#[derive(Default)]
pub(super) struct Controller {
    deployment: Option<(FunnelConfig, Weak<Router>)>,
    running: Option<Running>,
}

struct Running {
    stop: CancellationToken,
    task: JoinHandle<()>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

impl Controller {
    pub fn configured(config: FunnelConfig, router: Arc<Router>) -> Self {
        Self {
            deployment: Some((config, Arc::downgrade(&router))),
            running: None,
        }
    }

    pub fn available(&self) -> bool {
        self.deployment.is_some()
    }

    pub async fn start(
        &mut self,
        state: Arc<Mutex<PairingState>>,
        sessions: SessionStore,
    ) -> Result<()> {
        if self
            .running
            .as_ref()
            .is_some_and(|running| !running.task.is_finished())
        {
            return Ok(());
        }
        self.stop().await?;
        let (config, router) = self.deployment.as_ref().ok_or_else(|| {
            CalmError::BadRequest("Mobile access has not been configured on this server".into())
        })?;
        let router = router.upgrade().ok_or_else(unavailable)?;
        let status = config.json(&["status", "--json"]).await?;
        if status.get("BackendState").and_then(Value::as_str) != Some("Running") {
            return Err(unavailable());
        }
        let host = status
            .get("Self")
            .and_then(|node| node.get("DNSName"))
            .and_then(Value::as_str)
            .ok_or_else(unavailable)?
            .trim_end_matches('.');
        if !host.ends_with(".ts.net")
            || !host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        {
            return Err(unavailable());
        }
        let before = config.json(&["serve", "status", "--json"]).await?;
        if !port_configurations(&before, config.https_port).is_empty() {
            return Err(CalmError::BadRequest(
                "The configured HTTPS port is already in use; existing services were left intact"
                    .into(),
            ));
        }
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| unavailable())?;
        let target = format!(
            "http://{}",
            listener.local_addr().map_err(|_| unavailable())?
        );
        let mut child = config
            .command()
            .arg("funnel")
            .arg(format!("--https={}", config.https_port))
            .arg(&target)
            .spawn()
            .map_err(|_| unavailable())?;
        let ready = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if child.try_wait().map_err(|_| unavailable())?.is_some() {
                    return Err(unavailable());
                }
                let current = config.json(&["serve", "status", "--json"]).await?;
                if verified_mapping(&current, host, config.https_port, &target) {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
        })
        .await
        .map_err(|_| unavailable())?;
        ready?;
        let origin = if config.https_port == 443 {
            format!("https://{host}")
        } else {
            format!("https://{host}:{}", config.https_port)
        };
        {
            let mut locked = state.lock().map_err(|_| unavailable())?;
            locked.disable(&sessions);
            locked.origin = Some(origin);
        }
        let stop = CancellationToken::new();
        let stop_task = stop.clone();
        let public = (*router).clone();
        let task = tokio::spawn(async move {
            run_ingress(listener, public, state, sessions, &mut child, stop_task).await;
        });
        self.running = Some(Running { stop, task });
        Ok(())
    }

    pub async fn stop(&mut self) -> Result<()> {
        if let Some(mut running) = self.running.take() {
            running.stop.cancel();
            if tokio::time::timeout(Duration::from_secs(5), &mut running.task)
                .await
                .is_err()
            {
                running.task.abort();
                let _ = (&mut running.task).await;
                return Err(CalmError::Internal(
                    "Mobile access is still shutting down".into(),
                ));
            }
        }
        Ok(())
    }
}

async fn run_ingress(
    listener: TcpListener,
    router: Router,
    state: Arc<Mutex<PairingState>>,
    sessions: SessionStore,
    child: &mut Child,
    stop: CancellationToken,
) {
    let server = axum::serve(
        RevocableListener {
            listener,
            state: state.clone(),
        },
        router,
    );
    tokio::select! {
        _ = stop.cancelled() => {},
        _ = child.wait() => {},
        _ = std::future::IntoFuture::into_future(server) => {},
    }
    if let Ok(mut locked) = state.lock() {
        locked.disable(&sessions);
    }
    let _ = child.kill().await;
}
