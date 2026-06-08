use std::io::{ErrorKind, Write};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const TEST_BUDGET: Duration = Duration::from_secs(10);

async fn bind_stub() -> Option<(TcpListener, String)> {
    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            eprintln!("skipping bridge retry test: sandbox denied loopback bind: {e}");
            return None;
        }
        Err(e) => panic!("bind 127.0.0.1:0: {e}"),
    };
    let addr = listener.local_addr().unwrap();
    Some((listener, format!("http://{addr}")))
}

async fn serve_statuses(listener: TcpListener, statuses: Vec<u16>, attempts: Arc<AtomicUsize>) {
    for status in statuses {
        let (mut stream, _) = listener.accept().await.expect("accept retry conn");
        attempts.fetch_add(1, Ordering::SeqCst);
        let _ = read_http_request(&mut stream).await;
        let phrase = if status == 204 {
            "No Content"
        } else {
            "Internal Server Error"
        };
        let resp = format!("HTTP/1.1 {status} {phrase}\r\nConnection: close\r\n\r\n");
        let _ = stream.write_all(resp.as_bytes()).await;
        let _ = stream.shutdown().await;
    }
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

fn spawn_bridge(
    base_url: &str,
    fallback_dir: &std::path::Path,
    payload: serde_json::Value,
) -> (String, std::process::ExitStatus, String, Duration) {
    let bridge_bin = env!("CARGO_BIN_EXE_neige-codex-bridge");
    let mut child = std::process::Command::new(bridge_bin)
        .env("NEIGE_CARD_ID", "retry-card")
        .env("NEIGE_CALM_BASE_URL", base_url)
        .env("NEIGE_HOOK_FALLBACK_DIR", fallback_dir)
        .env_remove("NEIGE_HOOK_PROVIDER")
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

    let start = Instant::now();
    let output = wait_with_timeout(child, TEST_BUDGET);
    let elapsed = start.elapsed();
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        output.status,
        String::from_utf8_lossy(&output.stderr).to_string(),
        elapsed,
    )
}

fn retry_payload(hook_event_name: &str) -> serde_json::Value {
    serde_json::json!({
        "hook_event_name": hook_event_name,
        "session_id": "retry-session",
        "transcript_path": "/tmp/retry.jsonl",
        "transcript_size_bytes": 123,
    })
}

fn wait_with_timeout(mut child: std::process::Child, timeout: Duration) -> std::process::Output {
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait().expect("try_wait").is_some() {
            return child.wait_with_output().expect("wait_with_output");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return child
                .wait_with_output()
                .expect("wait_with_output after kill");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_hook_retries_until_success() {
    let Some((listener, base)) = bind_stub().await else {
        return;
    };
    let attempts = Arc::new(AtomicUsize::new(0));
    let stub = tokio::spawn(serve_statuses(
        listener,
        vec![500, 500, 204],
        attempts.clone(),
    ));
    let fallback = tempfile::tempdir().expect("tempdir");
    let base_clone = base.clone();
    let fallback_path = fallback.path().to_path_buf();

    let (stdout, status, stderr, elapsed) = tokio::task::spawn_blocking(move || {
        spawn_bridge(&base_clone, &fallback_path, retry_payload("Stop"))
    })
    .await
    .expect("spawn_blocking join");
    let _ = tokio::time::timeout(Duration::from_secs(2), stub).await;

    assert!(
        status.success(),
        "bridge exit {status:?}; stderr:\n{stderr}"
    );
    assert_eq!(stdout.trim(), "{}");
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert!(elapsed < Duration::from_secs(2), "elapsed = {elapsed:?}");
    assert!(!fallback.path().join("codex").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_hook_writes_fallback_after_all_retries_fail() {
    let Some((listener, base)) = bind_stub().await else {
        return;
    };
    let attempts = Arc::new(AtomicUsize::new(0));
    let stub = tokio::spawn(serve_statuses(
        listener,
        vec![500, 500, 500, 500, 500, 500],
        attempts.clone(),
    ));
    let fallback = tempfile::tempdir().expect("tempdir");
    let base_clone = base.clone();
    let fallback_path = fallback.path().to_path_buf();
    let first_payload = retry_payload("PreToolUse");
    let second_payload = retry_payload("PostToolUse");
    let first_body = first_payload.to_string();
    let second_body = second_payload.to_string();

    let (stdout, status, stderr, elapsed) = tokio::task::spawn_blocking(move || {
        spawn_bridge(&base_clone, &fallback_path, first_payload)
    })
    .await
    .expect("spawn_blocking join");
    let base_clone = base.clone();
    let fallback_path = fallback.path().to_path_buf();
    let (second_stdout, second_status, second_stderr, second_elapsed) =
        tokio::task::spawn_blocking(move || {
            spawn_bridge(&base_clone, &fallback_path, second_payload)
        })
        .await
        .expect("spawn_blocking join");
    let _ = tokio::time::timeout(Duration::from_secs(2), stub).await;

    assert!(
        status.success(),
        "bridge exit {status:?}; stderr:\n{stderr}"
    );
    assert!(
        second_status.success(),
        "bridge exit {second_status:?}; stderr:\n{second_stderr}"
    );
    assert_eq!(stdout.trim(), "{}");
    assert_eq!(second_stdout.trim(), "{}");
    assert_eq!(attempts.load(Ordering::SeqCst), 6);
    assert!(elapsed < Duration::from_secs(2), "elapsed = {elapsed:?}");
    assert!(
        second_elapsed < Duration::from_secs(2),
        "elapsed = {second_elapsed:?}"
    );

    let codex_dir = fallback.path().join("codex");
    let files = std::fs::read_dir(&codex_dir)
        .unwrap_or_else(|e| panic!("read fallback dir {}: {e}", codex_dir.display()))
        .collect::<Result<Vec<_>, _>>()
        .expect("fallback entries");
    assert_eq!(files.len(), 2, "files = {files:?}");

    let file_names = files
        .iter()
        .map(|file| file.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let first_hash = sha256_hex(&first_body);
    let second_hash = sha256_hex(&second_body);
    assert!(
        file_names
            .iter()
            .any(|name| name.ends_with(&format!("-{}.json", &first_hash[..16]))),
        "files = {file_names:?}"
    );
    assert!(
        file_names
            .iter()
            .any(|name| name.ends_with(&format!("-{}.json", &second_hash[..16]))),
        "files = {file_names:?}"
    );

    let records = files
        .iter()
        .map(|file| {
            serde_json::from_slice::<serde_json::Value>(
                &std::fs::read(file.path()).expect("read fallback"),
            )
            .expect("fallback json")
        })
        .collect::<Vec<_>>();
    let event_names = records
        .iter()
        .filter_map(|record| record["body"]["hook_event_name"].as_str())
        .collect::<Vec<_>>();
    assert!(event_names.contains(&"PreToolUse"), "records = {records:?}");
    assert!(
        event_names.contains(&"PostToolUse"),
        "records = {records:?}"
    );
    assert!(
        records
            .iter()
            .all(|record| record["card_id"] == "retry-card"
                && record["body"]["session_id"] == "retry-session"),
        "records = {records:?}"
    );
}

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}
