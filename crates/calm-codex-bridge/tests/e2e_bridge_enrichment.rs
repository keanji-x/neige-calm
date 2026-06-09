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
            eprintln!("skipping bridge E2E: sandbox denied loopback bind: {e}");
            return None;
        }
        Err(e) => panic!("bind 127.0.0.1:0: {e}"),
    };
    let addr = listener.local_addr().unwrap();
    Some((listener, format!("http://{addr}")))
}

async fn serve_one_hook(listener: TcpListener, captured: oneshot::Sender<String>) {
    let (mut stream, _) = listener.accept().await.expect("accept stub conn");
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
    }

    let body = request_body(&request).unwrap_or_default();
    let _ = captured.send(body);

    let resp = "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n";
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;
}

async fn serve_resolution_then_hook(listener: TcpListener, captured: oneshot::Sender<String>) {
    let (mut resolve_stream, _) = listener.accept().await.expect("accept resolve conn");
    let _resolve_request = read_http_request(&mut resolve_stream).await;
    let body =
        r#"{"thread_id":"test-session","card_id":"resolved-card","role":"worker","wave_id":null}"#;
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = resolve_stream.write_all(resp.as_bytes()).await;
    let _ = resolve_stream.shutdown().await;

    let (mut hook_stream, _) = listener.accept().await.expect("accept hook conn");
    let request = read_http_request(&mut hook_stream).await;
    let body = request_body(request.as_bytes()).unwrap_or_default();
    let _ = captured.send(body);
    let resp = "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n";
    let _ = hook_stream.write_all(resp.as_bytes()).await;
    let _ = hook_stream.shutdown().await;
}

async fn read_http_request(stream: &mut tokio::net::TcpStream) -> String {
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
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request);
            if !headers
                .lines()
                .any(|line| line.to_ascii_lowercase().starts_with("content-length:"))
            {
                break;
            }
        }
    }

    String::from_utf8_lossy(&request).to_string()
}

