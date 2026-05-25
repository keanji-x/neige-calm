#!/usr/bin/env python3
"""#293 linchpin test: do two clients on the SAME app-server unix socket
observe each other's actions on a shared thread?

Sequence:
  1. A: initialize + thread/start -> threadId
  2. A: turn/start ("OK") and wait for turn/completed (flushes a rollout file)
  3. B: initialize + thread/resume {threadId}  (rollout now exists on disk)
  4. A: turn/start a SECOND turn. Question: does B observe A's turn events?
  5. B: turn/start a turn. Question: does A observe B's turn events?

This directly answers: is the thread shared/observable across connections,
or is each connection an isolated view?
"""
import asyncio
import json
import sys
import time

from websockets.asyncio.client import unix_connect

URI = "ws://localhost/"


class C:
    def __init__(self, tag, sock):
        self.tag = tag
        self.sock = sock
        self.ws = None
        self.id = 0
        self.rx = []

    async def connect(self):
        self.ws = await unix_connect(self.sock, URI, compression=None)
        asyncio.create_task(self._rd())

    async def _rd(self):
        try:
            async for r in self.ws:
                o = json.loads(r)
                self.rx.append(o)
                m = o.get("method")
                if m and not m.startswith(("remoteControl", "mcpServer",
                                           "account/")):
                    print(f"[{self.tag}] EVENT {m}", flush=True)
        except Exception:
            pass

    async def req(self, method, params=None):
        self.id += 1
        msg = {"jsonrpc": "2.0", "id": self.id, "method": method}
        if params is not None:
            msg["params"] = params
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

    async def wait(self, method, t=120, since=0):
        d = time.time() + t
        while time.time() < d:
            for o in self.rx[since:]:
                if o.get("method") == method:
                    return o
            await asyncio.sleep(0.03)
        return None

    def methods(self, since=0):
        return [o.get("method") for o in self.rx[since:]
                if o.get("method")
                and not o.get("method").startswith(
                    ("remoteControl", "mcpServer", "account/"))]


async def init(c):
    rid = await c.req("initialize", {
        "clientInfo": {"name": f"spike-{c.tag}", "version": "0.0.1"},
        "capabilities": {"experimentalApi": True}})
    return await c.resp(rid)


async def run_turn(c, tid, text, label, t=120):
    since = len(c.rx)
    rid = await c.req("turn/start",
                      {"threadId": tid, "input": [{"type": "text", "text": text}]})
    await c.resp(rid, t=20)
    done = await c.wait("turn/completed", t=t, since=since)
    print(f"[{label}] turn/completed seen: {done is not None}")
    return since


async def main():
    sock = sys.argv[1]
    A = C("A", sock)
    await A.connect()
    await init(A)
    rid = await A.req("thread/start", {})
    st = await A.resp(rid)
    tid = st["result"]["thread"]["id"]
    print(f"[A] threadId = {tid}")

    print("\n--- A runs turn #1 (flush rollout) ---")
    await run_turn(A, tid, "Reply with the single word OK.", "A")

    print("\n--- B connects and thread/resume {tid} ---")
    B = C("B", sock)
    await B.connect()
    await init(B)
    rid = await B.req("thread/resume", {"threadId": tid})
    rz = await B.resp(rid)
    resume_ok = "result" in (rz or {})
    print(f"[B] resume_ok = {resume_ok} :: {json.dumps(rz)[:200]}")
    await asyncio.sleep(0.5)

    print("\n--- A runs turn #2 — does B observe it? ---")
    a_since = len(A.rx)
    b_since = len(B.rx)
    await run_turn(A, tid, "Reply with the single word TWO.", "A")
    await asyncio.sleep(1.0)
    print(f"[A] methods during turn2: {A.methods(a_since)}")
    print(f"[B] methods during A's turn2: {B.methods(b_since)}")
    b_saw_a = bool([m for m in B.methods(b_since)
                    if m in ("turn/started", "turn/completed", "item/completed")])
    print(f">>> B observed A's turn? {b_saw_a}")

    if resume_ok:
        print("\n--- B runs a turn — does A observe it? ---")
        a2 = len(A.rx)
        b2 = len(B.rx)
        await run_turn(B, tid, "Reply with the single word THREE.", "B")
        await asyncio.sleep(1.0)
        print(f"[B] methods during B's turn: {B.methods(b2)}")
        print(f"[A] methods during B's turn: {A.methods(a2)}")
        a_saw_b = bool([m for m in A.methods(a2)
                        if m in ("turn/started", "turn/completed",
                                 "item/completed")])
        print(f">>> A observed B's turn? {a_saw_b}")

    await A.ws.close()
    await B.ws.close()


asyncio.run(main())
