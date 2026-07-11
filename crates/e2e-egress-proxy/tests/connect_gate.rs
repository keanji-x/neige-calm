//! Integration tests for the security boundary: drive `serve_client` over a
//! real unix socket, with a tiny in-process TCP listener standing in for
//! sing-box. Fully hermetic — no real network, no sing-box, no DNS.
//!
//! The assertions ARE the fence contract (design §8.2):
//!   - CONNECT to prod (`127.0.0.1:4040/4041`), RFC1918/link-local literals on
//!     :443, the metadata IP, dot-anchor tricks, and a non-443 port on an
//!     allowlisted host -> `403`, WITHOUT ever reaching the stub upstream.
//!   - CONNECT to an allowlisted host on :443 -> `200`, reaching the stub.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tempfile::TempDir;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::net::UnixListener;
use tokio::net::UnixStream;

use e2e_egress_proxy::serve_client;

/// A stub upstream that answers every CONNECT with `200 Connection
/// established`, then holds the tunnel. Counts how many CONNECTs it received so
/// a test can assert a denied target NEVER reached upstream.
struct StubUpstream {
    addr: String,
    connects: Arc<AtomicUsize>,
}

async fn spawn_stub_upstream() -> StubUpstream {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind stub");
    let addr = listener.local_addr().expect("stub addr").to_string();
    let connects = Arc::new(AtomicUsize::new(0));
    let counter = connects.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let counter = counter.clone();
            tokio::spawn(async move {
                // Read the CONNECT head, count it, then acknowledge.
                let mut buf = Vec::new();
                let mut tmp = [0u8; 512];
                loop {
                    match sock.read(&mut tmp).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                counter.fetch_add(1, Ordering::SeqCst);
                let _ = sock
                    .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                    .await;
                // Hold briefly so the client-side splice has something to talk to.
                let mut sink = [0u8; 256];
                let _ = sock.read(&mut sink).await;
            });
        }
    });
    StubUpstream { addr, connects }
}

/// Bind our proxy on a short-path (/tmp) unix socket and serve it. Returns the
/// TempDir (keep it alive) + the socket path.
async fn spawn_proxy(upstream: String) -> (TempDir, PathBuf) {
    // Force the socket under /tmp so its path stays well under SUN_LEN (~108),
    // independent of the (long) worktree path or $TMPDIR.
    let dir = tempfile::Builder::new()
        .prefix("e2e-egress-proxy-")
        .tempdir_in("/tmp")
        .expect("tempdir");
    let sock = dir.path().join("proxy.sock");
    let listener = UnixListener::bind(&sock).expect("bind proxy sock");
    tokio::spawn(async move {
        loop {
            let Ok((client, _)) = listener.accept().await else {
                break;
            };
            let upstream = upstream.clone();
            tokio::spawn(async move {
                let _ = serve_client(client, &upstream).await;
            });
        }
    });
    (dir, sock)
}

/// Send `CONNECT <authority>` through the proxy socket and return the HTTP
/// status code the client observes (`None` on no answer / timeout).
async fn connect_status(sock: &Path, authority: &str) -> Option<u16> {
    let mut client = UnixStream::connect(sock).await.expect("connect proxy");
    let req = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n");
    client.write_all(req.as_bytes()).await.expect("write CONNECT");

    let mut buf = Vec::new();
    let mut tmp = [0u8; 256];
    loop {
        match tokio::time::timeout(Duration::from_secs(5), client.read(&mut tmp)).await {
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
            Ok(Ok(n)) => buf.extend_from_slice(&tmp[..n]),
        }
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let line_end = buf.windows(2).position(|w| w == b"\r\n").unwrap_or(buf.len());
    let line = std::str::from_utf8(&buf[..line_end]).ok()?;
    line.split_whitespace().nth(1)?.parse::<u16>().ok()
}

#[tokio::test]
async fn denies_prod_ports() {
    let up = spawn_stub_upstream().await;
    let (_dir, sock) = spawn_proxy(up.addr.clone()).await;
    for t in ["127.0.0.1:4040", "127.0.0.1:4041"] {
        assert_eq!(connect_status(&sock, t).await, Some(403), "want 403 for {t}");
    }
    assert_eq!(
        up.connects.load(Ordering::SeqCst),
        0,
        "denied prod CONNECTs must never reach upstream"
    );
}

#[tokio::test]
async fn denies_rfc1918_linklocal_and_metadata_on_443() {
    let up = spawn_stub_upstream().await;
    let (_dir, sock) = spawn_proxy(up.addr.clone()).await;
    for t in [
        "10.0.0.1:443",
        "169.254.169.254:443",
        "192.168.1.1:443",
    ] {
        assert_eq!(connect_status(&sock, t).await, Some(403), "want 403 for {t}");
    }
    assert_eq!(up.connects.load(Ordering::SeqCst), 0, "must not reach upstream");
}

#[tokio::test]
async fn denies_dot_anchor_tricks() {
    let up = spawn_stub_upstream().await;
    let (_dir, sock) = spawn_proxy(up.addr.clone()).await;
    for t in ["evilchatgpt.com:443", "chatgpt.com.evil.example:443"] {
        assert_eq!(connect_status(&sock, t).await, Some(403), "want 403 for {t}");
    }
    assert_eq!(up.connects.load(Ordering::SeqCst), 0, "must not reach upstream");
}

#[tokio::test]
async fn denies_host_charset_injection_authorities() {
    // #923 F5: authorities whose RAW suffix string-matches the allowlist (or a
    // homograph) but whose host carries injection chars (`:` `#` `@` `/` `?`) or
    // non-ASCII. "Deny by construction" must reject these at OUR charset gate,
    // never forwarding them verbatim upstream on the bet that sing-box/Go's
    // parser happens to choke on them.
    let up = spawn_stub_upstream().await;
    let (_dir, sock) = spawn_proxy(up.addr.clone()).await;
    for t in [
        "127.0.0.1:4040#.chatgpt.com:443",
        "127.0.0.1:4040.chatgpt.com:443",
        "user@127.0.0.1:4040#.chatgpt.com:443",
        "/a/b?x=.chatgpt.com:443",
        "\u{0441}hatgpt.com:443", // Cyrillic 'с' homograph of chatgpt.com
    ] {
        assert_eq!(connect_status(&sock, t).await, Some(403), "want 403 for {t:?}");
    }
    assert_eq!(
        up.connects.load(Ordering::SeqCst),
        0,
        "charset-injection CONNECTs must never reach upstream"
    );
}

#[tokio::test]
async fn denies_allowlisted_host_on_wrong_port() {
    let up = spawn_stub_upstream().await;
    let (_dir, sock) = spawn_proxy(up.addr.clone()).await;
    assert_eq!(
        connect_status(&sock, "chatgpt.com:80").await,
        Some(403),
        "allowlisted host on :80 must be denied on the port check"
    );
    assert_eq!(up.connects.load(Ordering::SeqCst), 0, "must not reach upstream");
}

#[tokio::test]
async fn allows_allowlisted_hosts_reaching_stub_upstream() {
    let up = spawn_stub_upstream().await;
    let (_dir, sock) = spawn_proxy(up.addr.clone()).await;
    let allowed = [
        "chatgpt.com:443",
        "sub.chatgpt.com:443",
        "auth.openai.com:443",
        "api.openai.com:443",
    ];
    for t in allowed {
        assert_eq!(connect_status(&sock, t).await, Some(200), "want 200 for {t}");
    }
    assert_eq!(
        up.connects.load(Ordering::SeqCst),
        allowed.len(),
        "each allowlisted CONNECT should reach the stub upstream exactly once"
    );
}
