#!/usr/bin/env python3
"""Spike harness for neige-calm #293: can two clients drive/observe the SAME
codex app-server thread?

WIRE FRAMING (observed, codex-cli 0.133.0):
  * `--listen stdio://` : newline-delimited JSON-RPC (one object per LF line).
  * `--listen unix://PATH`: **WebSocket** over the unix domain socket. The
    server speaks WS text frames carrying JSON-RPC objects. The client MUST
    disable the `permessage-deflate` extension (offer no compression) or the
    handshake is rejected ("Missing, duplicated or incorrect header
    sec-websocket-extensions"). URI path is "ws://localhost/".
  * The unix socket itself is created `srw-------` (owner only) and its parent
    directory is chmod'd to 0700 by the server, so the socket directory must be
    owned by the launching user (a bare shared /tmp fails with EPERM).
  * No `jsonrpc:"2.0"` field is required (the protocol ignores it); `id` may be
    int or string.

This harness opens two independent WS connections to the same socket:
  - Client A: initialize + thread/start  -> owns threadId
  - Client B: initialize + thread/resume {threadId} -> attempts to rejoin
then (optionally) drives a model turn from A and checks BOTH clients' streams.

Usage:
  appserver_thread_sharing.py --sock /path/to/app.sock [--turn] [--prompt TEXT]
"""
import argparse
import asyncio
import json
import sys
import time

from websockets.asyncio.client import unix_connect

URI = "ws://localhost/"


class Client:
    def __init__(self, tag, sock_path):
        self.tag = tag
        self.sock_path = sock_path
        self.ws = None
        self._next_id = 1
        self.received = []          # parsed frames in arrival order
        self._reader_task = None

    async def connect(self):
        # compression=None is REQUIRED: server rejects permessage-deflate.
        self.ws = await unix_connect(self.sock_path, URI, compression=None)
        self._reader_task = asyncio.create_task(self._read_loop())

    async def _read_loop(self):
        try:
            async for raw in self.ws:
                try:
                    obj = json.loads(raw)
                except json.JSONDecodeError:
                    obj = {"__raw__": raw}
                self.received.append(obj)
                m = obj.get("method")
                if m:
                    print(f"[{self.tag}] <- EVENT {m} "
                          f"{json.dumps(obj.get('params'))[:160]}", flush=True)
                else:
                    body = json.dumps(obj.get("result") or obj.get("error"))[:160]
                    print(f"[{self.tag}] <- RESP id={obj.get('id')} {body}",
                          flush=True)
        except Exception as e:  # noqa: BLE001
            print(f"[{self.tag}] reader stopped: {type(e).__name__}", flush=True)

    async def request(self, method, params=None):
        rid = self._next_id
        self._next_id += 1
        msg = {"jsonrpc": "2.0", "id": rid, "method": method}
        if params is not None:
            msg["params"] = params
        print(f"[{self.tag}] -> REQ id={rid} {method} "
              f"{json.dumps(params)[:160] if params else ''}", flush=True)
        await self.ws.send(json.dumps(msg))
        return rid

    async def wait_response(self, rid, timeout=15.0):
        deadline = time.time() + timeout
        while time.time() < deadline:
            for obj in self.received:
                if obj.get("id") == rid and ("result" in obj or "error" in obj):
                    return obj
            await asyncio.sleep(0.03)
        return None

    async def wait_method(self, method, timeout=15.0, since=0):
        deadline = time.time() + timeout
        while time.time() < deadline:
            for obj in self.received[since:]:
                if obj.get("method") == method:
                    return obj
            await asyncio.sleep(0.03)
        return None

    def methods_seen(self, since=0):
        return [o.get("method") for o in self.received[since:] if o.get("method")]

    async def close(self):
        if self.ws:
            await self.ws.close()


async def initialize(c):
    rid = await c.request("initialize", {
        "clientInfo": {"name": f"neige-spike-{c.tag}", "version": "0.0.1"},
        "capabilities": {"experimentalApi": True},
    })
    return await c.wait_response(rid)


async def run(args):
    print("=" * 70, "\nSTEP 1: Client A connect + initialize\n", "=" * 70, sep="")
    A = Client("A", args.sock)
    await A.connect()
    initA = await initialize(A)
    print(f"[A] initialize ok: {initA is not None and 'result' in (initA or {})}")

    print("=" * 70, "\nSTEP 2: Client A thread/start\n", "=" * 70, sep="")
    rid = await A.request("thread/start", {})
    started = await A.wait_response(rid)
    if not started or "result" not in started:
        print("FATAL: thread/start failed:", json.dumps(started))
        await A.close()
        return 2
    res = started["result"]
    thread_id = (res.get("threadId") or res.get("thread", {}).get("id")
                 or res.get("thread", {}).get("threadId"))
    print(f"[A] threadId = {thread_id}")
    print(f"[A] thread/start result = {json.dumps(res)[:400]}")
    await asyncio.sleep(0.5)

    print("=" * 70,
          "\nSTEP 3: Client B connect + initialize + thread/resume {threadId}\n",
          "=" * 70, sep="")
    B = Client("B", args.sock)
    await B.connect()
    initB = await initialize(B)
    print(f"[B] initialize ok: {initB is not None and 'result' in (initB or {})}")
    rid = await B.request("thread/resume", {"threadId": thread_id})
    resumed = await B.wait_response(rid)
    resume_ok = bool(resumed and "result" in resumed)
    print(f"[B] thread/resume ok: {resume_ok}")
    print(f"[B] thread/resume result/err = "
          f"{json.dumps(resumed)[:400] if resumed else 'NONE'}")
    await asyncio.sleep(0.5)

    turn = {"attempted": False}
    if args.turn and resume_ok:
        print("=" * 70,
              "\nSTEP 4: A turn/start (model call) — watch fan-out to A AND B\n",
              "=" * 70, sep="")
        a0, b0 = len(A.received), len(B.received)
        rid = await A.request("turn/start", {
            "threadId": thread_id,
            "input": [{"type": "text", "text": args.prompt}],
        })
        ts = await A.wait_response(rid, timeout=20)
        print(f"[A] turn/start result = {json.dumps(ts)[:300] if ts else 'NONE'}")
        comp_A = await A.wait_method("turn/completed", timeout=args.turn_timeout,
                                     since=a0)
        comp_B = await B.wait_method("turn/completed", timeout=10, since=b0)
        await asyncio.sleep(0.5)
        turn = {
            "attempted": True,
            "completed_A": comp_A is not None,
            "completed_B": comp_B is not None,
            "A_methods": A.methods_seen(a0),
            "B_methods": B.methods_seen(b0),
        }
        print(f"[A] methods during turn: {turn['A_methods']}")
        print(f"[B] methods during turn: {turn['B_methods']}")
    elif args.turn:
        print("Skipping turn: resume not ok.")

    print("=" * 70, "\nSUMMARY\n", "=" * 70, sep="")
    summary = {
        "thread_id": thread_id,
        "thread_start_ok": True,
        "resume_ok": resume_ok,
        "A_all_methods": A.methods_seen(),
        "B_all_methods": B.methods_seen(),
        "turn": turn,
    }
    print(json.dumps(summary, indent=2))
    await A.close()
    await B.close()
    return 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--sock", required=True)
    ap.add_argument("--turn", action="store_true")
    ap.add_argument("--prompt",
                    default="Reply with the single word OK and nothing else.")
    ap.add_argument("--turn-timeout", type=float, default=120.0)
    args = ap.parse_args()
    sys.exit(asyncio.run(run(args)))


if __name__ == "__main__":
    main()
