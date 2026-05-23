//! Test fixture (#177): a stand-in for the `codex` CLI that runs inside
//! a `calm-session-daemon` PTY, performs the same OSC 11 (default
//! background color) startup probe the real codex client does, and
//! writes the outcome to a sidecar result file so the test driver can
//! assert.
//!
//! This is the missing layer in our test pyramid: every other test
//! around #177 stops at either the spawn argv (the `argv-recorder-daemon`
//! fixture) or the daemon's internal vte parser (`calm-session/tests/v2_*`).
//! Nothing exercises the *full* round-trip — PTY child writes OSC 11
//! query → daemon's vte sees it → `TerminalModel::osc_color_query`
//! generates a reply → reply written back to PTY master → child reads
//! it from stdin and parses the RGB. If any link drops the theme, this
//! fixture surfaces it.
//!
//! ## Wire protocol (mirrors crossterm / codex behaviour)
//!
//! 1. Put stdin into raw mode (no line discipline, no echo) so the OSC
//!    reply bytes aren't held up waiting for a newline.
//! 2. Write `\x1b]11;?\x1b\\` to stdout. The daemon's `TerminalModel`
//!    parses this via vte and synthesizes `\x1b]11;rgb:RRRR/GGGG/BBBB\x1b\\`
//!    onto the PTY master.
//! 3. Read from stdin with `poll(2)` until we see an OSC `ST` (`\x1b\\`)
//!    or `BEL` (`\x07`) terminator, or a bounded timeout fires.
//! 4. Parse `\x1b]11;rgb:RRRR/GGGG/BBBB` — xterm convention is 16-bit
//!    per channel (4 hex nibbles), so divide by 257 (≈ 0xffff/0xff) to
//!    recover the original u8.
//! 5. Compare against `--expected-bg`. Write `OK` to `--result` on
//!    match, `FAIL: ...` otherwise.
//!
//! `--probe-twice` mode adds a second read after a configurable wait
//! (no second query — the daemon's mid-session `TerminalThemeUpdate`
//! path writes an unsolicited OSC 10/11 pair to the PTY, so we just
//! need to listen). Used by the mid-session toggle test.
//!
//! ## Why a fixture rather than the real codex CLI
//!
//! The real codex is a 50MB Node binary that needs auth + network
//! access. A 200-line Rust fixture exercises the same byte protocol
//! deterministically, cheaply (single static link, no fork+exec storm),
//! and offline.

#![cfg(unix)]

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

/// Parse `R,G,B` from a CLI arg into a `(u8, u8, u8)` tuple. Same
/// shape as the daemon's `--terminal-fg` value parser — keep them
/// aligned so a test can pass the same string to both sides.
fn parse_rgb(s: &str) -> Result<(u8, u8, u8), String> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 3 {
        return Err(format!("expected r,g,b got {s:?}"));
    }
    let p = |i: usize| {
        parts[i]
            .trim()
            .parse::<u8>()
            .map_err(|e| format!("ch{i}: {e}"))
    };
    Ok((p(0)?, p(1)?, p(2)?))
}

/// Walk argv looking for `--<key> <value>` pairs. Returns the value
/// for the first match. Tiny hand-rolled parser — pulling in clap
/// for a 4-arg fixture is overkill.
fn arg(name: &str) -> Option<String> {
    let av: Vec<String> = std::env::args().collect();
    for i in 1..av.len().saturating_sub(1) {
        if av[i] == name {
            return Some(av[i + 1].clone());
        }
    }
    None
}

fn flag(name: &str) -> bool {
    std::env::args().any(|a| a == name)
}

