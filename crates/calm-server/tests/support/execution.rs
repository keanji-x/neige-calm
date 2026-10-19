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
