use anyhow::Context;
use calm_proc_supervisor::{ProcRegistry, serve_control_socket};
use clap::Parser;
use std::path::PathBuf;
use tokio::sync::oneshot;

#[derive(Debug, Parser)]
#[command(name = "calm-proc-supervisor")]
struct Args {
    /// Control Unix socket path. In production callers should use
    /// $CALM_DATA_DIR/proc-supervisor.sock.
    #[arg(long)]
    control_sock: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("info,calm_proc_supervisor=debug")
            }),
        )
        .init();

    let args = Args::parse();
    let registry = ProcRegistry::new();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let serve_task = tokio::spawn(serve_control_socket(
        args.control_sock,
        registry.clone(),
        shutdown_rx,
    ));

    wait_for_shutdown_signal().await;
    let _ = shutdown_tx.send(());
    // Best-effort group-SIGTERM every live proc — enforces the #388
    // "supervisor death drops procs" Non-goal explicitly. Per-proc reap
    // happens via the registry's spawned wait tasks, which the runtime
    // join below drains; we don't hold the shutdown for a fixed grace
    // here, the daemons either honor SIGTERM or get SIGKILLed when this
    // process exits (kernel reaps via init's reparenting).
    registry.terminate_all_process_groups().await;
    serve_task.await.context("join control socket task")??;
    Ok(())
}

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT handler");
        tokio::select! {
            _ = sigterm.recv() => {},
            _ = sigint.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    tracing::info!("shutdown requested");
}