/// Test-driver entry points: argv first, fall back to env vars.
///
/// The argv path is used when the test driver spawns the fixture
/// directly via `spawn_daemon_with_parts` and controls the program
/// arg. The env-var path is needed when the fixture is invoked
/// indirectly — e.g. when the codex-cards endpoint hard-codes the
/// program name as `"codex"` and we symlink `osc-probe-child` onto
/// PATH as `codex`. With no control over argv we pass parameters
/// via env: `NEIGE_OSC_RESULT_PATH`, `NEIGE_OSC_EXPECTED_BG`,
/// `NEIGE_OSC_EXPECTED_BG_2`, `NEIGE_OSC_PROBE_TWICE`.
fn arg_or_env(arg_name: &str, env_name: &str) -> Option<String> {
    arg(arg_name).or_else(|| std::env::var(env_name).ok())
}

fn flag_or_env(arg_name: &str, env_name: &str) -> bool {
    flag(arg_name)
        || std::env::var(env_name)
            .ok()
            .map(|v| !v.is_empty() && v != "0" && v != "false")
            .unwrap_or(false)
}

/// Open `/dev/tty` (the controlling terminal). The PTY slave the
/// daemon spawned us against IS our controlling tty — portable_pty
/// dups the slave fd onto stdin/stdout/stderr and `setsid + ioctl
/// TIOCSCTTY`s it as the ctty. `/dev/tty` resolves to it; opening it
/// is the canonical way to access the terminal regardless of any
/// stdio redirection (matches crossterm's `tty::TtyFd::new`).
fn open_tty() -> std::io::Result<File> {
    OpenOptions::new().read(true).write(true).open("/dev/tty")
}

/// Save current termios, switch to raw mode, return the saved state
/// so caller can restore on exit. Uses libc directly — `nix::termios`
/// would be cleaner but pulling a new feature into the production
/// `nix` dep for a test fixture isn't worth it; libc is already in
/// the transitive dep graph.
fn enter_raw(fd: i32) -> std::io::Result<libc::termios> {
    // SAFETY: zeroed termios is the documented "uninitialized" state
    // for tcgetattr to fill in.
    let mut saved: libc::termios = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::tcgetattr(fd, &mut saved) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut raw = saved;
    // SAFETY: cfmakeraw mutates termios in place — no aliasing.
    unsafe {
        libc::cfmakeraw(&mut raw);
    }
    let rc = unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(saved)
}

fn restore_termios(fd: i32, saved: &libc::termios) {
    // SAFETY: `saved` came from a successful tcgetattr; tcsetattr
    // requires a valid termios pointer + fd.
    unsafe {
        libc::tcsetattr(fd, libc::TCSANOW, saved);
    }
}

/// Read bytes from `fd` into `buf` until a terminator is seen or the
/// deadline elapses. Uses `poll(2)` for a bounded wait between reads
/// — `read(2)` on a raw-mode PTY would block forever if the daemon
/// never emits the reply.
///
/// Returns the bytes read so far (caller decides whether terminator
/// was found).
fn read_until_terminator(fd: i32, deadline: Instant) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(64);
    loop {
        let now = Instant::now();
        if now >= deadline {
            return out;
        }
        let remaining = deadline - now;
        let ms = remaining.as_millis().min(i32::MAX as u128) as i32;

        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: pfd is a valid pollfd pointer with nfds=1.
        let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
        if rc <= 0 {
            return out;
        }
        if pfd.revents & libc::POLLIN == 0 {
            return out;
        }

        let mut chunk = [0u8; 256];
        // SAFETY: chunk is a valid writable buffer of len 256.
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr() as *mut _, chunk.len()) };
        if n <= 0 {
            return out;
        }
        out.extend_from_slice(&chunk[..n as usize]);

        // Look for OSC ST (`\x1b\\`) or BEL (`\x07`) terminator. We
        // need to find one that closes the most recent OSC opener
        // — but for our purposes, any ST or BEL after at least one
        // OSC opener is enough.
        if out.contains(&0x07) || out.windows(2).any(|w| w == [0x1b, 0x5c]) {
            return out;
        }
    }
}

