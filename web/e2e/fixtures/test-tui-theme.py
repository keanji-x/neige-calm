#!/usr/bin/env python3
"""Deterministic TUI fixture exercising the neige-calm host -> daemon
OSC 10/11 color contract (issue #545).

This is NOT a codex / claude / opencode replica. It speaks just enough of
the same OSC 10/11 + DECSET 1004 wire that real codex (the only one with
an open-source TUI: external/codex/codex-rs/tui/src/terminal_probe.rs)
uses, so the daemon's reply path is exercised end-to-end. Specific
choices that mirror codex:

  - one batched write of `]10;?ST]11;?ST` (codex terminal_probe.rs:228)
  - DECSET 1004 focus events; re-probe on `ESC[I` (codex
    terminal_palette.rs requery_default_colors)
  - order-independent buffer scan for OSC 10 + OSC 11 prefixes
    (codex parse_default_colors lines 826-830)
  - 4-hex per channel divided by 257 to recover u8 (codex
    parse_osc_component lines 861-867)

claude-code is closed source; opencode is Ink/React in Node and does no
OSC probing of its own (uses host terminal colors directly). So codex is
the only meaningful upstream comparison.

Out of scope vs. real codex: cursor-position (`ESC[6n`), kitty keyboard
flags (`ESC[?u`), primary device attributes (`ESC[c`). The daemon's
handling of those is anchored elsewhere (or doesn't need to be - they're
no-ops on neige's renderer).
"""
import os, re, select, signal, sys, termios
STOP, FOCUS_IN = False, b"\x1b[I"
OSC = {
    10: re.compile(br"\x1b\]10;rgb:([0-9a-fA-F]{4})/([0-9a-fA-F]{4})/([0-9a-fA-F]{4})(?:\x07|\x1b\\)"),
    11: re.compile(br"\x1b\]11;rgb:([0-9a-fA-F]{4})/([0-9a-fA-F]{4})/([0-9a-fA-F]{4})(?:\x07|\x1b\\)"),
}
def flag(name):
    return getattr(termios, name, 0)
def make_raw(attrs):
    attrs = list(attrs)
    attrs[3] &= ~(flag("ICANON") | flag("ECHO") | flag("ECHOE") | flag("ECHOK") | flag("ECHONL") | flag("ISIG") | flag("IEXTEN"))
    attrs[0] &= ~(flag("IGNBRK") | flag("BRKINT") | flag("PARMRK") | flag("ISTRIP") | flag("INLCR") | flag("IGNCR") | flag("ICRNL") | flag("IXON"))
    attrs[1] &= ~flag("OPOST")
    attrs[2] = (attrs[2] & ~flag("CSIZE")) | flag("CS8")
    attrs[2] &= ~flag("PARENB")
    return attrs
def on_stop(_signum, _frame):
    global STOP
    STOP = True
def write_all(fd, data):
    while data:
        data = data[os.write(fd, data):]

def parse_default_colors(buffer):
    fg = OSC[10].search(buffer)
    bg = OSC[11].search(buffer)
    if not (fg and bg):
        return None

    def to_hex(match):
        rgb = [int(part, 16) // 257 for part in match.groups()]
        return "#{:02x}{:02x}{:02x}".format(*rgb)

    return (to_hex(fg), to_hex(bg))

def emit(fd, text):
    write_all(fd, text.encode())
def emit_error(fd, marker, raw=None):
    emit(fd, f"THEME_FG=#{marker}\r\n")
    if raw is not None:
        emit(fd, f"RAW={raw.hex()}\r\n")
def probe(fd, pending):
    write_all(fd, b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\")
    deadline_ticks = 100
    while deadline_ticks > 0 and not STOP:
        parsed = parse_default_colors(bytes(pending))
        if parsed is not None:
            fg, bg = parsed
            emit(fd, f"THEME_FG={fg}\r\nTHEME_BG={bg}\r\n")
            pending.clear()
            return
        if select.select([fd], [], [], 0.05)[0]:
            chunk = os.read(fd, 4096)
            if not chunk:
                raise EOFError
            pending.extend(chunk)
        deadline_ticks -= 1
    emit_error(fd, "TIMEOUT")
def drain(fd):
    while not STOP and select.select([fd], [], [], 0)[0]:
        if not os.read(fd, 4096):
            raise EOFError
def run(fd):
    pending = bytearray()
    write_all(fd, b"\x1b[?1004h")
    probe(fd, pending)
    seen = b""
    while not STOP:
        if not select.select([fd], [], [], 1.0)[0]:
            continue
        chunk = os.read(fd, 4096)
        if not chunk:
            return
        seen = (seen + chunk)[-32:]
        if FOCUS_IN in seen:
            drain(fd)
            pending.clear()
            seen = b""
            probe(fd, pending)
def main():
    for sig in (signal.SIGTERM, signal.SIGALRM):
        signal.signal(sig, on_stop)
    signal.alarm(60)
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
            if saved is not None:
                termios.tcsetattr(fd, termios.TCSANOW, saved)
        except termios.error:
            pass
        os.close(fd)
    return 0
if __name__ == "__main__":
    sys.exit(main())