fn request_body(request: &[u8]) -> Option<String> {
    let (body_start, content_len) = request_body_bounds(request)?;
    if request.len() < body_start + content_len {
        return None;
    }
    Some(String::from_utf8_lossy(&request[body_start..body_start + content_len]).to_string())
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

fn spawn_bridge(
    base_url: &str,
    provider: &str,
    payload: serde_json::Value,
) -> (String, std::process::ExitStatus, String) {
    let bridge_bin = env!("CARGO_BIN_EXE_neige-codex-bridge");
    let mut child = std::process::Command::new(bridge_bin)
        .env("NEIGE_CARD_ID", "test-card")
        .env("NEIGE_CALM_BASE_URL", base_url)
        .env("NEIGE_HOOK_PROVIDER", provider)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bridge binary");

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

fn spawn_bridge_without_card_env(
    base_url: &str,
    provider: &str,
    payload: serde_json::Value,
) -> (String, std::process::ExitStatus, String) {
    let bridge_bin = env!("CARGO_BIN_EXE_neige-codex-bridge");
    let mut child = std::process::Command::new(bridge_bin)
        .env_remove("NEIGE_CARD_ID")
        .env("NEIGE_CALM_BASE_URL", base_url)
        .env("NEIGE_HOOK_PROVIDER", provider)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bridge binary");

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

fn write_transcript(text: &str) -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new().expect("tempfile");
    let record = serde_json::json!({
        "type": "assistant",
        "message": {
            "content": [
                { "type": "text", "text": text }
            ]
        }
    });
    writeln!(file, "{record}").expect("write transcript");
    file
}

fn write_codex_rollout(text: &str) -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new().expect("tempfile");
    let record = serde_json::json!({
        "type": "response_item",
        "payload": {
            "role": "assistant",
            "content": [
                { "type": "output_text", "text": text }
            ]
        }
    });
    writeln!(file, "{record}").expect("write codex rollout transcript");
    file
}

async fn run_bridge_and_capture_body(
    provider: &str,
    payload: serde_json::Value,
) -> Option<(serde_json::Value, String, String)> {
    let (listener, base) = bind_stub().await?;
    let (tx, rx) = oneshot::channel();
    let stub_handle = tokio::spawn(serve_one_hook(listener, tx));

    let base_clone = base.clone();
    let provider = provider.to_string();
    let (stdout, status, stderr) =
        tokio::task::spawn_blocking(move || spawn_bridge(&base_clone, &provider, payload))
            .await
            .expect("spawn_blocking join");

    assert!(
        status.success(),
        "bridge must exit 0 (got {status:?}); stderr:\n{stderr}"
    );
    let body = tokio::time::timeout(Duration::from_secs(2), rx)
        .await
        .expect("captured body timeout")
        .expect("captured body");
    let _ = tokio::time::timeout(Duration::from_secs(2), stub_handle).await;

    let parsed =
        serde_json::from_str(&body).unwrap_or_else(|_| panic!("POST body is JSON: {body}"));
    Some((parsed, stdout, stderr))
}

async fn run_bridge_with_session_resolution_and_capture_body(
    provider: &str,
    payload: serde_json::Value,
) -> Option<(serde_json::Value, String, String)> {
    let (listener, base) = bind_stub().await?;
    let (tx, rx) = oneshot::channel();
    let stub_handle = tokio::spawn(serve_resolution_then_hook(listener, tx));

    let base_clone = base.clone();
    let provider = provider.to_string();
    let (stdout, status, stderr) = tokio::task::spawn_blocking(move || {
        spawn_bridge_without_card_env(&base_clone, &provider, payload)
    })
    .await
    .expect("spawn_blocking join");

    assert!(
        status.success(),
        "bridge must exit 0 (got {status:?}); stderr:\n{stderr}"
    );
    let body = tokio::time::timeout(Duration::from_secs(2), rx)
        .await
        .expect("captured body timeout")
        .expect("captured body");
    let _ = tokio::time::timeout(Duration::from_secs(2), stub_handle).await;

    let parsed =
        serde_json::from_str(&body).unwrap_or_else(|_| panic!("POST body is JSON: {body}"));
    Some((parsed, stdout, stderr))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_stop_payload_is_enriched_from_transcript() {
    let transcript = write_transcript("claude final answer");
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "test-session",
        "transcript_path": transcript.path()
    });

    let Some((posted, stdout, _stderr)) = run_bridge_and_capture_body("claude", payload).await
    else {
        return;
    };

    assert_eq!(stdout.trim(), r#"{"continue":true}"#);
    assert_eq!(
        posted
            .get("last_assistant_message")
            .and_then(serde_json::Value::as_str),
        Some("claude final answer")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_provider_with_claude_shape_transcript_is_enriched() {
    let transcript = write_transcript("codex final answer");
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "test-session",
        "transcript_path": transcript.path()
    });

    let Some((posted, stdout, _stderr)) = run_bridge_and_capture_body("codex", payload).await
    else {
        return;
    };

    assert_eq!(stdout.trim(), "{}");
    assert_eq!(
        posted
            .get("last_assistant_message")
            .and_then(serde_json::Value::as_str),
        Some("codex final answer")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_id_resolution_path_preserves_stop_enrichment() {
    let transcript = write_transcript("resolved final answer");
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "test-session",
        "transcript_path": transcript.path()
    });

    let Some((posted, stdout, _stderr)) =
        run_bridge_with_session_resolution_and_capture_body("codex", payload).await
    else {
        return;
    };

    assert_eq!(stdout.trim(), "{}");
    assert_eq!(
        posted
            .get("last_assistant_message")
            .and_then(serde_json::Value::as_str),
        Some("resolved final answer")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_stop_payload_is_enriched_from_codex_shape_rollout() {
    let transcript = write_codex_rollout("codex rollout final answer");
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "test-session",
        "transcript_path": transcript.path()
    });

    let Some((posted, stdout, _stderr)) = run_bridge_and_capture_body("codex", payload).await
    else {
        return;
    };

    assert_eq!(stdout.trim(), "{}");
    assert_eq!(
        posted
            .get("last_assistant_message")
            .and_then(serde_json::Value::as_str),
        Some("codex rollout final answer")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn existing_last_assistant_message_is_preserved() {
    let transcript = write_transcript("new transcript value");
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "test-session",
        "transcript_path": transcript.path(),
        "last_assistant_message": "native value"
    });

    let Some((posted, _stdout, _stderr)) = run_bridge_and_capture_body("codex", payload).await
    else {
        return;
    };

    assert_eq!(
        posted
            .get("last_assistant_message")
            .and_then(serde_json::Value::as_str),
        Some("native value")
    );
}
