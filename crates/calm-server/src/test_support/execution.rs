//! Real execution assembly for library tests using the production scope-helper binary.
use crate::{config::Config, db::sqlite::SqliteDb, execution_boot::ExecutionEngine};
use clap::Parser;
use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};
use workspace_runtime::execution::CapabilityToken;

pub(crate) struct Supervisor {
    key: PathBuf,
    socket: PathBuf,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    server: Option<std::thread::JoinHandle<()>>,
    _root: tempfile::TempDir,
    _socket: tempfile::TempDir,
}
impl Supervisor {
    pub fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let socket_dir = calm_test_sockets::socket_dir("c4-lib-execution");
        let socket = socket_dir.path().join("control.sock");
        let secret = CapabilityToken::fresh().unwrap();
        let key = root.path().join("client-key.json");
        std::fs::write(&key, serde_json::to_vec(&secret).unwrap()).unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        let helper = helper_binary();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let (ready, started) = std::sync::mpsc::channel();
        let server_socket = socket.clone();
        let journal = root.path().join("journal");
        // A separate runtime owns the real supervisor. Drop can join its complete
        // shutdown even when the test itself uses Tokio's current-thread runtime.
        let server = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    let registry = calm_proc_supervisor::ProcRegistry::new()
                        .with_executor(calm_proc_supervisor::execution::ExecutorConfig {
                            journal_directory: journal,
                            helper_program: helper,
                            kernel_client_secret: secret,
                        })
                        .unwrap();
                    let listener =
                        calm_proc_supervisor::bind_control_listener(&server_socket).unwrap();
                    let (server_stop, server_stopped) = tokio::sync::oneshot::channel();
                    let closing = registry.clone();
                    tokio::spawn(async move {
                        let _ = stopped.await;
                        closing.terminate_all_process_groups().await;
                        let _ = server_stop.send(());
                    });
                    ready.send(()).unwrap();
                    calm_proc_supervisor::serve_with_listener(
                        listener,
                        server_socket,
                        registry,
                        server_stopped,
                    )
                    .await
                    .unwrap();
                });
        });
        started.recv().unwrap();
        Self {
            key,
            socket,
            stop: Some(stop),
            server: Some(server),
            _root: root,
            _socket: socket_dir,
        }
    }
    pub fn configure(&self, config: &mut Config) {
        config.kernel_client_key_file = Some(self.key.clone());
        config.proc_supervisor_sock = Some(self.socket.clone());
    }
    async fn recover(&self, pool: SqliteDb) -> Arc<ExecutionEngine> {
        let mut config = Config::try_parse_from(["calm-server"]).unwrap();
        self.configure(&mut config);
        config.execution_recovery_interval = std::time::Duration::from_secs(1);
        crate::execution_boot::recover(&config, pool)
            .await
            .unwrap()
            .unwrap()
    }
}

pub(crate) struct Fixture {
    pub engine: Arc<ExecutionEngine>,
    supervisor: Supervisor,
}
impl Fixture {
    pub async fn new(pool: SqliteDb) -> Self {
        let supervisor = Supervisor::new();
        let engine = supervisor.recover(pool).await;
        Self { engine, supervisor }
    }
    pub async fn new_kernel(&self, pool: SqliteDb) -> Arc<ExecutionEngine> {
        self.supervisor.recover(pool).await
    }
}
impl Drop for Supervisor {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}

/// Cargo reports the artifact path. Do not infer its private target-directory
/// layout, and do not substitute a script for the registered helper ELF image.
fn helper_binary() -> PathBuf {
    static HELPER: OnceLock<PathBuf> = OnceLock::new();
    HELPER
        .get_or_init(|| {
            let output = std::process::Command::new("cargo")
                .args([
                    "build",
                    "--locked",
                    "-p",
                    "calm-proc-supervisor",
                    "--bin",
                    "calm-proc-supervisor",
                    "--message-format=json-render-diagnostics",
                    "-j",
                    "6",
                ])
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .env_remove("NEIGE_CODEX_BIN")
                .env("RUSTC_WRAPPER", "")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "helper build: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .find_map(|artifact| {
                    if artifact["reason"] == "compiler-artifact"
                        && artifact["target"]["name"] == "calm-proc-supervisor"
                    {
                        artifact["executable"].as_str().map(PathBuf::from)
                    } else {
                        None
                    }
                })
                .expect("Cargo reported the production supervisor executable")
        })
        .clone()
}

pub(crate) struct Directory {
    pub _execution: Fixture,
    pub directory: tempfile::TempDir,
}
impl Directory {
    pub fn path(&self) -> &std::path::Path {
        self.directory.path()
    }
}

/// Fault the production Stop transaction, rather than a removed marker path.
pub(crate) async fn block_stops(pool: &SqliteDb) {
    sqlx::query(
        "CREATE TRIGGER c4_block_stops BEFORE UPDATE OF desired ON workspace_executions
         WHEN NEW.desired='stop' BEGIN SELECT RAISE(FAIL, 'provider Stop blocked'); END",
    )
    .execute(pool.writer_pool())
    .await
    .unwrap();
}
pub(crate) async fn unblock_stops(pool: &SqliteDb) {
    sqlx::query("DROP TRIGGER c4_block_stops")
        .execute(pool.writer_pool())
        .await
        .unwrap();
}