/// Find the OSC 11 reply in a byte stream and extract the RGB. The
/// daemon emits `\x1b]11;rgb:RRRR/GGGG/BBBB\x1b\\`; we tolerate BEL
/// (`\x07`) terminator too (xterm spec allows either).
///
/// Returns `Some((r, g, b))` if a complete OSC 11 reply with rgb:
/// payload is present.
fn parse_osc11_bg(buf: &[u8]) -> Option<(u8, u8, u8)> {
    // Find `\x1b]11;rgb:` opener
    let opener = b"\x1b]11;rgb:";
    let pos = buf.windows(opener.len()).position(|w| w == opener)?;
    let after = &buf[pos + opener.len()..];
    // Payload is `RRRR/GGGG/BBBB` terminated by ESC\ or BEL. Find
    // the terminator.
    let term_pos = after
        .iter()
        .position(|&b| b == 0x07)
        .or_else(|| after.windows(2).position(|w| w == b"\x1b\\"))?;
    let payload = std::str::from_utf8(&after[..term_pos]).ok()?;
    let parts: Vec<&str> = payload.split('/').collect();
    if parts.len() != 3 {
        return None;
    }
    let parse_ch = |s: &str| -> Option<u8> {
        let v = u16::from_str_radix(s, 16).ok()?;
        // xterm uses 16-bit per channel — divide by 257 to recover
        // the original 8-bit value. 257 = 0xffff / 0xff.
        Some((v / 257) as u8)
    };
    Some((
        parse_ch(parts[0])?,
        parse_ch(parts[1])?,
        parse_ch(parts[2])?,
    ))
}

