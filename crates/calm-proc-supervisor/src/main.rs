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

/// #1013 §2.4(a) — establish this binary's `SIGCHLD` process invariant.
///
/// **State it in the negative, because the positive form is literally false.**
/// The invariant is:
///
/// > In this binary `SIGCHLD` is never set to be ignored and never carries the
/// > no-child-wait flag. **Installing a handler is permitted.**
///
/// The tempting "this process sets the disposition once at startup and never
/// changes it" is not true: on hosts where pidfd is unavailable, tokio installs
/// a `SIGCHLD` *handler* (via `signal_hook_registry`) the first time it spawns a
/// child. That is harmless for #1013 — a handler still lets children become our
/// zombies, which is what the pin needs — but an invariant that is false on its
/// face gets discarded wholesale the next time someone checks it.
///
/// **Why the explicit reset is still needed**, given we never ignore it
/// ourselves: an ignored signal disposition **survives `execve`** (`man 2
/// execve`). A parent that ignored `SIGCHLD` and then exec'd us would hand us a
/// process in which the kernel auto-reaps every child at exit, silently
/// destroying every pin (#1013 §2.4) before we spawn anything. This one
/// `sigaction` closes that inheritance path.
///
/// Grade honestly: this is a **process invariant** for this binary, not
/// something the type system or a test enforces. The in-process mode
/// (`InProcessProcSupervisor`, and any `calm-server` test binary that hosts a
/// `ProcRegistry`) cannot make this promise — other code in the host process
/// may change the disposition at any time — and is a documented degraded
/// configuration, not a covered case. The lint side is the
/// `no_wildcard_wait_in_the_supervisor_host` scan, which forbids the two
/// dangerous literals anywhere under this crate's `src`.
///
/// Failure is logged, not fatal: a supervisor that cannot reset the disposition
/// is still far more useful than one that refuses to start, and the WARN is
/// what makes the degraded state diagnosable.
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
