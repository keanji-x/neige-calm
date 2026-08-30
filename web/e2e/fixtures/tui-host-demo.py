#!/usr/bin/env python3
"""Split-pane mouse TUI for 验收-ing Neige's xterm host.

Mirrors grok's conversation / content split:

  - DECSET 1049 alt screen
  - 1000 + 1002 + 1006 SGR mouse
  - wheel over a pane scrolls THAT pane
  - startup + `y` emit OSC 52 so the host clipboard can be checked

This is a fixture, not grok. It speaks the same mouse + clipboard wire
so a headed Playwright run (or a human in a terminal card) can see the
host actually forwarding those sequences.

Markers painted into the grid (and therefore visible to `__xtermDumps__`):

  TUI_HOST_DEMO_READY
  WHEEL pane=conversation|content
  COPIED=neige-osc52-ok
"""
from __future__ import annotations

import base64
import os
import select
import signal
import sys
import termios

COPY_MARKER = "neige-osc52-ok"
READY_MARKER = "TUI_HOST_DEMO_READY"
STOP = False
RESIZED = False
CONVERSATION = [f"conversation line {i}" for i in range(40)]
CONTENT = [f"content block {i}" for i in range(40)]


def flag(name: str) -> int:
    return getattr(termios, name, 0)


def make_raw(attrs: list) -> list:
    attrs = list(attrs)
    attrs[3] &= ~(
        flag("ICANON")
        | flag("ECHO")
        | flag("ECHOE")
        | flag("ECHOK")
        | flag("ECHONL")
        | flag("ISIG")
        | flag("IEXTEN")
    )
    attrs[0] &= ~(
        flag("IGNBRK")
        | flag("BRKINT")
        | flag("PARMRK")
        | flag("ISTRIP")
        | flag("INLCR")
        | flag("IGNCR")
        | flag("ICRNL")
        | flag("IXON")
    )
    attrs[1] &= ~flag("OPOST")
    attrs[2] = (attrs[2] & ~flag("CSIZE")) | flag("CS8")
    attrs[2] &= ~flag("PARENB")
    return attrs


def on_stop(_signum, _frame) -> None:
    global STOP
    STOP = True


def on_winch(_signum, _frame) -> None:
    """Flag ONLY — the repaint happens in run()'s select loop.

    Drawing from inside the handler would interleave with an in-flight
    write_all() and tear the escape sequences mid-stream.
    """
    global RESIZED
    RESIZED = True


def write_all(fd: int, data: bytes) -> None:
    while data:
        data = data[os.write(fd, data) :]


def size(fd: int) -> tuple[int, int]:
    try:
        cols, rows = os.get_terminal_size(fd)
    except OSError:
        cols, rows = 80, 24
    return max(cols, 20), max(rows, 8)


def osc52(fd: int, text: str) -> None:
    payload = base64.b64encode(text.encode("utf-8"))
    write_all(fd, b"\x1b]52;c;" + payload + b"\x07")


def enter_ui(fd: int) -> None:
    write_all(
        fd,
        b"\x1b[?1049h\x1b[?25l\x1b[?1000h\x1b[?1002h\x1b[?1006h",
    )


def leave_ui(fd: int) -> None:
    write_all(
        fd,
        b"\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?25h\x1b[?1049l",
    )


def clip(text: str, width: int) -> str:
    if width <= 0:
        return ""
    if len(text) <= width:
        return text + " " * (width - len(text))
    return text[:width]


