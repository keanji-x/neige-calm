//! Production assembly, independent of future execution consumers.
use crate::{config::Config, execution_store::SqliteExecutionStore};
use std::sync::Arc;
use workspace_runtime::execution::{Engine, session_executor::SessionExecutor};

pub type ExecutionEngine = Engine<SqliteExecutionStore, SessionExecutor>;

/// Provider completion ports are registered beside their consumer projections.
/// Host construction owns provider-specific resources; the sweep only consumes ports.
pub(crate) fn provider_owners(
    claude: &crate::claude_planner::config::ClaudePlannerHost,
    acp: &crate::acp_planner::config::AcpPlannerHost,
) -> Vec<crate::provider_execution::ProviderExecution> {
    vec![
        claude.provider_execution.clone(),
        acp.provider_execution.clone(),
    ]
}

pub async fn recover(
    cfg: &Config,
    pool: crate::db::sqlite::SqliteDb,
) -> anyhow::Result<Option<Arc<ExecutionEngine>>> {
    let Some(key_file) = &cfg.kernel_client_key_file else {
        return Ok(None);
    };
    let secret = serde_json::from_reader(std::fs::File::open(key_file)?)?;
    let executor = SessionExecutor::new(cfg.proc_supervisor_sock_resolved(), secret);
    let store = SqliteExecutionStore::new(pool)
        .with_projection(
            crate::acp_planner::execution::CONSUMER.into(),
            Arc::new(crate::provider_execution::Projection),
        )
        .with_projection(
            crate::claude_planner::execution::CONSUMER.into(),
            Arc::new(crate::provider_execution::Projection),
        )
        .with_projection(
            crate::operation::gate_execution::CONSUMER.into(),
            Arc::new(crate::operation::gate_execution::Projection),
        )
        .with_projection(
            crate::operation::forge_execution::CONSUMER.into(),
            Arc::new(crate::operation::forge_execution::Projection),
        )
        .with_projection(
            crate::operation::terminal_execution_create::CONSUMER.into(),
            Arc::new(crate::operation::terminal_execution_create::TerminalProjection),
        );
    let store = store.with_projection(
        crate::workspace_service::reclaim::operation::CONSUMER.into(),
        Arc::new(crate::workspace_service::reclaim::operation::Projection),
    );
    #[cfg(feature = "fixtures")]
    let store = store.with_faults(cfg.execution_recovery_faults.clone());
    let engine = Arc::new(Engine::new(store, executor));
    let report = engine.recover().await;
    if let Some(error) = report.error {
        anyhow::bail!("execution recovery: {}", error.0);
    }
    tracing::info!(scheduled = report.scheduled, "execution recovery scheduled");
    // One sequential pass per interval. Skip missed ticks instead of replaying a backlog.
    // pending uses the partial index; each live row also incurs Engine reload/inspect work.
    let weak = Arc::downgrade(&engine);
    let interval = cfg.execution_recovery_interval;
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            let Some(engine) = weak.upgrade() else { break };
            if let Some(error) = engine.recover().await.error {
                tracing::warn!(error = %error.0, "periodic execution recovery failed; retry next tick");
            }
        }
    });
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
        let mut cfg = Config::try_parse_from([
            "calm-server",
            "--kernel-client-key-file",
            key.to_str().unwrap(),
            "--proc-supervisor-sock",
            socket.to_str().unwrap(),
        ])
        .unwrap();
        cfg.execution_recovery_interval = std::time::Duration::from_millis(20);
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let pool = repo.sqlite_db().unwrap();
        let store = SqliteExecutionStore::new(pool.clone()).with_projection(
            test_support::TEST_CONSUMER.into(),
            Arc::new(crate::execution_store::tests::NoopProjection),
        );
        let intent = test_support::intent(temp.path().to_owned());
        let mut tx = pool.writer_pool().begin().await.unwrap();
        store.insert(&mut tx, &intent).await.unwrap();
        tx.commit().await.unwrap();
        let engine = recover(&cfg, pool).await.unwrap().unwrap();
        assert!(!engine.available());
        assert_eq!(engine.recover().await.scheduled, 1);
        assert_eq!(store.pending().await.unwrap(), vec![intent.metadata.id]);
        #[cfg(feature = "fixtures")]
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while cfg
                .execution_recovery_faults
                .pending_calls
                .load(std::sync::atomic::Ordering::SeqCst)
                < 4
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        stop.send(()).unwrap();
        server.await.unwrap().unwrap();
    }
}