fn main() {
    let result_path = arg_or_env("--result", "NEIGE_OSC_RESULT_PATH")
        .expect("--result <path> or NEIGE_OSC_RESULT_PATH required");
    let expected_bg = parse_rgb(
        &arg_or_env("--expected-bg", "NEIGE_OSC_EXPECTED_BG")
            .expect("--expected-bg or NEIGE_OSC_EXPECTED_BG required"),
    )
    .expect("expected-bg parse");
    let probe_twice = flag_or_env("--probe-twice", "NEIGE_OSC_PROBE_TWICE");
    let expected_bg_2 = if probe_twice {
        Some(
            parse_rgb(
                &arg_or_env("--expected-bg-2", "NEIGE_OSC_EXPECTED_BG_2")
                    .expect("--expected-bg-2 or NEIGE_OSC_EXPECTED_BG_2 required"),
            )
            .expect("expected-bg-2 parse"),
        )
    } else {
        None
    };

    // Outcome accumulator. We write the entire result file at the
    // end so a partial write under a panic doesn't leave the test
    // driver reading half a line.
    let mut outcome = String::new();
    let write_result = |outcome: &str| {
        let mut f = File::create(&result_path).expect("create result file");
        f.write_all(outcome.as_bytes()).expect("write result");
        let _ = f.sync_all();
    };

    // Open /dev/tty. If that fails (rare — daemon always gives us a
    // ctty), fall back to raw stdin/stdout.
    let mut tty = match open_tty() {
        Ok(f) => f,
        Err(e) => {
            outcome.push_str(&format!("FAIL: open /dev/tty: {e}\n"));
            write_result(&outcome);
            std::process::exit(1);
        }
    };
    let tty_fd = tty.as_raw_fd();

    let saved = match enter_raw(tty_fd) {
        Ok(s) => s,
        Err(e) => {
            outcome.push_str(&format!("FAIL: enter raw: {e}\n"));
            write_result(&outcome);
            std::process::exit(1);
        }
    };

    // Optional diagnostic dump: when `NEIGE_OSC_TRACE_PATH` is set in
    // the env, write a startup line + per-probe raw-byte dumps to
    // that path. Helps the test driver distinguish "fixture never
    // ran" (file empty) from "fixture ran, daemon didn't reply"
    // (file has startup line + read-zero-bytes dump). Off by
    // default so happy-path runs stay quiet.
    if let Ok(trace_path) = std::env::var("NEIGE_OSC_TRACE_PATH") {
        let _ = std::fs::write(
            &trace_path,
            format!(
                "fixture-startup argv={:?} probe_twice={} expected_bg={:?} expected_bg_2={:?}\n",
                std::env::args().collect::<Vec<_>>(),
                probe_twice,
                expected_bg,
                expected_bg_2,
            ),
        );
    }

    // ---- Probe 1 ----
    // Write OSC 11 query.
    if let Err(e) = tty.write_all(b"\x1b]11;?\x1b\\") {
        outcome.push_str(&format!("FAIL: write OSC11 query: {e}\n"));
        restore_termios(tty_fd, &saved);
        write_result(&outcome);
        std::process::exit(1);
    }
    let _ = tty.flush();

    let deadline = Instant::now() + Duration::from_secs(3);
    let buf = read_until_terminator(tty_fd, deadline);
    match parse_osc11_bg(&buf) {
        Some(rgb) if rgb == expected_bg => {
            outcome.push_str("OK\n");
        }
        Some(rgb) => {
            outcome.push_str(&format!(
                "FAIL: probe1 got rgb {},{},{} want {},{},{}\n",
                rgb.0, rgb.1, rgb.2, expected_bg.0, expected_bg.1, expected_bg.2
            ));
        }
        None => {
            outcome.push_str(&format!(
                "FAIL: probe1 no OSC 11 reply within deadline; got {} bytes: {:?}\n",
                buf.len(),
                String::from_utf8_lossy(&buf)
            ));
        }
    }

    // Write probe1 outcome immediately so the test driver can decide
    // whether to short-circuit before the toggle path runs.
    write_result(&outcome);

    // ---- Probe 2 (mid-session toggle) ----
    //
    // Mid-session: daemon's `Effect::TerminalThemeUpdate` writes the
    // updated OSC 10/11 reply pair to the PTY *unsolicited* (no
    // re-query needed from our side). We just listen for up to 10s
    // for the next OSC 11 reply with the new RGB.
    //
    // The 10s ceiling is the WS toggle round-trip budget: test sends
    // `TerminalThemeUpdate` → server handler intercepts + persists →
    // bridge forwards to daemon → daemon synthesizes OSC reply →
    // bytes hit our PTY input. In practice this takes ~50ms; 10s
    // exists to keep the test deterministic on a hot CI box and to
    // give us diagnostic budget when the chain is broken.
    if let Some(exp2) = expected_bg_2 {
        let deadline2 = Instant::now() + Duration::from_secs(10);
        let buf2 = read_until_terminator(tty_fd, deadline2);
        // Dump raw bytes seen to the trace file for diagnostics
        if let Ok(trace_path) = std::env::var("NEIGE_OSC_TRACE_PATH") {
            let _ = std::fs::OpenOptions::new()
                .append(true)
                .open(&trace_path)
                .and_then(|mut f| {
                    f.write_all(
                        format!("probe2 raw bytes ({}): {:?}\n", buf2.len(), buf2).as_bytes(),
                    )
                });
        }
        match parse_osc11_bg(&buf2) {
            Some(rgb) if rgb == exp2 => {
                outcome.push_str("OK2\n");
            }
            Some(rgb) => {
                outcome.push_str(&format!(
                    "FAIL: probe2 got rgb {},{},{} want {},{},{}\n",
                    rgb.0, rgb.1, rgb.2, exp2.0, exp2.1, exp2.2
                ));
            }
            None => {
                outcome.push_str(&format!(
                    "FAIL: probe2 no OSC 11 reply within deadline; got {} bytes: {:?}\n",
                    buf2.len(),
                    String::from_utf8_lossy(&buf2)
                ));
            }
        }
        write_result(&outcome);
    }

    restore_termios(tty_fd, &saved);

    // Exit code is informational only — the test driver reads the
    // result file. 0 on probe1-OK (probe2 may have failed; the
    // result file carries the full picture).
    if outcome.contains("FAIL") {
        std::process::exit(1);
    }
}
