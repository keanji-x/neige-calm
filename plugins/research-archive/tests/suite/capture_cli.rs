//! `research-archive capture` as a subprocess, in real time: a kill during
//! the fetch (I4), auth that cannot be established (I9), the lock (I10)
//! and the calendar refresh. Fabricated tokens only.
//!
//! The membership phase cannot be reached in real time at every hour: a
//! window starts at 16:00 New York time or later and the carry deadline is
//! at most 600 minutes after it, so from about 10:00 to 16:00 New York time
//! every window's carry deadline has passed. The kill during membership
//! (I4) and I12 therefore run in process on a fake clock (`net::interrupt`).

use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    os::unix::fs::{MetadataExt, PermissionsExt},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use research_archive::types::RunId;

use crate::archive::{Knobs, members};
use crate::capture_proc::{Proc, code, kill, seen, text, unix_now};
use crate::fake::{FakeServer, Reply, Seen};
use crate::support::raw;

const TOKEN: &str = "ap_m_PROC-TOKEN-0c1d";
const FAR: u64 = 4_102_444_800;
const CONSTITUENTS: &str = "/v1/quote/index-constituents";

/// Members for any index; a delay on `slow_path` (every path for `*`) while
/// `slow` is set.
async fn server(slow: Arc<AtomicBool>, slow_path: &'static str) -> FakeServer {
    FakeServer::start(move |s: &Seen| {
        let reply = if s.path == CONSTITUENTS {
            Reply::data(&members(&["AAPL.US", "MSFT.US"]))
        } else {
            Reply::data(r#"{"v":1}"#)
        };
        if (slow_path == "*" || s.path == slow_path) && slow.load(Ordering::SeqCst) {
            reply.after(Duration::from_secs(120))
        } else {
            reply
        }
    })
    .await
}

fn index_knobs() -> Knobs {
    Knobs {
        research: vec!["AAPL.US"],
        indexes: vec![".SPX.US"],
        ..Knobs::default()
    }
}

/// `(key, origin)` of every `run_key` row, and `(index, mode)` of every
/// `run_membership` row, and the run's end.
async fn universe(p: &Proc) -> (Vec<(String, String)>, Vec<(String, String)>, Option<String>) {
    let mut conn = raw(&p.archive.data_dir.join("archive.sqlite3"), false).await;
    let keys = sqlx::query_as("SELECT key, origin FROM run_key ORDER BY key, origin")
        .fetch_all(&mut conn)
        .await
        .unwrap();
    let frozen = sqlx::query_as("SELECT index_sym, mode FROM run_membership ORDER BY index_sym")
        .fetch_all(&mut conn)
        .await
        .unwrap();
    let end = sqlx::query_scalar("SELECT outcome FROM run_end")
        .fetch_optional(&mut conn)
        .await
        .unwrap();
    (keys, frozen, end)
}

fn pairs(rows: &[(&str, &str)]) -> Vec<(String, String)> {
    rows.iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect()
}

/// I4: SIGKILL while a per-key request is in flight. The freeze is whole,
/// the run is open, and a rerun completes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn i4_a_kill_during_fetch_leaves_a_whole_freeze_and_a_rerun_completes() {
    let slow = Arc::new(AtomicBool::new(true));
    let server = server(slow.clone(), "/v1/quote/forecast-eps").await;
    let p = Proc::new(&server.url, index_knobs());
    assert_eq!(code(&p.import(TOKEN, None, FAR).await), 0);
    let child = p.spawn_capture();
    seen(&server, |s| s.path == "/v1/quote/forecast-eps").await;
    kill(child).await;

    let (keys, frozen, end) = universe(&p).await;
    // The carry deadline of the real-time window has passed, so the start
    // froze at once from the stored rows: `none`, no members.
    assert_eq!(frozen, pairs(&[(".SPX.US", "none")]));
    assert_eq!(keys.len(), 2, "{keys:?}");
    assert_eq!(end, None);

    slow.store(false, Ordering::SeqCst);
    let out = p.capture().await;
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    let (_, _, end) = universe(&p).await;
    assert_eq!(end.as_deref(), Some("complete"));
    let coverage = p.archive.store().await.coverage(RunId(1)).await.unwrap();
    assert_eq!(coverage.len(), 7);
    assert!(coverage.iter().all(|c| c.has_ok));
}

/// I9: a token inside the SDK's five-minute margin with no refresh token
/// fails the refresh locally (`NoRefreshToken`), so the SDK falls back to
/// the browser flow, which exits `AUTH_UNAVAILABLE` at once. Nothing stays
/// listening, and after a fresh token is imported the next start completes
/// inside the window.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn i9_an_unusable_token_exits_75_and_a_fresh_import_recovers() {
    let server = server(Arc::new(AtomicBool::new(false)), "").await;
    let p = Proc::new(&server.url, Knobs::default());
    assert_eq!(code(&p.import(TOKEN, None, unix_now() + 120).await), 0);
    let started = Instant::now();
    let out = p.capture().await;
    assert_eq!(code(&out), 75, "{}", text(&out.stderr));
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
    assert!(text(&out.stderr).contains("OAuth browser authorization required"));
    assert!(server.seen().is_empty());
    let state = p.archive.last_exit();
    assert_eq!(
        (state["kind"].as_str(), state["code"].as_i64()),
        (Some("auth_unavailable"), Some(75))
    );
    assert_eq!(state["as_of"], p.as_of.as_str());
    // The process is gone and the registered callback port is free.
    let config = p.archive.config();
    TcpListener::bind(("127.0.0.1", config.callback_port.get())).unwrap();

    assert_eq!(code(&p.import(TOKEN, None, FAR).await), 0);
    let out = p.capture().await;
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    assert_eq!(p.archive.last_exit()["kind"], "complete");
    // The run planned by the failed start is the one that completed.
    let plan = p.archive.store().await.plan(RunId(1)).await.unwrap();
    assert_eq!(plan.as_of.to_string(), p.as_of);
    assert_eq!(plan.ended.map(|o| o.as_str()), Some("complete"));
}

