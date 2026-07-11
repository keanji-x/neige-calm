//! `e2e-egress-proxy` — the host-side CONNECT gate binary (see lib docs).
//!
//! Usage:
//!   e2e-egress-proxy <listen-unix-socket> [upstream host:port]
//!
//! Or via env (argv wins): `E2E_EGRESS_PROXY_SOCK`, `E2E_EGRESS_PROXY_UPSTREAM`.
//! Upstream defaults to sing-box on host loopback ([`DEFAULT_UPSTREAM`]).

use std::os::unix::fs::PermissionsExt;
use std::process::ExitCode;

use e2e_egress_proxy::DEFAULT_UPSTREAM;
use e2e_egress_proxy::serve_client;
use tokio::net::UnixListener;

#[tokio::main]
async fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let listen = args
        .next()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("E2E_EGRESS_PROXY_SOCK").ok())
        .filter(|s| !s.is_empty());
    let upstream = args
        .next()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("E2E_EGRESS_PROXY_UPSTREAM").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_UPSTREAM.to_string());

    let Some(listen) = listen else {
        eprintln!("usage: e2e-egress-proxy <listen-unix-socket> [upstream host:port]");
        eprintln!("       (or set E2E_EGRESS_PROXY_SOCK / E2E_EGRESS_PROXY_UPSTREAM)");
        return ExitCode::from(2);
    };

    match run(&listen, &upstream).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[e2e-egress-proxy] fatal: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(listen: &str, upstream: &str) -> std::io::Result<()> {
    // Fresh bind: unlink a stale socket first (mirrors socat unlink-early).
    match std::fs::remove_file(listen) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(listen)?;
    // 0600: only our uid may connect, defense-in-depth atop the 700 sock dir.
    std::fs::set_permissions(listen, std::fs::Permissions::from_mode(0o600))?;
    eprintln!(
        "[e2e-egress-proxy] listening on {listen} -> upstream {upstream}; \
         admits CONNECT :443 to dot-anchored chatgpt/openai only"
    );

    let upstream = upstream.to_string();
    loop {
        let (client, _addr) = listener.accept().await?;
        let upstream = upstream.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_client(client, &upstream).await {
                eprintln!("[e2e-egress-proxy] connection error: {e}");
            }
        });
    }
}
