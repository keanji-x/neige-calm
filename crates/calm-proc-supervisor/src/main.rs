use anyhow::Context;
use calm_proc_supervisor::{ProcRegistry, bind_control_listener, serve_with_listener};
use clap::Parser;
use std::path::PathBuf;
use tokio::sync::oneshot;

#[derive(Debug, Parser)]
#[command(name = "calm-proc-supervisor", version)]
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
    establish_sigchld_process_invariant();
    let registry = ProcRegistry::new();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let listener = bind_control_listener(&args.control_sock)?;
    let serve_task = tokio::spawn(serve_with_listener(
        listener,
        args.control_sock,
        registry.clone(),
        shutdown_rx,
    ));

    wait_for_shutdown_signal().await;
    let _ = shutdown_tx.send(());
    // Best-effort group-SIGTERM every live proc; no fixed grace — the daemons either honor SIGTERM or get SIGKILLed when this process exits.
    registry.terminate_all_process_groups().await;
    serve_task.await.context("join control socket task")??;
    Ok(())
}

/// In this binary `SIGCHLD` is never ignored and never carries the no-child-wait flag (a handler is permitted).
/// An ignored disposition survives `execve`, so a parent that ignored `SIGCHLD` would hand us a process where the kernel auto-reaps every child; this reset closes that path. Failure is logged, not fatal.
fn establish_sigchld_process_invariant() {
    // SAFETY: `action` is a fully initialised `sigaction` we own; `sigaction(2)`
    // only reads it. `SIG_DFL` with no flags is the state a freshly-exec'd
    // process is supposed to be in.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = libc::SIG_DFL;
        action.sa_flags = 0;
        libc::sigemptyset(&mut action.sa_mask);
        if libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut()) != 0 {
            let err = std::io::Error::last_os_error();
            tracing::warn!(
                %err,
                "could not reset the SIGCHLD disposition at startup; if this process was \
                 exec'd by a parent that had it ignored, the kernel will auto-reap our \
                 children and the #1013 pty leader pin will not hold"
            );
        }
    }
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
