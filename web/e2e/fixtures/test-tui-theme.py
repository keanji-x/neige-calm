#!/usr/bin/env python3
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
def end_at(buf):
    ends = []
    bel = buf.find(b"\x07")
    st = buf.find(b"\x1b\\")
    if bel >= 0:
        ends.append((bel, bel + 1))
    if st >= 0:
        ends.append((st, st + 2))
    return min(ends)[1] if ends else None
def read_frame(fd, pending):
    for _ in range(100):
        end = end_at(pending)
        if end is not None:
            frame = bytes(pending[:end])
            del pending[:end]
            return frame
        if STOP:
            return None
        if select.select([fd], [], [], 0.05)[0]:
            chunk = os.read(fd, 4096)
            if not chunk:
                raise EOFError
            pending.extend(chunk)
    return None
def parse_color(kind, frame):
    match = OSC[kind].search(frame)
    if not match:
        return None
    rgb = [int(part, 16) // 257 for part in match.groups()]
    return "#{:02x}{:02x}{:02x}".format(*rgb)
def emit(fd, text):
    write_all(fd, text.encode())
def emit_error(fd, marker, raw=None):
    emit(fd, f"THEME_FG=#{marker}\r\n")
    if raw is not None:
        emit(fd, f"RAW={raw.hex()}\r\n")
def probe(fd, pending):
    write_all(fd, b"\x1b]10;?\x1b\\")
    write_all(fd, b"\x1b]11;?\x1b\\")
    values = []
    for kind in (10, 11):
        frame = read_frame(fd, pending)
        if frame is None:
            emit_error(fd, "TIMEOUT")
            return
        color = parse_color(kind, frame)
        if color is None:
            emit_error(fd, "PARSE_ERR", frame)
            return
        values.append(color)
    emit(fd, f"THEME_FG={values[0]}\r\nTHEME_BG={values[1]}\r\n")
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
