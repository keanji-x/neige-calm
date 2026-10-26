//! A real typed supervisor for forge's library-level production-path tests.
use crate::execution_quiesce::ExecutionLifecycle;
use std::path::PathBuf;

pub(crate) struct Executor {
    pub lifecycle: ExecutionLifecycle,
    pub(in crate::operation) _root: tempfile::TempDir,
    pub(in crate::operation) _socket: tempfile::TempDir,
    child: std::process::Child,
}
impl Drop for Executor {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Executor {
    pub async fn new(pool: crate::db::sqlite::SqliteDb) -> Self {
        Self::with_environment(pool, &[]).await
    }
    pub async fn with_environment(
        pool: crate::db::sqlite::SqliteDb,
        environment: &[(std::ffi::OsString, std::ffi::OsString)],
    ) -> Self {
        use clap::Parser;
        let root = tempfile::tempdir().unwrap();
        let socket = calm_test_sockets::socket_dir("forge-lib");
        let path = socket.path().join("control.sock");
        let key = root.path().join("key.json");
        let secret = workspace_runtime::execution::CapabilityToken::fresh().unwrap();
        std::fs::write(&key, serde_json::to_vec(&secret).unwrap()).unwrap();
        // Cargo places this repository's declared fixture binary beside deps/.
        let helper: PathBuf = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("osc-probe-child");
        assert!(
            helper.is_file(),
            "build calm-server's osc-probe-child fixture before library tests"
        );
        let child = std::process::Command::new(helper)
            .envs(environment.iter().cloned())
            .arg("--execution-supervisor")
            .arg(root.path().join("journal"))
            .arg(&path)
            .arg(&key)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let cfg = crate::config::Config::try_parse_from([
            "calm-server",
            "--kernel-client-key-file",
            key.to_str().unwrap(),
            "--proc-supervisor-sock",
            path.to_str().unwrap(),
        ])
        .unwrap();
        let engine = crate::execution_boot::recover(&cfg, pool, &["claude-planner", "acp-planner"])
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
        let lifecycle = ExecutionLifecycle::default();
        lifecycle.bind(engine);
        Self {
            lifecycle,
            _root: root,
            _socket: socket,
            child,
        }
    }
}
