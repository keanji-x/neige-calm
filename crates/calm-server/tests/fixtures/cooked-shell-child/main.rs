//! Test fixture (fix B, shell direction): a stand-in for an interactive
//! shell (`zsh` / `fish`) sitting at its prompt inside a
//! `calm-session-daemon` PTY.
//!
//! ## Why this fixture exists — complements `osc-probe-child`
//!
//! `osc-probe-child` models a **focus-aware TUI** (codex / claude-tui):
//! it enables DECSET 1004 (focus event reporting) and probes OSC 11.
//! `theme_osc_roundtrip.rs` uses it to prove the daemon's mid-session
//! `Effect::TerminalThemeUpdate` write still reaches a 1004-opted-in
//! consumer — i.e. that fix B does **not** over-gate.
//!
//! This fixture models the **opposite** case that fix B exists to
//! protect: an interactive shell at its prompt. The crucial, easily-
//! missed fact is that a modern shell at its prompt is **not** in cooked
//! mode — zsh's ZLE (and fish's reader) put the tty into a raw-mode line
//! editor (`ECHO` off, `ICANON` off), identical termios to a real TUI.
//! What it does NOT do is opt into DECSET 1004: it only enables
//! bracketed paste (`ESC[?2004h`) and never sends `ESC[?1004h`, never
//! queries OSC 10/11.
//!
//! So if the daemon writes anything mid-session — pre-#305 a synthetic
//! OSC 10/11 reply pair, post-#305 just `ESC[I` — the shell's ZLE
//! doesn't silently consume it like a TUI would; it treats the bytes as
//! *input* and redraws them at the prompt as (syntax-highlighted)
//! garbage. That is the OSC-echo bug. The original fix B gated on the
//! PTY `ECHO` flag, which is useless here precisely because ZLE turns
//! ECHO off — making the shell look like a TUI. The corrected fix B
//! gates on DECSET 1004 instead, which the shell never sets.
//!
//! ## What this fixture does — faithfully reproduces ZLE's raw mode
//!
//!   1. Enters **raw mode** (`cfmakeraw`: ECHO off, ICANON off) just
//!      like ZLE does at the prompt. This is the whole point — the old
//!      version of this fixture left the tty cooked, which gave a false
//!      sense of security: the old ECHO-based gate happened to catch the
//!      cooked case, but real shells are in raw mode at the prompt.
//!   2. Does **NOT** enable DECSET 1004 (no `ESC[?1004h`) and does
//!      **NOT** write any OSC query — a passive shell prompt that hasn't
//!      opted into focus events. This is exactly what the corrected fix B
//!      keys off: no 1004 → no mid-session daemon write.
//!   3. **Echoes whatever it reads on stdin straight back to stdout.**
//!      This is the load-bearing part. In raw mode the *kernel* line
//!      discipline no longer echoes (that was the cooked-mode behaviour
//!      the old ECHO gate relied on); instead the *application* redraws
//!      input — which is exactly what zsh's ZLE does when bytes arrive at
//!      the prompt (it re-renders them, often syntax-highlighted, as
//!      visible output). So if the daemon (wrongly) writes anything on
//!      theme update — post-#305 `ESC[I`; pre-#305 also OSC 10/11 RGB —
//!      the bytes hit our stdin and we write them back; the daemon's
//!      PTY reader sees them and broadcasts a `RenderPatch` carrying
//!      e.g. `[I` or `]10;rgb:…` — the garbage the user would see. The
//!      corrected fix B (gated on DECSET 1004, which we never set) means
//!      the daemon never writes anything for us to echo, so no such
//!      `RenderPatch` is ever produced. A fixture that merely *discarded*
//!      stdin would PASS even with the gate removed (raw mode has no
//!      kernel echo), giving false confidence — hence we echo.
//!
//! No env/argv parameters: there is nothing to configure. The test
//! driver asserts on the broadcast `RenderPatch` bytes, not on any
//! sidecar file this fixture writes.

#![cfg(unix)]

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;

/// Open `/dev/tty` (the controlling terminal). The PTY slave the daemon
/// spawned us against IS our controlling tty — portable_pty dups the
/// slave fd onto stdin/stdout/stderr and `setsid + ioctl TIOCSCTTY`s it
/// as the ctty. `/dev/tty` resolves to it; opening it is the canonical
/// way to access the terminal. (Mirrors `osc-probe-child::open_tty`.)
fn open_tty() -> std::io::Result<File> {
    OpenOptions::new().read(true).write(true).open("/dev/tty")
}

/// Switch the tty to raw mode (`cfmakeraw`: ECHO off, ICANON off),
/// faithfully reproducing what zsh's ZLE / fish's reader do at the
/// prompt. We deliberately do NOT restore — the daemon SIGKILLs us at
/// teardown, and the whole point is to stay in raw mode for the test's
/// duration so the corrected fix B can't fall back to the ECHO heuristic.
fn enter_raw(fd: i32) -> std::io::Result<()> {
    // SAFETY: zeroed termios is the documented "uninitialized" state for
    // tcgetattr to fill in.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::tcgetattr(fd, &mut t) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: cfmakeraw mutates termios in place — no aliasing.
    unsafe {
        libc::cfmakeraw(&mut t);
    }
    let rc = unsafe { libc::tcsetattr(fd, libc::TCSANOW, &t) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn main() {
    // Put the tty into raw mode — this is what a shell's line editor does
    // at the prompt. Crucially we do NOT enable DECSET 1004 and never
    // emit an OSC query, so the corrected fix B (gated on 1004) skips the
    // mid-session theme write for us (post-#305: `ESC[I`). If `/dev/tty`
    // is unavailable (rare — the daemon always gives us a ctty), fall
    // back to raw stdin: still no 1004, still passive, which is all the
    // test needs.
    if let Ok(tty) = open_tty() {
        let _ = enter_raw(tty.as_raw_fd());
        // Keep `tty` alive past the raw-mode switch by leaking it; we
        // never need to touch it again and don't want Drop closing the
        // fd. (Cheap: one fd for the process lifetime, reaped by the
        // daemon's SIGKILL at teardown.)
        std::mem::forget(tty);
    } else {
        let _ = enter_raw(std::io::stdin().as_raw_fd());
    }

    // Read stdin forever and echo every byte back to stdout — simulating
    // ZLE redrawing input at the prompt (see module doc, step 3). We
    // never exit on our own — the daemon SIGKILLs us at PTY teardown when
    // the test's tempdir/socket drops. Because we never enabled DECSET
    // 1004, the corrected fix B means the daemon never writes the
    // mid-session theme bytes (post-#305: `ESC[I`), so there's nothing
    // for us to echo and the broadcast `RenderPatch` stays free of those
    // literals — what the test asserts. (Remove the gate and the bytes
    // arrive here, get echoed, and surface in a `RenderPatch` — making
    // the test fail, as intended.)
    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut buf = [0u8; 256];
    loop {
        match stdin.read(&mut buf) {
            // EOF (read half closed): nothing more will arrive. Park on a
            // long sleep rather than busy-looping so we stay alive for the
            // daemon to reap us at teardown.
            Ok(0) => std::thread::sleep(std::time::Duration::from_secs(3600)),
            // Echo what we read straight back, like ZLE redrawing input.
            Ok(n) => {
                if stdout.write_all(&buf[..n]).is_err() {
                    break;
                }
                let _ = stdout.flush();
            }
            // Read error (e.g. the slave fd went away): exit cleanly.
            Err(_) => break,
        }
    }
}
