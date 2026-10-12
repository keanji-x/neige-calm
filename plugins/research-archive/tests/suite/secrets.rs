//! I11: no token, refresh token or account string reaches the binary's
//! output, our logs, the archive file or a `Debug` rendering. The capture
//! log's part of I11 arrives with the runner (S1-2b).

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::Path,
    process::Stdio,
    sync::mpsc,
    time::Duration,
};

use longbridge::oauth::StoredToken;
use research_archive::auth::{self, ClientId, DataDirTokenStorage};
use research_archive::catalog::Catalog;
use research_archive::config::Config;
use research_archive::types::Status;
use research_archive::{Store, logging};
use tracing::Level;

use crate::cli::{CLIENT, Env, text};
use crate::fake::{FakeServer, Reply};
use crate::logs::Capture;
use crate::source::source;
use crate::support::{at, key, plan_request};

const ACCESS: &str = "CANARY-I11-ACCESS-2c41";
const REFRESH: &str = "CANARY-I11-REFRESH-9e07";
/// What an account number looks like where one could appear: inside the
/// access token (a JWT carries the member id) and in upstream error text.
const ACCOUNT: &str = "CANARY-I11-ACCOUNT-80451234";
const CANARIES: [&str; 3] = [ACCESS, REFRESH, ACCOUNT];

fn access_token() -> String {
    format!("ap_m_{ACCESS}.{ACCOUNT}")
}

fn assert_clean(what: &str, bytes: &[u8]) {
    let haystack = String::from_utf8_lossy(bytes);
    for canary in CANARIES {
        assert!(!haystack.contains(canary), "{what} contains {canary}");
    }
}

