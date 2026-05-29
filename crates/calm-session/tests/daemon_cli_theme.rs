//! #177 PR2 — `calm-session-daemon` `--terminal-fg` / `--terminal-bg`
//! CLI argument coverage.
//!
//! Three things this file pins:
//!   1. Both flags REQUIRED in terminal mode. Default `Mode::Terminal`
//!      with neither flag → daemon exits non-zero before binding the
//!      socket. (clap's `required_if_eq` doesn't fire on the default
//!      arm — we re-check in `run_terminal` and `anyhow::bail!` with
//!      a descriptive error.)
//!   2. Malformed RGB rejected at clap parse time (non-numeric channel,
//!      out-of-range channel, wrong arity). The daemon should not even
//!      start in those cases.
//!   3. Well-formed flags accepted: daemon parses successfully and
//!      reaches socket-bind. We don't need to exchange frames here —
//!      `tests/v2_render_plane.rs` / `tests/terminal_handler_model.rs`
//!      cover the in-memory OSC reply behaviour at the model layer.
//!
//! Chat mode is not exercised here — the args are intentionally ignored
//! when `--mode chat` (no `TerminalModel` to apply colors to).

use std::io::Read;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use uuid::Uuid;

/// Path to the daemon binary cargo built for this test crate.
fn daemon_bin() -> &'static str {
    env!("CARGO_BIN_EXE_calm-session-daemon")
}

fn supervisor_bin() -> &'static str {
    "calm-proc-supervisor"
}

fn locate_bin(name: &str) -> PathBuf {
    let env_key = format!("CARGO_BIN_EXE_{name}");
    if let Ok(path) = std::env::var(env_key) {
        return PathBuf::from(path);
    }
    let me = std::env::current_exe().expect("current_exe");
    let target_profile = me
        .parent()
        .and_then(|p| p.parent())
        .expect("test bin parent");
    let candidate = target_profile.join(name);
    if candidate.exists() {
        return candidate;
    }
    panic!("{name} binary not found at {}", candidate.display());
}

