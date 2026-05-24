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
//! 2. Enable DECSET 1004 (`\x1b[?1004h`) — focus event reporting. The
//!    real codex opts in on startup, and the daemon gates the
//!    mid-session `ESC[I` theme nudge on this flag (only a focus-aware
//!    TUI receives it; a shell's raw-mode line editor, which never
//!    enables 1004, would surface it as a stray byte). Sending this
//!    here keeps the fixture faithful to codex so the mid-session
//!    toggle in `--probe-twice` mode still reaches us.
//! 3. Write `\x1b]11;?\x1b\\` to stdout. The daemon's `TerminalModel`
//!    parses this via vte and synthesizes `\x1b]11;rgb:RRRR/GGGG/BBBB\x1b\\`
//!    onto the PTY master.
//! 4. Read from stdin with `poll(2)` until we see an OSC `ST` (`\x1b\\`)
//!    or `BEL` (`\x07`) terminator, or a bounded timeout fires.
//! 5. Parse `\x1b]11;rgb:RRRR/GGGG/BBBB` — xterm convention is 16-bit
//!    per channel (4 hex nibbles), so divide by 257 (≈ 0xffff/0xff) to
//!    recover the original u8.
//! 6. Compare against `--expected-bg`. Write `OK` to `--result` on
//!    match, `FAIL: ...` otherwise.
//!
//! `--probe-twice` mode adds a second read after a configurable wait.
//! Behaviour depends on `NEIGE_OSC_REPROBE`:
//!
//! - **Default (no reprobe)** — Daemon used to write an unsolicited
//!   OSC 10/11 pair on `TerminalThemeUpdate`. Since #295 followup 1 it
//!   writes only a focus-in `ESC[I`. Probe2 reads raw bytes for a
//!   fixed window, then dumps them into the result file and the trace
//!   file. The test driver asserts on those raw bytes (e.g. contains
//!   `\x1b[I`, does NOT contain `\x1b]10;rgb:`).
//!
//! - **`NEIGE_OSC_REPROBE=1`** — Probe2 waits for the focus-in
//!   `ESC[I` the daemon writes after `TerminalThemeUpdate`, then
//!   actively re-queries `\x1b]11;?\x1b\\` (mirroring codex's
//!   `terminal_palette::requery_default_colors` on `FocusGained`).
//!   The reply RGB is compared against `NEIGE_OSC_EXPECTED_BG_2`.
//!   This exercises the post-#295 solicited-only path end-to-end.
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

/// Read bytes from `fd` into `buf` until a needle is seen or the
/// deadline elapses. Uses `poll(2)` for a bounded wait between reads
/// — `read(2)` on a raw-mode PTY would block forever if the daemon
/// never emits the reply.
///
/// Returns `(bytes_read, found)` where `found` is true if `needle`
/// appeared somewhere in the buffer.
fn read_until(fd: i32, deadline: Instant, needle: &[u8]) -> (Vec<u8>, bool) {
    let mut out: Vec<u8> = Vec::with_capacity(64);
    loop {
        let now = Instant::now();
        if now >= deadline {
            return (out, false);
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
            return (out, false);
        }
        if pfd.revents & libc::POLLIN == 0 {
            return (out, false);
        }

        let mut chunk = [0u8; 256];
        // SAFETY: chunk is a valid writable buffer of len 256.
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr() as *mut _, chunk.len()) };
        if n <= 0 {
            return (out, false);
        }
        out.extend_from_slice(&chunk[..n as usize]);

        if needle.is_empty() {
            continue;
        }
        if out.windows(needle.len()).any(|w| w == needle) {
            return (out, true);
        }
    }
}

/// Drain whatever bytes arrive on `fd` until the deadline elapses.
/// Unlike `read_until`, this never returns early on a terminator —
/// it keeps polling for the full window so the caller observes the
/// **complete** unsolicited write stream from the daemon. Used by
/// the "assert no OSC RGB bytes in unsolicited stream" probe2 path.
///
/// We do NOT bail on `poll` timeout: the daemon's write may arrive
/// hundreds of ms after probe2 begins (it has to round-trip through
/// the WS handler + session-state machine), so an early-return on a
/// quiet poll would miss it. Only `read(2)` errors / EOF (`n <= 0`)
/// terminate the loop early.
fn drain_for(fd: i32, deadline: Instant) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(64);
    loop {
        let now = Instant::now();
        if now >= deadline {
            return out;
        }
        let remaining = deadline - now;
        // Cap each poll wait at 100ms so we re-check the deadline
        // promptly. Without this an arbitrarily-large `remaining`
        // could mask a missed wake-up; with it the worst-case excess
        // is one poll cycle (~100ms) past the deadline.
        let ms = remaining.as_millis().min(100) as i32;

        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: pfd is a valid pollfd pointer with nfds=1.
        let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
        if rc < 0 {
            // EINTR is recoverable (a signal interrupted poll). Any
            // other error means we can't reliably wait — bail.
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return out;
        }
        if rc == 0 {
            // Poll timeout — no data yet. Keep waiting; the daemon's
            // toggle round-trip can be slow.
            continue;
        }
        if pfd.revents & (libc::POLLIN | libc::POLLHUP) == 0 {
            // POLLERR / POLLNVAL — terminal state, bail.
            return out;
        }
        if pfd.revents & libc::POLLIN == 0 {
            // POLLHUP only (no data) — give one final read a chance
            // (drain any FIFO residue), then bail.
            let mut chunk = [0u8; 256];
            // SAFETY: chunk is a valid writable buffer of len 256.
            let n = unsafe { libc::read(fd, chunk.as_mut_ptr() as *mut _, chunk.len()) };
            if n > 0 {
                out.extend_from_slice(&chunk[..n as usize]);
            }
            return out;
        }

        let mut chunk = [0u8; 256];
        // SAFETY: chunk is a valid writable buffer of len 256.
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr() as *mut _, chunk.len()) };
        if n <= 0 {
            return out;
        }
        out.extend_from_slice(&chunk[..n as usize]);
    }
}

