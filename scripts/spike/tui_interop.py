#!/usr/bin/env python3
"""Real-TUI interop test: a programmatic WS client starts a thread, then the real
`codex resume <threadId> --remote unix://PATH` TUI is driven in a PTY on the same thread."""
import asyncio
import json
import os
import pty
import re
import select
import signal
import sys
import time

from websockets.asyncio.client import unix_connect

SOCK = sys.argv[1]
URI = "ws://localhost/"


class C:
    def __init__(self, tag):
        self.tag = tag
        self.ws = None
        self.id = 0
        self.rx = []

    async def connect(self):
        self.ws = await unix_connect(SOCK, URI, compression=None)
        asyncio.create_task(self._rd())

    async def _rd(self):
        try:
            async for r in self.ws:
                self.rx.append(json.loads(r))
        except Exception:
            pass

    async def req(self, m, p=None):
        self.id += 1
        msg = {"jsonrpc": "2.0", "id": self.id, "method": m}
        if p is not None:
            msg["params"] = p
        await self.ws.send(json.dumps(msg))
        return self.id

    async def resp(self, rid, t=20):
        d = time.time() + t
        while time.time() < d:
            for o in self.rx:
                if o.get("id") == rid and ("result" in o or "error" in o):
                    return o
            await asyncio.sleep(0.03)
        return None

    async def wait(self, m, t=120, since=0):
        d = time.time() + t
        while time.time() < d:
            for o in self.rx[since:]:
                if o.get("method") == m:
                    return o
            await asyncio.sleep(0.03)
        return None

    def methods(self, since=0):
        return [o.get("method") for o in self.rx[since:] if o.get("method")]


async def main():
    A = C("A")
    await A.connect()
    rid = await A.req("initialize", {
        "clientInfo": {"name": "A", "version": "0"},
        "capabilities": {"experimentalApi": True}})
    await A.resp(rid)
    rid = await A.req("thread/start", {})
    st = await A.resp(rid)
    tid = st["result"]["thread"]["id"]
    print(f"[A] thread {tid}")

    # turn 1 to flush a rollout
    since = len(A.rx)
    rid = await A.req("turn/start", {"threadId": tid,
                                     "input": [{"type": "text",
                                                "text": "Reply with OK."}]})
    await A.resp(rid)
    await A.wait("turn/completed", t=90, since=since)
    print("[A] rollout flushed (turn 1 complete)")

    a_before = len(A.rx)
    env = dict(os.environ)
    env["HTTP_PROXY"] = "http://127.0.0.1:2080"
    env["HTTPS_PROXY"] = "http://127.0.0.1:2080"
    env["TERM"] = "xterm-256color"
    pid, fd = pty.fork()
    if pid == 0:
        os.execvpe("codex", ["codex", "resume", tid,
                             "--remote", f"unix://{SOCK}"], env)
        os._exit(1)

    def drain(seconds):
        buf = b""
        t = time.time()
        while time.time() - t < seconds:
            r, _, _ = select.select([fd], [], [], 0.2)
            if r:
                try:
                    d = os.read(fd, 65536)
                except OSError:
                    break
                if not d:
                    break
                buf += d
        return buf

    boot = drain(5)
    os.write(fd, b"Reply with the single word FOUR.")
    time.sleep(0.5)
    os.write(fd, b"\r")
    drain(6)

    new = A.methods(a_before)
    print(f"[A] methods after TUI attached + typed: {new}")
    comp = await A.wait("turn/completed", t=40, since=a_before)
    print(f"[A] observed a turn/completed from TUI-driven turn: {comp is not None}")
    got_items = [o for o in A.rx[a_before:]
                 if o.get("method") in ("turn/started", "item/started",
                                        "item/completed")]
    print(f"[A] observed {len(got_items)} turn/item events after TUI attach")

    txt = re.sub(r'\x1b\[[0-9;?]*[A-Za-z]', '', boot.decode("utf-8", "replace"))
    txt = re.sub(r'[\x00-\x08\x0b-\x1f\x7f]', '', txt)
    print("--- TUI boot (first 600 cleaned chars) ---")
    print(txt[:600])

    try:
        os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    await A.ws.close()


asyncio.run(main())
