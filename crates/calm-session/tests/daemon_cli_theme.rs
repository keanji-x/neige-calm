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

use std::process::Stdio;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Path to the daemon binary cargo built for this test crate.
fn daemon_bin() -> &'static str {
    env!("CARGO_BIN_EXE_calm-session-daemon")
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
    // non-zero before binding. PR1's NOT NULL row invariant makes this
    // an upstream-bug signal — never legitimate in steady state.
    let sock = fresh_sock("nobars");
    let id = Uuid::new_v4().to_string();
    let mut child = std::process::Command::new(daemon_bin())
        .args(["--id", &id])
        .args(["--sock", &sock.to_string_lossy()])
        .args(["--", "/bin/true"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let outcome = wait_bind_or_exit(&mut child, &sock);
    match outcome {
        Outcome::Exited(code) => assert_ne!(
            code,
            Some(0),
            "missing flags must yield non-zero exit, got code={code:?}",
        ),
        other => panic!("expected non-zero exit on missing flags, got {other:?}"),
    }
}

#[test]
fn missing_only_bg_fails_fast() {
    // Asymmetric case: only fg supplied. Both must be present together.
    let sock = fresh_sock("no-bg");
    let id = Uuid::new_v4().to_string();
    let mut child = std::process::Command::new(daemon_bin())
        .args(["--id", &id])
        .args(["--sock", &sock.to_string_lossy()])
        .args(["--terminal-fg", "216,219,226"])
        .args(["--", "/bin/true"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let outcome = wait_bind_or_exit(&mut child, &sock);
    assert!(
        matches!(outcome, Outcome::Exited(code) if code != Some(0)),
        "expected non-zero exit when only --terminal-fg given, got {outcome:?}",
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
    let id = Uuid::new_v4().to_string();
    let mut child = std::process::Command::new(daemon_bin())
        .args(["--id", &id])
        .args(["--sock", &sock.to_string_lossy()])
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
    let id = Uuid::new_v4().to_string();
    let mut child = std::process::Command::new(daemon_bin())
        .args(["--id", &id])
        .args(["--sock", &sock.to_string_lossy()])
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
    let _ = std::fs::remove_file(&sock);
    assert!(
        matches!(outcome, Outcome::Bound),
        "whitespace-padded RGB must still parse, got {outcome:?}",
    );
}