/// Read bytes from `fd` into `buf` until an OSC terminator (`ESC\` or
/// `BEL`) is seen or the deadline elapses. Thin wrapper kept for
/// historical callers — probe1 uses this to receive a single complete
/// OSC 11 reply.
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
    let reprobe = flag_or_env("--reprobe", "NEIGE_OSC_REPROBE");
    // `expected-bg-2` is required only in reprobe mode; default
    // probe2 dumps raw bytes for the driver and ignores the value.
    let expected_bg_2 = if probe_twice {
        match arg_or_env("--expected-bg-2", "NEIGE_OSC_EXPECTED_BG_2") {
            Some(s) => Some(parse_rgb(&s).expect("expected-bg-2 parse")),
            None if reprobe => {
                panic!("--expected-bg-2 or NEIGE_OSC_EXPECTED_BG_2 required in reprobe mode")
            }
            None => Some((0, 0, 0)), // unused in default mode
        }
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
                "fixture-startup argv={:?} probe_twice={} reprobe={} expected_bg={:?} expected_bg_2={:?}\n",
                std::env::args().collect::<Vec<_>>(),
                probe_twice,
                reprobe,
                expected_bg,
                expected_bg_2,
            ),
        );
    }

    // Enable DECSET 1004 (focus event reporting) up front, before any
    // query and well before the mid-session toggle. The real codex opts
    // in on startup; the daemon gates the mid-session focus-in
    // `ESC[I` write on this flag, so a faithful prober must set it or
    // it would never see probe 2's signal. (In default probe2 mode we
    // observe ESC[I directly; in reprobe mode we use it as the trigger
    // to re-query OSC 11. The focus-in is harmless filler in our
    // read buffer; `parse_osc11_bg` scans for the OSC opener regardless.)
    if let Err(e) = tty.write_all(b"\x1b[?1004h") {
        outcome.push_str(&format!("FAIL: write DECSET 1004: {e}\n"));
        restore_termios(tty_fd, &saved);
        write_result(&outcome);
        std::process::exit(1);
    }
    let _ = tty.flush();

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
    // Two modes, selected by `NEIGE_OSC_REPROBE`:
    //
    // - Default: passively drain bytes the daemon writes after the
    //   toggle. Since #295 followup 1 the daemon writes only the
    //   focus-in CSI (`ESC[I`) — no unsolicited OSC 10/11 RGB. The
    //   raw byte stream is dumped to the trace + result file so the
    //   test driver can assert on its contents.
    //
    // - Reprobe (`NEIGE_OSC_REPROBE=1`): wait for `ESC[I`, then
    //   actively re-query `OSC 11;?`, mirroring codex's
    //   `terminal_palette::requery_default_colors` on `FocusGained`.
    //   Parse the reply and compare against `NEIGE_OSC_EXPECTED_BG_2`.
    //   This exercises the full solicited path end-to-end.
    //
    // The 10s outer ceiling is the WS toggle round-trip budget: test
    // sends `TerminalThemeUpdate` → server handler intercepts +
    // persists → bridge forwards to daemon → daemon writes ESC[I →
    // bytes hit our PTY input. In practice this takes ~50ms; 10s
    // exists to keep the test deterministic on a hot CI box and to
    // give us diagnostic budget when the chain is broken.
    if let Some(exp2) = expected_bg_2 {
        let deadline2 = Instant::now() + Duration::from_secs(10);

        if reprobe {
            // Wait for the focus-in CSI; once it arrives, re-query
            // OSC 11 and parse the reply.
            let (focus_buf, saw_focus_in) = read_until(tty_fd, deadline2, b"\x1b[I");
            if let Ok(trace_path) = std::env::var("NEIGE_OSC_TRACE_PATH") {
                let _ = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&trace_path)
                    .and_then(|mut f| {
                        f.write_all(
                            format!(
                                "probe2 reprobe focus-in wait ({}): saw_focus_in={} bytes={:?}\n",
                                focus_buf.len(),
                                saw_focus_in,
                                focus_buf
                            )
                            .as_bytes(),
                        )
                    });
            }
            if !saw_focus_in {
                outcome.push_str(&format!(
                    "FAIL: probe2 never saw focus-in ESC[I within deadline; \
                     got {} bytes: {:?}\n",
                    focus_buf.len(),
                    String::from_utf8_lossy(&focus_buf),
                ));
                write_result(&outcome);
                restore_termios(tty_fd, &saved);
                std::process::exit(1);
            }
            // Re-query OSC 11. Daemon's vte parser sees this and
            // synthesizes a reply from the (now-updated)
            // `default_bg`. Echo of the OSC 11 query bytes is
            // suppressed at the daemon's vte layer — only the reply
            // comes back.
            if let Err(e) = tty.write_all(b"\x1b]11;?\x1b\\") {
                outcome.push_str(&format!("FAIL: probe2 reprobe write OSC11: {e}\n"));
                write_result(&outcome);
                restore_termios(tty_fd, &saved);
                std::process::exit(1);
            }
            let _ = tty.flush();
            let reply_deadline = Instant::now() + Duration::from_secs(5);
            let reply_buf = read_until_terminator(tty_fd, reply_deadline);
            if let Ok(trace_path) = std::env::var("NEIGE_OSC_TRACE_PATH") {
                let _ = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&trace_path)
                    .and_then(|mut f| {
                        f.write_all(
                            format!(
                                "probe2 reprobe reply bytes ({}): {:?}\n",
                                reply_buf.len(),
                                reply_buf
                            )
                            .as_bytes(),
                        )
                    });
            }
            match parse_osc11_bg(&reply_buf) {
                Some(rgb) if rgb == exp2 => {
                    outcome.push_str("OK2\n");
                }
                Some(rgb) => {
                    outcome.push_str(&format!(
                        "FAIL: probe2 reprobe got rgb {},{},{} want {},{},{}\n",
                        rgb.0, rgb.1, rgb.2, exp2.0, exp2.1, exp2.2
                    ));
                }
                None => {
                    outcome.push_str(&format!(
                        "FAIL: probe2 reprobe no OSC 11 reply within deadline; \
                         got {} bytes: {:?}\n",
                        reply_buf.len(),
                        String::from_utf8_lossy(&reply_buf)
                    ));
                }
            }
        } else {
            // Default mode: passively collect raw bytes the daemon
            // writes after the toggle. We use a two-stage approach:
            //   1. `read_until(needle=ESC[I)` with the full deadline.
            //      As soon as the focus-in arrives we know the toggle
            //      reached the daemon; we then move on without
            //      blocking for the rest of the window.
            //   2. A short tail-drain to catch any trailing bytes
            //      that arrive in the same syscall window (e.g. if a
            //      regression re-introduces unsolicited OSC RGB
            //      before/after the focus-in, we want to see them too).
            //
            // Why not a single `drain_for(deadline2)`: when the daemon
            // is well-behaved its post-toggle write is a single 3-byte
            // ESC[I, after which it falls quiet. Waiting passively for
            // 10 wall-clock seconds for ANY signal makes the test
            // brittle (and adds 10s to every run); driving the window
            // closed on the expected needle is both faster and tighter.
            let drain_start = Instant::now();
            let (mut buf2, saw_focus_in) = read_until(tty_fd, deadline2, b"\x1b[I");
            // Tail-drain: 200ms after seeing the needle (or end of
            // deadline2 if we never saw it). Anything that arrives
            // here is suspicious and we want it in the dump.
            let tail_deadline = if saw_focus_in {
                Instant::now() + Duration::from_millis(200)
            } else {
                deadline2
            };
            let tail = drain_for(tty_fd, tail_deadline);
            buf2.extend_from_slice(&tail);
            let drain_elapsed = drain_start.elapsed();
            if let Ok(trace_path) = std::env::var("NEIGE_OSC_TRACE_PATH") {
                let _ = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&trace_path)
                    .and_then(|mut f| {
                        f.write_all(
                            format!(
                                "probe2 raw bytes ({}) saw_focus_in={} elapsed={:?}: {:?}\n",
                                buf2.len(),
                                saw_focus_in,
                                drain_elapsed,
                                buf2
                            )
                            .as_bytes(),
                        )
                    });
            }
            // Encode the raw bytes in a single result line as a hex
            // dump so the driver can decode them without needing to
            // share the trace file path.
            let mut hex = String::with_capacity(buf2.len() * 2);
            for b in &buf2 {
                use std::fmt::Write;
                let _ = write!(hex, "{:02x}", b);
            }
            outcome.push_str(&format!("PROBE2_BYTES_HEX={hex}\n"));
            // `exp2` is unused in default mode but mandatory in the
            // CLI for parity with reprobe mode (so the same env shape
            // works regardless).
            let _ = exp2;
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
