//! Process-level fixtures for `research-archive capture` and `notify`: the
//! binary in a cleared environment, real time, a calendar seeded so that a
//! window is open now, and the fake server as the only reachable host.
//!
//! Every run gets an empty environment plus proxies that point at a closed
//! local port, with `NO_PROXY=127.0.0.1` so that only the fake server on
//! loopback is reachable. The quote WebSocket URL is a closed loopback port,
//! and the seeded calendar is fresh, so no refresh is attempted unless a
//! test makes it stale on purpose.

use std::{
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

use chrono::Days;
use research_archive::calendar::ny_date;
use research_archive::clock::{Clock, SystemClock};

use crate::capture_support::{Archive, CLIENT, Knobs, closed_port};
use crate::fake::{FakeServer, Seen};

/// An archive whose window is open for the rest of today, New York: the
/// calendar lists yesterday and tomorrow but not today, so the window runs
/// from yesterday 16:00 to tomorrow 09:30.
pub struct Proc {
    pub archive: Archive,
    pub cwd: PathBuf,
    /// Yesterday in New York: the open window's trading day.
    pub as_of: String,
}

impl Proc {
    pub fn new(url: &str, knobs: Knobs) -> Proc {
        let knobs = Knobs {
            window_start: "16:00",
            ..knobs
        };
        let archive = Archive::new(url, &knobs);
        let cwd = archive.root.path().join("work");
        std::fs::create_dir(&cwd).unwrap();
        let today = ny_date(SystemClock.now());
        let as_of = (today - Days::new(1)).to_string();
        let tomorrow = (today + Days::new(1)).to_string();
        archive.calendar(
            SystemClock.now(),
            &(today - Days::new(3)).to_string(),
            &(today + Days::new(3)).to_string(),
            &[&as_of, &tomorrow],
        );
        Proc {
            archive,
            cwd,
            as_of,
        }
    }

    /// The binary with the controlled environment.
    pub fn command(&self, args: &[&str]) -> Command {
        let proxy = format!("http://127.0.0.1:{}", closed_port());
        let mut command = Command::new(env!("CARGO_BIN_EXE_research-archive"));
        command
            .env_clear()
            .envs([
                ("HTTP_PROXY", proxy.as_str()),
                ("HTTPS_PROXY", proxy.as_str()),
                ("ALL_PROXY", proxy.as_str()),
                ("NO_PROXY", "127.0.0.1"),
            ])
            .current_dir(&self.cwd)
            .args(args)
            .arg("--config")
            .arg(&self.archive.config_path);
        command
    }

    /// Run to completion off the async runtime, so the fake server keeps
    /// answering.
    pub async fn run(&self, args: &[&str]) -> Output {
        let mut command = self.command(args);
        tokio::task::spawn_blocking(move || command.output().unwrap())
            .await
            .unwrap()
    }

    pub async fn capture(&self) -> Output {
        self.run(&["capture"]).await
    }

    /// Start `capture` in the background.
    pub fn spawn_capture(&self) -> Child {
        self.command(&["capture"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    /// Write an SDK-shaped token file and `auth import` it.
    pub async fn import(&self, access: &str, refresh: Option<&str>, expires_at: u64) -> Output {
        let from = self.archive.root.path().join("sdk-token.json");
        let json = serde_json::to_string_pretty(&serde_json::json!({
            "client_id": CLIENT,
            "access_token": access,
            "refresh_token": refresh,
            "expires_at": expires_at,
        }))
        .unwrap();
        std::fs::write(&from, json).unwrap();
        self.run(&["auth", "import", "--from", from.to_str().unwrap()])
            .await
    }

    pub fn token_path(&self) -> PathBuf {
        self.archive
            .data_dir
            .join("oauth")
            .join(format!("{CLIENT}.json"))
    }
}

/// Unix seconds now.
pub fn unix_now() -> u64 {
    u64::try_from(SystemClock.now().0 / 1000).unwrap()
}

/// Wait until `server` has seen a request matching `pred`.
pub async fn seen(server: &FakeServer, pred: impl Fn(&Seen) -> bool) {
    let started = Instant::now();
    while !server.seen().iter().any(&pred) {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the request never came: {:?}",
            server.seen()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// SIGKILL `child` and reap it.
pub async fn kill(mut child: Child) {
    child.kill().unwrap();
    tokio::task::spawn_blocking(move || child.wait().unwrap())
        .await
        .unwrap();
}

pub fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The exit code, with stderr in the failure message.
#[track_caller]
pub fn code(out: &Output) -> i32 {
    out.status
        .code()
        .unwrap_or_else(|| panic!("killed: {}", text(&out.stderr)))
}