/// A token refresh that hangs (the OAuth host reached through a proxy that
/// never answers) is dropped at the run deadline, and the start exits
/// `AUTH_UNAVAILABLE` instead of hanging. Hermetic: the refresh goes to the
/// local listener as `HTTPS_PROXY`. Takes the minimum attempt timeout, one
/// minute.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hanging_token_refresh_is_dropped_at_the_deadline() {
    let server = server(Arc::new(AtomicBool::new(false)), "").await;
    let p = Proc::new(
        &server.url,
        Knobs {
            attempt_timeout_minutes: 1,
            ..Knobs::default()
        },
    );
    assert_eq!(
        code(
            &p.import(TOKEN, Some("PROC-REFRESH"), unix_now() + 120)
                .await
        ),
        0
    );
    let token_before = std::fs::read(p.token_path()).unwrap();
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy = format!("http://{}", silent.local_addr().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in silent.incoming() {
            let stream = stream.unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            let _ = tx.send(line);
            held.push(stream);
        }
    });
    let mut command = p.command(&["capture"]);
    // reqwest 0.11 (oauth2's client) lets ALL_PROXY override HTTPS_PROXY.
    command.env("HTTPS_PROXY", &proxy).env("ALL_PROXY", &proxy);
    let started = Instant::now();
    let out = tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(code(&out), 75, "{}", text(&out.stderr));
    assert!(
        elapsed >= Duration::from_secs(55) && elapsed < Duration::from_secs(90),
        "{elapsed:?}: {}",
        text(&out.stderr)
    );
    let connect = rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(
        connect.starts_with("CONNECT openapi.longbridge.cn:443"),
        "{connect}"
    );
    assert!(server.seen().is_empty());
    assert_eq!(std::fs::read(p.token_path()).unwrap(), token_before);
    assert_eq!(p.archive.last_exit()["kind"], "auth_unavailable");
    let _ = std::io::stdout().flush();
}

/// I10: while a capture holds the lock, a second capture exits `BUSY` (0)
/// without reading the token or sending a request, and `auth import` does
/// nothing. The token file is made unreadable while the first capture
/// runs: a second process that tried to load it would fail to and exit 75.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn i10_a_second_start_is_busy_without_the_token_or_a_request() {
    // Every request hangs, so the first capture holds its four slots.
    let server = server(Arc::new(AtomicBool::new(true)), "*").await;
    let p = Proc::new(&server.url, Knobs::default());
    assert_eq!(code(&p.import(TOKEN, None, FAR).await), 0);
    let first = p.spawn_capture();
    let in_flight = research_archive::capture::MAX_IN_FLIGHT;
    let started = Instant::now();
    while server.seen().len() < in_flight {
        assert!(started.elapsed() < Duration::from_secs(30));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let token = p.token_path();
    let bytes = std::fs::read(&token).unwrap();
    let meta = std::fs::metadata(&token).unwrap();
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o000)).unwrap();

    let out = p.capture().await;
    let stderr = text(&out.stderr);
    assert_eq!(code(&out), 0, "{stderr}");
    assert!(stderr.contains("held by another process"), "{stderr}");
    assert_eq!(server.seen().len(), in_flight);

    let out = p.import("ap_m_OTHER", None, FAR).await;
    assert_eq!(code(&out), research_archive::exit::LOCKED);
    assert!(text(&out.stderr).contains("held by another process"));
    assert!(!text(&out.stdout).contains("imported"));

    kill(first).await;
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    let after = std::fs::metadata(&token).unwrap();
    assert_eq!(std::fs::read(&token).unwrap(), bytes);
    assert_eq!(
        (after.ino(), after.mtime_nsec()),
        (meta.ino(), meta.mtime_nsec())
    );
    assert_eq!(server.seen().len(), in_flight);
}

/// A day-old calendar needs a refresh, which needs the token: with none
/// stored, the start exits `AUTH_UNAVAILABLE` before any request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_calendar_without_a_token_exits_75() {
    let server = server(Arc::new(AtomicBool::new(false)), "").await;
    let p = Proc::new(&server.url, Knobs::default());
    p.archive.calendar(
        research_archive::types::UnixMs(0),
        "2026-01-01",
        "2026-01-02",
        &[],
    );
    let out = p.capture().await;
    assert_eq!(code(&out), 75, "{}", text(&out.stderr));
    assert!(server.seen().is_empty());
    assert_eq!(p.archive.last_exit()["kind"], "auth_unavailable");
}

/// A refresh that cannot reach the quote server (a closed loopback port)
/// fails the start as `FATAL`, and no catalog request is sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_calendar_refresh_is_fatal_without_a_catalog_request() {
    let server = server(Arc::new(AtomicBool::new(false)), "").await;
    let p = Proc::new(&server.url, Knobs::default());
    assert_eq!(code(&p.import(TOKEN, None, FAR).await), 0);
    std::fs::remove_file(research_archive::calendar::TradingCalendar::path(
        &p.archive.data_dir,
    ))
    .unwrap();
    let out = p.capture().await;
    assert_eq!(code(&out), 70, "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("refreshing the calendar failed"));
    assert!(
        server
            .seen()
            .iter()
            .all(|s| !s.path.starts_with("/v1/quote/")),
        "{:?}",
        server.seen()
    );
    assert_eq!(p.archive.last_exit()["kind"], "fatal");
}
