use std::path::{Path, PathBuf};
use workspace_runtime::execution::CapabilityToken;

pub fn supervisor(
    journal_root: &Path,
) -> (
    tempfile::TempDir,
    PathBuf,
    CapabilityToken,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let socket_dir = calm_test_sockets::socket_dir("typed-hooks");
    let socket = socket_dir.path().join("control.sock");
    let secret = CapabilityToken::fresh().unwrap();
    let registry = calm_proc_supervisor::ProcRegistry::new()
        .with_executor(calm_proc_supervisor::execution::ExecutorConfig {
            journal_directory: journal_root.join("journal"),
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
    (socket_dir, socket, secret, stop, server)
}

pub struct Fixture {
    pub key: PathBuf,
    pub socket: PathBuf,
    pub lifecycle: calm_server::execution_quiesce::ExecutionLifecycle,
    _socket: tempfile::TempDir,
    _root: tempfile::TempDir,
    server: std::process::Child,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = self.server.wait();
    }
}
impl Fixture {
    pub fn kernel_args(&self) -> Vec<std::ffi::OsString> {
        vec![
            "--kernel-client-key-file".into(),
            self.key.clone().into(),
            "--proc-supervisor-sock".into(),
            self.socket.clone().into(),
        ]
    }

    fn spawn(root: &Path, socket: &Path, key: &Path) -> std::process::Child {
        std::process::Command::new(env!("CARGO_BIN_EXE_osc-probe-child"))
            .arg("--execution-supervisor")
            .arg(root.join("journal"))
            .arg(socket)
            .arg(key)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap()
    }
    pub async fn restart(&mut self) {
        self.server.kill().unwrap();
        self.server.wait().unwrap();
        while self.lifecycle.engine().unwrap().available() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        self.server = Self::spawn(self._root.path(), &self.socket, &self.key);
        while !self.lifecycle.engine().unwrap().available() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
    pub async fn new(pool: calm_server::db::sqlite::SqliteDb) -> Self {
        use clap::Parser;
        let root = tempfile::tempdir().unwrap();
        let socket = calm_test_sockets::socket_dir("typed-gate");
        let path = socket.path().join("control.sock");
        let secret = CapabilityToken::fresh().unwrap();
        let key = root.path().join("key.json");
        std::fs::write(&key, serde_json::to_vec(&secret).unwrap()).unwrap();
        let server = Self::spawn(root.path(), &path, &key);
        let cfg = calm_server::config::Config::try_parse_from([
            "calm-server",
            "--kernel-client-key-file",
            key.to_str().unwrap(),
            "--proc-supervisor-sock",
            path.to_str().unwrap(),
        ])
        .unwrap();
        let engine = calm_server::execution_boot::recover(&cfg, pool)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !engine.available() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let lifecycle = calm_server::execution_quiesce::ExecutionLifecycle::default();
        lifecycle.bind(engine);
        Self {
            key,
            socket: path,
            lifecycle,
            _socket: socket,
            _root: root,
            server,
        }
    }
}
