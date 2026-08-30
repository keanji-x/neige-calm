#!/usr/bin/env python3
"""Drive the REAL `tui-host-demo.py` through a REAL pty at a fixed width.

Why this exists (issue #1152): the demo fixture paints its footer through
`clip(footer, cols)`, so anything living at the tail of that footer is silently
chopped when the terminal card is narrow. CI rendered the card at exactly 60
columns and `TUI_HOST_DEMO_READY` lost its last character, which turned into a
15s `toContain` timeout in `tui-host-demo.spec.ts` — a flake, because the card's
measured width depends on font metrics / layout timing.

This harness deliberately does NOT re-implement `clip()` or the footer string.
It spawns the fixture on a real pty, reads whatever bytes the fixture actually
paints, and asserts the markers survive intact. A check that agreed with the
fixture's own arithmetic would prove nothing.

Two modes, because the fix had two halves:

  `<cols>`                 fixed width, set in the child before `execvp`. Guards
                           the "marker moved to the front of the footer" half.
  `resize:<from>:<to>`     start WIDE, let the fixture paint, then narrow the
                           pty from the PARENT with `TIOCSWINSZ` on the master
                           fd. Only this mode makes SIGWINCH fire at all, so it
                           is the only guard on the repaint half of the fix. In
                           the fixed mode the winsize is already correct before
                           the child starts, so deleting the fixture's
                           `signal.signal(signal.SIGWINCH, on_winch)` leaves it
                           green — half the fix would be unguarded.

Three properties are asserted, all read off the LAST COMPLETE REPAINT rather
than "somewhere in the byte stream". The fixture paints at least twice at
startup (`draw()` then `copy()` -> `draw()`), so a stream-wide search would go
green even if a later repaint lost the marker:

  1. `TUI_HOST_DEMO_READY` is present and whole.
  2. `COPIED=neige-osc52-ok` is present, at every width where it fits. The
     fixture calls `copy()` at startup, so it needs no input, and
     `tui-host-demo.spec.ts:143` asserts it too — it is the next width-dependent
     assertion in line to flake. It is checked twice, at its two real
     thresholds: anywhere in the grid from 22 columns (what the spec polls; the
     status row carries it), and in the FOOTER row from 42 columns (the row this
     fix rearranged). Below those it legitimately cannot fit and is NOT asserted
     — notably not at 20 columns.
  3. The child actually OBSERVED the width we set. `clip()` pads to width, so
     the footer row is exactly `cols` characters. Without this a silently
     no-op'd `TIOCSWINSZ` degrades to `pty.fork()`'s default `(0,0,0,0)`,
     `size()` floors that to 20 columns, and the check still passes while no
     longer testing the width it claims.

Run standalone:

    python3 web/e2e/fixtures/tui-host-demo-width-check.py       # all modes
    python3 web/e2e/fixtures/tui-host-demo-width-check.py 60    # one width
    python3 web/e2e/fixtures/tui-host-demo-width-check.py resize:100:60

Exit code 0 = every mode painted the markers whole, at the width it asked for.
"""
from __future__ import annotations

import fcntl
import os
import pty
import select
import signal
import struct
import sys
import termios
import time

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURE = os.path.join(HERE, "tui-host-demo.py")
READY_MARKER = b"TUI_HOST_DEMO_READY"
COPIED_MARKER = b"COPIED=neige-osc52-ok"
# `clip()` floors the terminal at 20 columns, so READY (19 chars) at column 1
# always fits. COPIED is painted in TWO rows and they clip at different widths,
# so they get separate thresholds — a single one would either under-assert or
# assert something that legitimately cannot fit:
#   status row  `clip(" COPIED=neige-osc52-ok", cols)`      -> 1 + 21 columns
#   footer row  `READY + "  " + COPIED + hints`, clipped    -> 19 + 2 + 21 columns
COPIED_STATUS_MIN_COLS = 1 + len(COPIED_MARKER)
COPIED_FOOTER_MIN_COLS = len(READY_MARKER) + 2 + len(COPIED_MARKER)
# Every `draw()` opens with cursor-home + erase-display, which makes this the
# exact frame boundary. The bytes after the LAST one are the final painted state.
REPAINT_START = b"\x1b[H\x1b[2J"
DEFAULT_MODES = ("20", "60", "100", "resize:100:60")
ROWS = 24
READ_SECONDS = 4.0
QUIET_SECONDS = 0.75


