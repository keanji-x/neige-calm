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
    async fn recover(&self, pool: SqliteDb, interval: std::time::Duration) -> Arc<ExecutionEngine> {
        let mut config = Config::try_parse_from(["calm-server"]).unwrap();
        self.configure(&mut config);
        config.execution_recovery_interval = interval;
        let engine =
            crate::execution_boot::recover(&config, pool, &["claude-planner", "acp-planner"])
                .await
                .unwrap()
                .unwrap();
        // Listener binding and SQL recovery do not await the client's Watching.
        // Both initial assembly and a reopened kernel must finish that handshake.
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            engine.output_generation(),
        )
        .await
        .expect("execution fixture did not reach Watching")
        .expect("execution fixture Watch failed");
        engine
    }
}

pub(crate) struct Fixture {
    pub engine: Arc<ExecutionEngine>,
    supervisor: Supervisor,
}
impl Fixture {
    pub async fn new(pool: SqliteDb) -> Self {
        Self::new_with_interval(pool, std::time::Duration::from_secs(1)).await
    }
    /// A long interval leaves only explicit drives to start admitted executions.
    pub async fn new_with_interval(pool: SqliteDb, interval: std::time::Duration) -> Self {
        let supervisor = Supervisor::new();
        let engine = supervisor.recover(pool, interval).await;
        Self { engine, supervisor }
    }
    pub async fn new_kernel(&self, pool: SqliteDb) -> Arc<ExecutionEngine> {
        self.supervisor
            .recover(pool, std::time::Duration::from_secs(1))
            .await
    }
    /// Stop the supervisor; returns once the engine has observed the disconnect.
    pub async fn disconnect(self) -> Arc<ExecutionEngine> {
        let watching = self.engine.output_generation().await.unwrap();
        drop(self.supervisor);
        tokio::time::timeout(std::time::Duration::from_secs(10), watching.changed())
            .await
            .expect("engine must observe supervisor disconnect")
            .unwrap();
        assert!(!self.engine.available());
        self.engine
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

/// Cargo reports the artifact path through the one builder,
/// scripts/prepare-proc-supervisor-helper.py. Do not infer its private
/// target-directory layout, and do not substitute a script for the registered
/// helper ELF image. nextest runs every test in its own process, so its
/// `proc-supervisor-helper` setup script builds once before any test starts;
/// a test here never runs Cargo. libtest runs this whole binary in one process,
/// which builds once here.
fn helper_binary() -> PathBuf {
    const PREPARED: &str = "NEIGE_TEST_PROC_SUPERVISOR_BIN";
    static HELPER: OnceLock<PathBuf> = OnceLock::new();
    HELPER
        .get_or_init(|| {
            if std::env::var_os("NEXTEST").is_some_and(|runner| runner == "1") {
                return std::env::var_os(PREPARED)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        panic!("{PREPARED} is unset: the nextest setup script did not prepare it")
                    });
            }
            let output = std::process::Command::new("python3")
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../scripts/prepare-proc-supervisor-helper.py"
                ))
                .arg("--print")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "helper build: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let path = String::from_utf8(output.stdout).unwrap();
            PathBuf::from(path.strip_suffix('\n').expect("helper builder path line"))
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
    (pool)
        .writer()
        .execute(sqlx::query(
            "CREATE TRIGGER c4_block_stops BEFORE UPDATE OF desired ON workspace_executions
         WHEN NEW.desired='stop' BEGIN SELECT RAISE(FAIL, 'provider Stop blocked'); END",
        ))
        .await
        .unwrap();
}

pub(crate) async fn unblock_stops(pool: &SqliteDb) {
    (pool)
        .writer()
        .execute(sqlx::query("DROP TRIGGER c4_block_stops"))
        .await
        .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use calm_truth::db::{Repo, sqlite::SqlxRepo};

    #[tokio::test]
    async fn new_returns_after_watching() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let fixture = Fixture::new(repo.sqlite_db().unwrap()).await;
        assert!(fixture.engine.available(), "new returned before Watching");
        assert!(crate::provider_execution::engine(Some(&fixture.engine)).is_ok());
    }

    #[tokio::test]
    async fn new_kernel_returns_after_watching() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let pool = repo.sqlite_db().unwrap();
        let fixture = Fixture::new(pool.clone()).await;
        let engine = fixture.new_kernel(pool).await;
        assert!(engine.available(), "new_kernel returned before Watching");
        assert!(crate::provider_execution::engine(Some(&engine)).is_ok());
    }

    #[tokio::test]
    async fn watching_does_not_hide_later_disconnect() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let pool = repo.sqlite_db().unwrap();
        let fixture = Fixture::new(pool.clone()).await;
        let reopened = fixture.new_kernel(pool).await;
        let first = fixture.engine.output_generation().await.unwrap();
        let second = reopened.output_generation().await.unwrap();
        drop(fixture.supervisor);
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            first.changed().await.unwrap();
            second.changed().await.unwrap();
        })
        .await
        .expect("both clients must observe supervisor disconnect");
        for engine in [&fixture.engine, &reopened] {
            assert!(!engine.available());
            assert!(matches!(
                crate::provider_execution::engine(Some(engine)),
                Err(crate::error::CalmError::ExecutionUnavailable)
            ));
        }
    }
}
