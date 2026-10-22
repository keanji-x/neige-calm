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
    pub faults: std::sync::Arc<calm_server::test_seams::ExecutionRecoveryFaults>,
    pub key: PathBuf,
    pub socket: PathBuf,
    pub lifecycle: calm_server::execution_quiesce::ExecutionLifecycle,
    _socket: tempfile::TempDir,
    _root: tempfile::TempDir,
    server: std::process::Child,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // The supervisor's existing executor shutdown owns TERM/KILL and descendant reap.
        unsafe {
            libc::kill(self.server.id() as i32, libc::SIGTERM);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if self.server.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = self.server.kill();
        let _ = self.server.wait();
    }
}
impl Fixture {
    pub fn assert_journal_excludes(&self, needle: &str) {
        fn visit(dir: &Path, needle: &[u8], journals: &mut usize) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(&path, needle, journals);
                } else if path
                    .extension()
                    .is_some_and(|extension| extension == "execution")
                {
                    *journals += 1;
                    let bytes = std::fs::read(path).unwrap();
                    assert!(!bytes.windows(needle.len()).any(|window| window == needle));
                }
            }
        }
        let mut journals = 0;
        visit(
            &self._root.path().join("journal"),
            needle.as_bytes(),
            &mut journals,
        );
        assert!(journals > 0, "secrecy assertion inspected a live journal");
    }

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
    /// Start the supervisor independently of any kernel or database lifetime.
    pub fn start() -> Self {
        let root = tempfile::tempdir().unwrap();
        let socket = calm_test_sockets::socket_dir("typed-gate");
        let path = socket.path().join("control.sock");
        let secret = CapabilityToken::fresh().unwrap();
        let key = root.path().join("key.json");
        std::fs::write(&key, serde_json::to_vec(&secret).unwrap()).unwrap();
        let server = Self::spawn(root.path(), &path, &key);
        Self {
            faults: Default::default(),
            key,
            socket: path,
            lifecycle: Default::default(),
            _socket: socket,
            _root: root,
            server,
        }
    }

    pub async fn new_kernel(
        &self,
        pool: calm_server::db::sqlite::SqliteDb,
        hold_retire: bool,
    ) -> calm_server::execution_boot::ExecutionEngine {
        use clap::Parser;
        let mut cfg = calm_server::config::Config::try_parse_from(["calm-server"]).unwrap();
        self.configure(&mut cfg);
        cfg.execution_recovery_interval = std::time::Duration::from_secs(3600);
        if hold_retire {
            cfg.execution_recovery_faults.arm("load");
        }
        let engine = calm_server::execution_boot::recover(&cfg, pool)
            .await
            .unwrap()
            .unwrap();
        if hold_retire {
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                while cfg
                    .execution_recovery_faults
                    .failures
                    .load(std::sync::atomic::Ordering::SeqCst)
                    != 1
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
        Self::wait_available(&engine).await;
        engine.as_ref().clone()
    }
    async fn wait_available(engine: &calm_server::execution_boot::ExecutionEngine) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !engine.available() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    pub fn configure(&self, cfg: &mut calm_server::config::Config) {
        cfg.kernel_client_key_file = Some(self.key.clone());
        cfg.proc_supervisor_sock = Some(self.socket.clone());
        cfg.execution_recovery_interval = std::time::Duration::from_secs(1);
    }

    pub async fn new(pool: calm_server::db::sqlite::SqliteDb) -> Self {
        Self::new_with_interval(pool, std::time::Duration::from_secs(1)).await
    }
    pub async fn new_with_interval(
        pool: calm_server::db::sqlite::SqliteDb,
        interval: std::time::Duration,
    ) -> Self {
        use clap::Parser;
        let mut fixture = Self::start();
        let mut cfg = calm_server::config::Config::try_parse_from(["calm-server"]).unwrap();
        fixture.configure(&mut cfg);
        cfg.execution_recovery_interval = interval;
        fixture.faults = cfg.execution_recovery_faults.clone();
        let engine = calm_server::execution_boot::recover(&cfg, pool)
            .await
            .unwrap()
            .unwrap();
        Self::wait_available(&engine).await;
        fixture.lifecycle.bind(engine);
        fixture
    }
}