def spawn_at(cols: int, rows: int) -> tuple[int, int]:
    """Fork the fixture onto a pty already sized `cols`x`rows`.

    `pty.fork()` hands the child a controlling terminal (setsid + TIOCSCTTY),
    which the fixture needs: it reads `/dev/tty`, not stdin. Setting the winsize
    in the child before `execvp` removes the race a post-fork parent-side
    TIOCSWINSZ would have with the fixture's first `draw()` — and, deliberately,
    means SIGWINCH never fires in this mode. `resize:` mode covers that.
    """
    pid, master = pty.fork()
    if pid == 0:  # child
        try:
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
            env = dict(os.environ, TUI_HOST_DEMO_SECONDS="10")
            os.execvpe("python3", ["python3", FIXTURE], env)
        except BaseException:  # pragma: no cover - child can only bail out
            os._exit(127)
    return pid, master


def set_winsize(master: int, cols: int, rows: int) -> None:
    """Resize from the PARENT end, which is what makes the kernel raise SIGWINCH."""
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))


def drain(master: int, out: bytearray) -> bytearray:
    """Append pty output to `out` until it goes quiet (or the read budget ends).

    Reading to QUIESCENCE rather than stopping at the first marker byte is what
    makes "the last repaint" meaningful: the fixture writes each frame with one
    `write_all()`, so once no byte has arrived for QUIET_SECONDS the trailing
    frame in the buffer is a complete one.
    """
    start = time.monotonic()
    seen_any = len(out) > 0
    last = start
    while True:
        now = time.monotonic()
        if now - start > READ_SECONDS:
            break
        if seen_any and now - last > QUIET_SECONDS:
            break
        if not select.select([master], [], [], 0.05)[0]:
            continue
        try:
            chunk = os.read(master, 65536)
        except OSError:
            break
        if not chunk:
            break
        out.extend(chunk)
        seen_any = True
        last = time.monotonic()
    return out


def reap(pid: int, master: int) -> None:
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.kill(pid, sig)
        except ProcessLookupError:
            break
        for _ in range(50):
            if os.waitpid(pid, os.WNOHANG)[0] == pid:
                break
            time.sleep(0.02)
        else:
            continue
        break
    try:
        os.close(master)
    except OSError:
        pass


def tail(painted: bytes) -> str:
    """Last painted line-ish slice, for a readable failure message."""
    return repr(painted[-160:])


def last_repaint(painted: bytes) -> bytes | None:
    """Bytes of the final complete frame, or None if the fixture never painted one."""
    index = painted.rfind(REPAINT_START)
    if index < 0:
        return None
    return painted[index + len(REPAINT_START):]


def footer_row(repaint: bytes, rows: int) -> bytes | None:
    """The bottom row of the final frame — `draw()` writes it last, so it runs to the end."""
    cursor = b"\x1b[%d;1H" % rows
    index = repaint.rfind(cursor)
    if index < 0:
        return None
    return repaint[index + len(cursor):]


def truncation_reason(repaint: bytes) -> str:
    for cut in range(len(READY_MARKER) - 1, 3, -1):
        if READY_MARKER[:cut] in repaint:
            return (f"READY marker was truncated to {READY_MARKER[:cut].decode()!r} "
                    f"({cut}/{len(READY_MARKER)} chars survived clip(footer, cols))")
    return "no trace of the READY marker in the final repaint"