#[allow(clippy::zombie_processes)]
fn spawn_supervisor() -> (Child, PathBuf, TempDir) {
    let temp = tempfile::tempdir().expect("tempdir");
    let sock = temp.path().join("proc-supervisor.sock");
    let mut child = std::process::Command::new(locate_bin(supervisor_bin()))
        .arg("--control-sock")
        .arg(&sock)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn supervisor");
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("try_wait supervisor") {
            panic!("supervisor exited before listening: {status}");
        }
        if UnixStream::connect(&sock).is_ok() {
            return (child, sock, temp);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("supervisor never listened on {}", sock.display());
}

/// Block until either the socket file exists OR the child exits, then
/// return whichever happened first. Mirrors the kernel-side readiness
/// poll (3s budget) but stops early on child exit so missing-flag tests
/// don't waste their full timeout.
fn wait_bind_or_exit(child: &mut std::process::Child, sock: &std::path::Path) -> Outcome {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return Outcome::Exited(status.code());
        }
        if sock.exists() {
            return Outcome::Bound;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    Outcome::Hung
}

#[derive(Debug)]
enum Outcome {
    Bound,
    Exited(Option<i32>),
    Hung,
}

fn fresh_sock(label: &str) -> std::path::PathBuf {
    let id = Uuid::new_v4();
    let p = std::env::temp_dir().join(format!("calm-cli-theme-{label}-{id}.sock"));
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn missing_both_flags_fails_fast() {
    // Default mode is `terminal`. Neither flag set → daemon must exit
    // non-zero before binding AND the stderr bail message must point
    // operators at the missing `--terminal-fg`. PR1's NOT NULL row
    // invariant makes this an upstream-bug signal — never legitimate in
    // steady state. Pinning the stderr substring guards against a
    // regression to a generic "missing argument" message.
    let sock = fresh_sock("nobars");
    let id = Uuid::new_v4().to_string();
    let mut child = std::process::Command::new(daemon_bin())
        .args(["--id", &id])
        .args(["--sock", &sock.to_string_lossy()])
        .args(["--", "/bin/true"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let outcome = wait_bind_or_exit(&mut child, &sock);
    let mut stderr = String::new();
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_string(&mut stderr);
    }
    match outcome {
        Outcome::Exited(code) => assert_ne!(
            code,
            Some(0),
            "missing flags must yield non-zero exit, got code={code:?}, stderr={stderr:?}",
        ),
        other => panic!("expected non-zero exit on missing flags, got {other:?}"),
    }
    assert!(
        stderr.contains("--terminal-fg is required"),
        "expected stderr to mention `--terminal-fg is required`, got {stderr:?}",
    );
}

#[test]
fn missing_only_bg_fails_fast() {
    // Asymmetric case: only fg supplied. Both must be present together,
    // and the stderr bail must name the specific missing flag so an
    // operator doesn't have to grep source to fix the regression.
    let sock = fresh_sock("no-bg");
    let id = Uuid::new_v4().to_string();
    let mut child = std::process::Command::new(daemon_bin())
        .args(["--id", &id])
        .args(["--sock", &sock.to_string_lossy()])
        .args(["--terminal-fg", "216,219,226"])
        .args(["--", "/bin/true"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let outcome = wait_bind_or_exit(&mut child, &sock);
    let mut stderr = String::new();
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_string(&mut stderr);
    }
    assert!(
        matches!(outcome, Outcome::Exited(code) if code != Some(0)),
        "expected non-zero exit when only --terminal-fg given, got {outcome:?}, stderr={stderr:?}",
    );
    assert!(
        stderr.contains("--terminal-bg is required"),
        "expected stderr to mention `--terminal-bg is required`, got {stderr:?}",
    );
}

#[test]
fn malformed_rgb_non_numeric_channel_rejected() {
    // `parse_rgb` is plugged in as clap's value_parser → bad input
    // fails at parse time with exit code 2 (clap's standard "usage").
    let sock = fresh_sock("nan");
    let id = Uuid::new_v4().to_string();
    let mut child = std::process::Command::new(daemon_bin())
        .args(["--id", &id])
        .args(["--sock", &sock.to_string_lossy()])
        .args(["--terminal-fg", "ff,gg,hh"]) // hex-like but not decimal
        .args(["--terminal-bg", "15,20,24"])
        .args(["--", "/bin/true"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let outcome = wait_bind_or_exit(&mut child, &sock);
    assert!(
        matches!(outcome, Outcome::Exited(code) if code != Some(0)),
        "expected non-zero exit on malformed RGB, got {outcome:?}",
    );
}

#[test]
fn malformed_rgb_out_of_range_channel_rejected() {
    // Channel > 255 → u8::parse fails inside `parse_rgb`.
    let sock = fresh_sock("oor");
    let id = Uuid::new_v4().to_string();
    let mut child = std::process::Command::new(daemon_bin())
        .args(["--id", &id])
        .args(["--sock", &sock.to_string_lossy()])
        .args(["--terminal-fg", "300,20,24"])
        .args(["--terminal-bg", "15,20,24"])
        .args(["--", "/bin/true"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let outcome = wait_bind_or_exit(&mut child, &sock);
    assert!(
        matches!(outcome, Outcome::Exited(code) if code != Some(0)),
        "expected non-zero exit on out-of-range channel, got {outcome:?}",
    );
}

#[test]
fn malformed_rgb_wrong_arity_rejected() {
    // Only two channels → `parts.len() != 3` branch of `parse_rgb`.
    let sock = fresh_sock("arity");
    let id = Uuid::new_v4().to_string();
    let mut child = std::process::Command::new(daemon_bin())
        .args(["--id", &id])
        .args(["--sock", &sock.to_string_lossy()])
        .args(["--terminal-fg", "15,20"])
        .args(["--terminal-bg", "15,20,24"])
        .args(["--", "/bin/true"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let outcome = wait_bind_or_exit(&mut child, &sock);
    assert!(
        matches!(outcome, Outcome::Exited(code) if code != Some(0)),
        "expected non-zero exit on wrong arity, got {outcome:?}",
    );
}

#[test]
fn well_formed_flags_bind_socket() {
    // Happy path: both flags parsed correctly → daemon reaches the
    // listen() call and the socket file appears before the child exits.
    // We use `sleep 30` so the daemon doesn't immediately exit and
    // race-delete the socket out from under our `sock.exists()` check.
    let sock = fresh_sock("ok");
    let (mut supervisor, supervisor_sock, _supervisor_temp) = spawn_supervisor();
    let id = Uuid::new_v4().to_string();
    let mut child = std::process::Command::new(daemon_bin())
        .args(["--id", &id])
        .args(["--sock", &sock.to_string_lossy()])
        .arg("--proc-supervisor-sock")
        .arg(&supervisor_sock)
        .args(["--terminal-fg", "216,219,226"])
        .args(["--terminal-bg", "15,20,24"])
        .args(["--", "sh", "-c", "sleep 30"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let outcome = wait_bind_or_exit(&mut child, &sock);
    let _ = child.kill();
    let _ = child.wait();
    let _ = supervisor.kill();
    let _ = supervisor.wait();
    let _ = std::fs::remove_file(&sock);
    assert!(
        matches!(outcome, Outcome::Bound),
        "expected daemon to bind socket with well-formed flags, got {outcome:?}",
    );
}

#[test]
fn parse_rgb_accepts_whitespace_around_channels() {
    // `parse_rgb` trims each channel; `--terminal-fg ' 15 , 20 , 24 '`
    // should round-trip to (15, 20, 24).
    let sock = fresh_sock("ws");
    let (mut supervisor, supervisor_sock, _supervisor_temp) = spawn_supervisor();
    let id = Uuid::new_v4().to_string();
    let mut child = std::process::Command::new(daemon_bin())
        .args(["--id", &id])
        .args(["--sock", &sock.to_string_lossy()])
        .arg("--proc-supervisor-sock")
        .arg(&supervisor_sock)
        .args(["--terminal-fg", " 216 , 219 , 226 "])
        .args(["--terminal-bg", "15,20,24"])
        .args(["--", "sh", "-c", "sleep 30"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let outcome = wait_bind_or_exit(&mut child, &sock);
    let _ = child.kill();
    let _ = child.wait();
    let _ = supervisor.kill();
    let _ = supervisor.wait();
    let _ = std::fs::remove_file(&sock);
    assert!(
        matches!(outcome, Outcome::Bound),
        "whitespace-padded RGB must still parse, got {outcome:?}",
    );
}
