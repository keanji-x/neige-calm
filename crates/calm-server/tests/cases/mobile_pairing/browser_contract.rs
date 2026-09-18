use super::super::*;
use std::net::SocketAddr;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

async fn serve(
    router: axum::Router,
) -> (
    String,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async {
            let _ = stopped.await;
        })
        .await
        .unwrap();
    });
    (format!("http://{address}"), stop, task)
}

/// Separate from ordinary Rust CI: this executes the real bundled frontend and
/// Chromium against real routers/cookies, with no Codex process or cloud account.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires locked fe dependencies and local Chromium; run explicitly for scan integration"]
async fn scan_bundled_frontend_real_cookie_contract() {
    let (fixture, _, control) = private_tailnet_fixture().await;
    assert_eq!(
        request(
            &fixture.local,
            "POST",
            "/api/mobile/access",
            Some(&fixture.owner_cookie),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (management, stop_management, management_task) = serve(fixture.local.clone()).await;
    let (public, stop_public, public_task) = serve(fixture.public.as_ref().clone()).await;
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let artifacts = tempfile::Builder::new()
        .prefix("1712-scan-host-contract-")
        .tempdir()
        .unwrap()
        .keep();
    let mut command = tokio::process::Command::new("node");
    command
        .arg(root.join("fe/e2e/helpers/scan-host-contract.mjs"))
        .current_dir(root.join("fe"))
        .env_clear()
        .env("RAYON_NUM_THREADS", "2")
        .env("NPM_CONFIG_AUDIT", "false")
        .env("CI", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for key in [
        "PATH",
        "HOME",
        "XDG_CACHE_HOME",
        "PLAYWRIGHT_BROWSERS_PATH",
        "LD_LIBRARY_PATH",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    let mut child = command.spawn().unwrap();
    let input = serde_json::to_vec(&json!({
        "origin": "https://fixture.example.ts.net", "management": management, "public": public,
        "ownerCookie": fixture.owner_cookie, "cookieName": SESSION_COOKIE, "artifacts": artifacts,
    }))
    .unwrap();
    child.stdin.take().unwrap().write_all(&input).await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(180), child.wait_with_output()).await;
    let _ = stop_management.send(());
    let _ = stop_public.send(());
    management_task.await.unwrap();
    public_task.await.unwrap();
    fixture.auth.mobile.shutdown().await.unwrap();
    control.abort();
    let output = result
        .expect("bounded frontend contract run timed out")
        .unwrap();
    assert!(
        output.status.success(),
        "frontend contract failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["passed"],
        json!(["real-cookie-business", "stale-cookie-rejected"])
    );
    assert_eq!(report["webCompatVersion"], 30);
    assert_eq!(report["apiVersion"], "9");
    println!(
        "scan frontend/host contract artifacts: {}",
        artifacts.display()
    );
}
