//! Production assembly, independent of future execution consumers.
use crate::{config::Config, execution_store::SqliteExecutionStore};
use std::sync::Arc;
use workspace_runtime::execution::{Engine, session_executor::SessionExecutor};

pub type ExecutionEngine = Engine<SqliteExecutionStore, SessionExecutor>;

pub async fn recover(
    cfg: &Config,
    pool: sqlx::SqlitePool,
) -> anyhow::Result<Option<Arc<ExecutionEngine>>> {
    let Some(key_file) = &cfg.kernel_client_key_file else {
        return Ok(None);
    };
    let secret = serde_json::from_reader(std::fs::File::open(key_file)?)?;
    let executor = SessionExecutor::new(cfg.proc_supervisor_sock_resolved(), secret);
    let engine = Arc::new(Engine::new(SqliteExecutionStore::new(pool), executor));
    let report = engine.recover().await;
    if let Some(error) = report.error {
        anyhow::bail!("execution recovery: {}", error.0);
    }
    tracing::info!(scheduled = report.scheduled, "execution recovery scheduled");
    Ok(Some(engine))
}

#[cfg(test)]
mod port_contract {
    use super::*;
    use calm_truth::db::{Repo, sqlite::SqlxRepo};
    use clap::Parser;
    use workspace_runtime::execution::{CapabilityToken, Store, test_support};

    #[tokio::test]
    async fn boot_recover_with_pre_upgrade_supervisor_is_unavailable() {
        let temp = tempfile::tempdir().unwrap();
        let key = temp.path().join("key.json");
        std::fs::write(
            &key,
            serde_json::to_vec(&CapabilityToken::fresh().unwrap()).unwrap(),
        )
        .unwrap();
        let socket_dir = calm_test_sockets::socket_dir("execution-boot");
        let socket = socket_dir.path().join("supervisor.sock");
        let listener = calm_proc_supervisor::bind_control_listener(&socket).unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(calm_proc_supervisor::serve_with_listener(
            listener,
            socket.clone(),
            calm_proc_supervisor::ProcRegistry::new(),
            stopped,
        ));
        let cfg = Config::try_parse_from([
            "calm-server",
            "--kernel-client-key-file",
            key.to_str().unwrap(),
            "--proc-supervisor-sock",
            socket.to_str().unwrap(),
        ])
        .unwrap();
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let pool = repo.sqlite_pool().unwrap();
        let store = SqliteExecutionStore::new(pool.clone());
        let intent = test_support::intent(temp.path().to_owned());
        let mut tx = pool.begin().await.unwrap();
        store.insert(&mut tx, &intent).await.unwrap();
        tx.commit().await.unwrap();
        let engine = recover(&cfg, pool).await.unwrap().unwrap();
        assert!(!engine.available());
        assert_eq!(engine.recover().await.scheduled, 1);
        assert_eq!(store.pending().await.unwrap(), vec![intent.metadata.id]);
        stop.send(()).unwrap();
        server.await.unwrap().unwrap();
    }
}
