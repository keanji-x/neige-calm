#!/usr/bin/env python3
"""Drive the REAL `tui-host-demo.py` through a REAL pty at a fixed width.

Why this exists (issue #1152): the demo fixture paints its footer through
`clip(footer, cols)`, so anything living at the tail of that footer is silently
chopped when the terminal card is narrow. CI rendered the card at exactly 60
columns and `TUI_HOST_DEMO_READY` lost its last character, which turned into a
15s `toContain` timeout in `tui-host-demo.spec.ts` — a flake, because the card's
measured width depends on font metrics / layout timing.

This harness deliberately does NOT re-implement `clip()` or the footer string.
It spawns the fixture on a pty whose winsize is set BEFORE exec, reads whatever
bytes the fixture actually paints, and asserts the marker survives intact. A
check that agreed with the fixture's own arithmetic would prove nothing.

Run standalone:

    python3 web/e2e/fixtures/tui-host-demo-width-check.py          # 60 and 100
    python3 web/e2e/fixtures/tui-host-demo-width-check.py 60       # one width

Exit code 0 = the marker is whole at every width checked.
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
DEFAULT_WIDTHS = (60, 100)
ROWS = 24
READ_SECONDS = 3.0


def spawn_at(cols: int, rows: int) -> tuple[int, int]:
    """Fork the fixture onto a pty already sized `cols`x`rows`.

    `pty.fork()` hands the child a controlling terminal (setsid + TIOCSCTTY),
    which the fixture needs: it reads `/dev/tty`, not stdin. Setting the winsize
    in the child before `execvp` removes the race a post-fork parent-side
    TIOCSWINSZ would have with the fixture's first `draw()`.
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


def read_for(master: int, seconds: float) -> bytes:
    """Drain the pty for at most `seconds`, stopping early once READY shows up."""
    deadline = time.monotonic() + seconds
    out = bytearray()
    while time.monotonic() < deadline:
        remaining = max(0.0, deadline - time.monotonic())
        if not select.select([master], [], [], min(0.2, remaining))[0]:
            continue
        try:
            chunk = os.read(master, 65536)
        except OSError:
            break
        if not chunk:
            break
        out.extend(chunk)
        if READY_MARKER in out:
            break
    return bytes(out)


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


def check_width(cols: int) -> str | None:
    """Return None when the marker survives, else a human-readable reason."""
    pid, master = spawn_at(cols, ROWS)
    try:
        painted = read_for(master, READ_SECONDS)
    finally:
        reap(pid, master)
    if READY_MARKER in painted:
        return None
    if not painted:
        return f"{cols} cols: the fixture painted nothing at all"
    for cut in range(len(READY_MARKER) - 1, 3, -1):
        if READY_MARKER[:cut] in painted:
            truncated = READY_MARKER[:cut].decode()
            return (
                f"{cols} cols: READY marker was truncated to {truncated!r} "
                f"({cut}/{len(READY_MARKER)} chars survived clip(footer, cols)). "
                f"painted tail: {tail(painted)}"
            )
    return f"{cols} cols: no trace of the READY marker. painted tail: {tail(painted)}"


def main(argv: list[str]) -> int:
    widths = [int(value) for value in argv[1:]] or list(DEFAULT_WIDTHS)
    failures = []
    for cols in widths:
        reason = check_width(cols)
        # stderr + flush: the Playwright wrapper inherits these fds, and a buffered
        # diagnostic that only lands at exit is a diagnostic nobody reads in CI.
        stream = sys.stdout if reason is None else sys.stderr
        print(f"ok   {cols} cols: TUI_HOST_DEMO_READY painted in full" if reason is None
              else f"FAIL {reason}", file=stream, flush=True)
        if reason is not None:
            failures.append(reason)
    if failures:
        print(
            f"{len(failures)}/{len(widths)} width(s) lost the READY marker. "
            "web/e2e/tui-host-demo.spec.ts polls for it and will time out.",
            file=sys.stderr, flush=True,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