/// Every file under `dir`, recursively.
fn files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(files(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[tokio::test]
async fn i11_no_secret_reaches_output_logs_archive_or_debug() {
    let env = Env::new();
    let from = env.root.path().join("sdk-token");
    let sdk_file = |client_id: &str, expires_at: serde_json::Value| {
        serde_json::to_string_pretty(&serde_json::json!({
            "client_id": client_id,
            "access_token": access_token(),
            "refresh_token": REFRESH,
            "expires_at": expires_at,
        }))
        .unwrap()
    };

    // The binary: import, status, and both refusals.
    let mut outputs = Vec::new();
    let mut run = |file: String, args: &[&str]| {
        fs::write(&from, file).unwrap();
        let out = env.run(args);
        outputs.push((args.join(" "), out));
    };
    let import = ["auth", "import", "--from", from.to_str().unwrap()];
    run(sdk_file("other-client", 4_102_444_800u64.into()), &import);
    run(sdk_file(CLIENT, ACCOUNT.into()), &import);
    run(sdk_file(CLIENT, 4_102_444_800u64.into()), &import);
    run(String::new(), &["auth", "status"]);
    let codes: Vec<_> = outputs.iter().map(|(_, o)| o.status.code()).collect();
    assert_eq!(codes, [Some(70), Some(70), Some(0), Some(0)]);
    for (args, out) in &outputs {
        assert_clean(&format!("stdout of {args}"), &out.stdout);
        assert_clean(&format!("stderr of {args}"), &out.stderr);
    }
    assert!(text(&outputs[3].1.stdout).contains("refresh_token=present"));

    // In process, under the production log filter: the stored token drives
    // requests that succeed and fail, recorded into the archive.
    let capture = Capture::default();
    let writer = capture.clone();
    let _guard =
        tracing::subscriber::set_default(logging::subscriber(Level::INFO, move || writer.clone()));
    let config = Config::load(&env.config).unwrap();
    let client = ClientId::parse(CLIENT).unwrap();
    crate::dotenv_reach::assert_no_dotenv_reach();
    let storage = DataDirTokenStorage::open(config.oauth_dir()).unwrap();
    let oauth = auth::build_oauth(&client, storage.clone(), Duration::from_secs(5))
        .await
        .unwrap();
    let server = FakeServer::start(|seen| match seen.path.as_str() {
        "/v1/quote/financial-consensus-detail" => Reply::data(r#"{"eps":1.5}"#),
        "/v1/quote/forecast-eps" => Reply::new(
            200,
            format!(r#"{{"code":429002,"message":"member {ACCOUNT} retry after: 0.2s"}}"#),
        ),
        "/v1/quote/institution-ratings" => {
            Reply::new(500, format!("<html>{ACCOUNT}</html>")).header("x-trace-id", ACCOUNT)
        }
        "/v1/quote/valuation" => Reply::new(401, format!(r#"{{"error":"{ACCOUNT}"}}"#)),
        _ => Reply::new(
            200,
            format!(r#"{{"code":2601400,"message":"no data for {ACCOUNT}","data":null}}"#),
        ),
    })
    .await;
    let source = source(&server.url, oauth.clone());
    let store = Store::open(&config.archive_path()).await.unwrap();
    let plan = store
        .plan_run(&plan_request(1, &["AAPL.US"], &[]))
        .await
        .unwrap();
    let catalog = Catalog::compiled();
    let mut statuses = Vec::new();
    for (i, e) in catalog
        .endpoints()
        .iter()
        .filter(|e| e.id.as_str() != "index_constituents")
        .enumerate()
    {
        let fetched = source.fetch(e, &key("AAPL.US")).await;
        let recorded = store
            .record_observation(
                plan.run_id,
                &key("AAPL.US"),
                e.id,
                at(1, 10 + i as i64),
                fetched.response(),
            )
            .await
            .unwrap();
        statuses.push(recorded.status.as_str());
    }
    assert!(statuses.contains(&Status::Ok.as_str()));
    assert!(
        statuses.iter().filter(|s| **s != "ok").count() >= 4,
        "{statuses:?}"
    );
    store.close().await;
    // The canaries did flow: the fake saw the token.
    let bearer = format!("Bearer {}", access_token());
    assert!(
        server
            .seen()
            .iter()
            .all(|s| s.header("authorization") == Some(&*bearer))
    );
    assert!(
        fs::read_to_string(env.token_path())
            .unwrap()
            .contains(REFRESH)
    );

    assert_clean("our logs", capture.text().as_bytes());
    for path in files(&env.data_dir) {
        if path == env.token_path() {
            continue;
        }
        assert_clean(&path.display().to_string(), &fs::read(&path).unwrap());
    }
    let loaded = storage.load_checked(&client).unwrap().unwrap();
    for (what, rendered) in [
        ("storage", format!("{storage:?}")),
        ("loaded token", format!("{loaded:?}")),
        ("loaded token, pretty", format!("{loaded:#?}")),
        ("oauth", format!("{oauth:?}")),
        ("config", format!("{config:?}")),
    ] {
        assert_clean(what, rendered.as_bytes());
    }
    // Control: the SDK's own type would print the token.
    let sdk: StoredToken = loaded.into();
    assert!(format!("{sdk:?}").contains(ACCESS));
}

/// I11, the browser callback: whoever reaches the login's callback port
/// chooses the `error` query parameter, which the SDK logs and embeds in
/// its error (`oauth/src/callback.rs:41-47,94-99`). A real `auth login` that
/// receives a callback carrying a canary prints none of it.
#[test]
fn i11_login_callback_text_never_reaches_output() {
    let env = Env::new();
    let mut child = env
        .command(&["auth", "login"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (url_tx, url_rx) = mpsc::channel();
    let (all_tx, all_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut all = String::new();
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if line.starts_with("https://") {
                let _ = url_tx.send(());
            }
            all.push_str(&line);
            all.push('\n');
        }
        let _ = all_tx.send(all);
    });
    url_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("login printed no URL");

    // The callback, as a browser would send it, with the canary as `error`.
    let mut callback = TcpStream::connect(("127.0.0.1", env.callback_port)).unwrap();
    write!(
        callback,
        "GET /callback?error={ACCOUNT}%20{ACCESS} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut reply = String::new();
    let _ = callback.read_to_string(&mut reply);
    assert!(reply.starts_with("HTTP/1.1 400"), "{reply}");

    let status = child.wait().unwrap();
    let stderr = all_rx.recv_timeout(Duration::from_secs(60)).unwrap();
    let mut out = Vec::new();
    stdout.read_to_end(&mut out).unwrap();
    assert_eq!(status.code(), Some(75), "{stderr}");
    assert!(
        stderr.contains("login failed: authorization_failed"),
        "{stderr}"
    );
    assert_clean("stderr of auth login", stderr.as_bytes());
    assert_clean("stdout of auth login", &out);
    assert!(!env.token_path().exists());
}