def failures_at(painted: bytes, cols: int, rows: int, label: str) -> list[str]:
    """Every way the FINAL painted frame disagrees with what `cols` promised."""
    if not painted:
        return [f"{label}: the fixture painted nothing at all"]
    repaint = last_repaint(painted)
    if repaint is None:
        return [f"{label}: the fixture never emitted a full repaint. painted tail: {tail(painted)}"]
    reasons = []
    if READY_MARKER not in repaint:
        reasons.append(f"{label}: {truncation_reason(repaint)}. painted tail: {tail(repaint)}")
    # What `tui-host-demo.spec.ts:143` polls for: the marker ANYWHERE in the grid dump.
    if cols >= COPIED_STATUS_MIN_COLS and COPIED_MARKER not in repaint:
        reasons.append(f"{label}: {COPIED_MARKER.decode()} is missing from the whole final repaint "
                       f"even though {cols} >= {COPIED_STATUS_MIN_COLS} columns fit it in the status "
                       f"row. tui-host-demo.spec.ts polls for it. painted tail: {tail(repaint)}")
    footer = footer_row(repaint, rows)
    if footer is None:
        reasons.append(f"{label}: could not find the footer row in the final repaint, so the width "
                       f"the child observed is unknown. painted tail: {tail(repaint)}")
        return reasons
    # And separately in the FOOTER, which is the row this fix rearranged. Without this the check
    # above passes off the status row and a footer that lost the marker stays green.
    if cols >= COPIED_FOOTER_MIN_COLS and COPIED_MARKER not in footer:
        reasons.append(f"{label}: {COPIED_MARKER.decode()} is missing from the FOOTER row even "
                       f"though {cols} >= {COPIED_FOOTER_MIN_COLS} columns fit it there. "
                       f"footer: {footer!r}")
    if len(footer) != cols:
        reasons.append(f"{label}: the child painted {len(footer)} columns, not {cols} — it never "
                       "observed the winsize this check set, so the width under test is not the "
                       "one claimed")
    return reasons


def check_width(cols: int) -> list[str]:
    """Fixed width, set before exec. No SIGWINCH is involved by construction."""
    pid, master = spawn_at(cols, ROWS)
    try:
        painted = drain(master, bytearray())
    finally:
        reap(pid, master)
    return failures_at(bytes(painted), cols, ROWS, f"{cols} cols")


def check_resize(start_cols: int, end_cols: int) -> list[str]:
    """Start wide, paint, then narrow from the parent — the SIGWINCH repaint path.

    The host does exactly this: `terminal.rs` opens the pty at 80 columns and the
    frontend resizes it to the card's measured width after attach. Without a
    SIGWINCH repaint the fixture's first paint (at whatever width it happened to
    start with) is the only one until input arrives.
    """
    label = f"resize {start_cols}->{end_cols} cols"
    pid, master = spawn_at(start_cols, ROWS)
    try:
        painted = drain(master, bytearray())
        initial = failures_at(bytes(painted), start_cols, ROWS, f"{label} (before resize)")
        if initial:
            return initial
        before = len(painted)
        set_winsize(master, end_cols, ROWS)
        drain(master, painted)
        if len(painted) == before:
            return [f"{label}: the fixture emitted NOTHING after the parent resized the pty, so it "
                    f"is still painted at {start_cols} columns. It is missing the SIGWINCH repaint "
                    "(signal.signal(signal.SIGWINCH, on_winch) + the RESIZED branch in run())."]
        return failures_at(bytes(painted), end_cols, ROWS, label)
    finally:
        reap(pid, master)


def run_mode(mode: str) -> list[str]:
    if mode.startswith("resize:"):
        parts = mode.split(":")
        if len(parts) != 3:
            raise SystemExit(f"bad mode {mode!r}: expected resize:<from-cols>:<to-cols>")
        return check_resize(int(parts[1]), int(parts[2]))
    return check_width(int(mode))


def main(argv: list[str]) -> int:
    modes = argv[1:] or list(DEFAULT_MODES)
    failed = 0
    for mode in modes:
        reasons = run_mode(mode)
        # stderr + flush: the Playwright wrapper inherits these fds, and a buffered
        # diagnostic that only lands at exit is a diagnostic nobody reads in CI.
        if not reasons:
            print(f"ok   {mode}: every marker wide enough to fit was painted in full, "
                  "at the width the child actually observed", file=sys.stdout, flush=True)
            continue
        failed += 1
        for reason in reasons:
            print(f"FAIL {reason}", file=sys.stderr, flush=True)
    if failed:
        print(
            f"{failed}/{len(modes)} mode(s) failed. web/e2e/tui-host-demo.spec.ts polls for these "
            "markers and will time out.",
            file=sys.stderr, flush=True,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
