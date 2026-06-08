use std::io::{ErrorKind, Write};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

const TEST_BUDGET: Duration = Duration::from_secs(10);

async fn bind_stub() -> Option<(TcpListener, String)> {
    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            eprintln!("skipping bridge session_id test: sandbox denied loopback bind: {e}");
            return None;
        }
        Err(e) => panic!("bind 127.0.0.1:0: {e}"),
    };
    let addr = listener.local_addr().unwrap();
    Some((listener, format!("http://{addr}")))
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> String {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let n = match tokio::time::timeout(Duration::from_secs(2), stream.read(&mut chunk)).await {
            Ok(Ok(n)) => n,
            _ => 0,
        };
        if n == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..n]);
        if let Some((body_start, content_len)) = request_body_bounds(&request)
            && request.len() >= body_start + content_len
        {
            break;
        }
        if request.windows(4).any(|w| w == b"\r\n\r\n")
            && !String::from_utf8_lossy(&request).contains("content-length:")
        {
            break;
        }
    }
    String::from_utf8_lossy(&request).to_string()
}

fn request_body_bounds(request: &[u8]) -> Option<(usize, usize)> {
    let header_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")?;
    let headers = String::from_utf8_lossy(&request[..header_end]);
    let content_len = headers
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        })
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())?;
    Some((header_end + 4, content_len))
}

async fn serve_resolution_then_hook(listener: TcpListener, captured: oneshot::Sender<String>) {
    let (mut get_stream, _) = listener.accept().await.expect("accept resolve conn");
    let get_req = read_request(&mut get_stream).await;
    assert!(
        get_req.contains("GET /api/threads/thread-abc/card?provider=codex"),
        "resolution request was:\n{get_req}"
    );
    let body =
        r#"{"thread_id":"thread-abc","card_id":"card-from-thread","role":"plain","wave_id":null}"#;
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = get_stream.write_all(resp.as_bytes()).await;
    let _ = get_stream.shutdown().await;

    let (mut post_stream, _) = listener.accept().await.expect("accept hook conn");
    let post_req = read_request(&mut post_stream).await;
    let _ = captured.send(post_req);
    let resp = "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n";
    let _ = post_stream.write_all(resp.as_bytes()).await;
    let _ = post_stream.shutdown().await;
}

fn spawn_bridge(base_url: &str) -> (String, std::process::ExitStatus, String) {
    let bridge_bin = env!("CARGO_BIN_EXE_neige-codex-bridge");
    let mut child = std::process::Command::new(bridge_bin)
        .env_remove("NEIGE_CARD_ID")
        .env("NEIGE_CALM_BASE_URL", base_url)
        .env("NEIGE_HOOK_PROVIDER", "codex")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bridge binary");
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "thread-abc",
    });
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    child.stdin.take();

    let output = wait_with_timeout(child, TEST_BUDGET);
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        output.status,
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn wait_with_timeout(mut child: std::process::Child, timeout: Duration) -> std::process::Output {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if child.try_wait().expect("try_wait").is_some() {
            return child.wait_with_output().expect("wait_with_output");
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            return child
                .wait_with_output()
                .expect("wait_with_output after kill");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_resolves_card_id_from_hook_session_id() {
    let Some((listener, base)) = bind_stub().await else {
        return;
    };
    let (tx, rx) = oneshot::channel();
    let stub_handle = tokio::spawn(serve_resolution_then_hook(listener, tx));

    let (stdout, status, stderr) = tokio::task::spawn_blocking(move || spawn_bridge(&base))
        .await
        .expect("spawn_blocking join");
    assert!(
        status.success(),
        "bridge must exit 0 (got {status:?}); stderr:\n{stderr}"
    );
    assert_eq!(stdout.trim(), "{}");

    let post_req = tokio::time::timeout(Duration::from_secs(2), rx)
        .await
        .expect("captured POST timeout")
        .expect("captured POST");
    let _ = tokio::time::timeout(Duration::from_secs(2), stub_handle).await;
    assert!(
        post_req.contains("POST /internal/codex/hook?card_id=card-from-thread"),
        "hook POST used resolved card id; request was:\n{post_req}"
    );
}