class Demo:
    def __init__(self, fd: int) -> None:
        self.fd = fd
        self.conv_off = 0
        self.content_off = 0
        self.last = "waiting for wheel"
        self.copied = False

    def draw(self) -> None:
        cols, rows = size(self.fd)
        split = cols // 2
        left_w = max(split - 1, 8)
        right_w = max(cols - split - 1, 8)
        body_rows = max(rows - 3, 1)
        chunks = [b"\x1b[H\x1b[2J"]
        header = clip(" conversation", left_w) + "|" + clip(" content", right_w)
        chunks.append(b"\x1b[1;1H" + header.encode("ascii", "replace"))
        for i in range(body_rows):
            left = CONVERSATION[(self.conv_off + i) % len(CONVERSATION)]
            right = CONTENT[(self.content_off + i) % len(CONTENT)]
            line = clip(left, left_w) + "|" + clip(right, right_w)
            chunks.append(f"\x1b[{i + 2};1H".encode() + line.encode("ascii", "replace"))
        status = clip(f" {self.last}", cols)
        chunks.append(f"\x1b[{rows - 1};1H".encode() + status.encode("ascii", "replace"))
        # Markers FIRST, hints last: draw() clips the footer to the terminal
        # width and size() floors cols at 20, so a 19-char marker starting at
        # column 1 survives every width the host can hand us. With the marker at
        # the tail a 60-column card chopped it to TUI_HOST_DEMO_READ and the
        # e2e poll timed out (#1152). The hints are cosmetic and may clip.
        footer = READY_MARKER
        if self.copied:
            footer += "  COPIED=" + COPY_MARKER
        footer += "  y=copy  wheel=pane under cursor  q=quit"
        chunks.append(f"\x1b[{rows};1H".encode() + clip(footer, cols).encode("ascii", "replace"))
        write_all(self.fd, b"".join(chunks))

    def wheel(self, col: int, button: int) -> None:
        cols, _rows = size(self.fd)
        delta = -1 if button == 64 else 1
        pane = "conversation" if col < cols // 2 else "content"
        if pane == "conversation":
            self.conv_off = max(0, self.conv_off + delta)
        else:
            self.content_off = max(0, self.content_off + delta)
        self.last = f"WHEEL pane={pane} dy={delta} col={col}"
        self.draw()

    def copy(self) -> None:
        osc52(self.fd, COPY_MARKER)
        self.copied = True
        self.last = f"COPIED={COPY_MARKER}"
        self.draw()


def parse_sgr(buf: bytearray) -> list[tuple[int, int, int, str]]:
    events: list[tuple[int, int, int, str]] = []
    while True:
        start = buf.find(b"\x1b[<")
        if start < 0:
            break
        end_m = buf.find(b"M", start)
        end_r = buf.find(b"m", start)
        ends = [i for i in (end_m, end_r) if i >= 0]
        if not ends:
            break
        end = min(ends)
        payload = buf[start + 3 : end].decode("ascii", "replace")
        kind = chr(buf[end])
        del buf[: end + 1]
        parts = payload.split(";")
        if len(parts) != 3:
            continue
        try:
            button, col, row = (int(parts[0]), int(parts[1]), int(parts[2]))
        except ValueError:
            continue
        events.append((button, col, row, kind))
    if len(buf) > 64:
        del buf[:-16]
    return events


def run(fd: int) -> None:
    demo = Demo(fd)
    enter_ui(fd)
    demo.draw()
    demo.copy()
    pending = bytearray()
    global RESIZED
    while not STOP:
        if RESIZED:
            # The host resizes the pty after attach; without this the very first
            # paint (at whatever width we happened to start with) would be the
            # only one until input arrives.
            RESIZED = False
            demo.draw()
        if not select.select([fd], [], [], 0.25)[0]:
            continue
        chunk = os.read(fd, 4096)
        if not chunk:
            return
        pending.extend(chunk)
        for button, col, row, kind in parse_sgr(pending):
            if kind == "M" and button in (64, 65):
                demo.wheel(col, button)
            else:
                demo.last = f"MOUSE btn={button} col={col} row={row} {kind}"
                demo.draw()
        # Remaining bytes may include keys. Keep it tiny: q / Ctrl-C / y / ESC CR.
        text = bytes(pending)
        if b"q" in text or b"\x03" in text:
            return
        if b"y" in text:
            demo.copy()
            pending.clear()
        elif b"\x1b\r" in text:
            demo.last = "NEWLINE shift-enter"
            demo.draw()
            pending.clear()


def main() -> int:
    for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGALRM):
        signal.signal(sig, on_stop)
    signal.signal(signal.SIGWINCH, on_winch)
    signal.alarm(int(os.environ.get("TUI_HOST_DEMO_SECONDS", "120")))
    try:
        fd = os.open("/dev/tty", os.O_RDWR)
    except OSError:
        return 0
    saved = None
    try:
        saved = termios.tcgetattr(fd)
        termios.tcsetattr(fd, termios.TCSANOW, make_raw(saved))
        run(fd)
    except (EOFError, OSError, termios.error):
        pass
    finally:
        try:
            leave_ui(fd)
        except OSError:
            pass
        try:
            if saved is not None:
                termios.tcsetattr(fd, termios.TCSANOW, saved)
        except termios.error:
            pass
        os.close(fd)
    return 0


if __name__ == "__main__":
    sys.exit(main())
