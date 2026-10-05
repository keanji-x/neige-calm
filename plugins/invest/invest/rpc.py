"""Neige stdio app protocol with independent callback acknowledgement routing."""
import json
import os
from pathlib import Path
import queue
import sys
import threading
import time

from . import VERSION, series
from .runtime import Runtime
from .broker import Broker
from .errors import Refused
from .portfolio import RESEARCH_TOOLS, VIEWS, Portfolio
from .research_views import research_units
from .settings import InvestConfig


class Rpc:
    def __init__(self):
        self.lock = threading.Lock()
        self.pending = {}
        self.sequence = 0

    def send(self, frame):
        with self.lock:
            sys.stdout.write(json.dumps(frame, ensure_ascii=True, allow_nan=False) + "\n")
            sys.stdout.flush()

    def call(self, method, params):
        with self.lock:
            self.sequence += 1
            key = f"invest-{self.sequence}"
            receiver = queue.Queue(maxsize=1)
            self.pending[key] = receiver
        try:
            self.send({"jsonrpc": "2.0", "id": key, "method": method, "params": params})
            reply = receiver.get(timeout=10)
            if "error" in reply:
                raise ValueError("host refused report projection")
        finally:
            with self.lock:
                self.pending.pop(key, None)

    def receive(self, frame):
        with self.lock:
            receiver = self.pending.get(frame.get("id"))
            if receiver:
                try:
                    receiver.put_nowait(frame)
                except queue.Full:
                    pass


def serve():
    root = Path(os.environ["NEIGE_PLUGIN_DATA_DIR"])
    rpc = Rpc()
    manifest = json.loads((Path(__file__).parents[1] / "manifest.json").read_text())
    runtime = None
    work = queue.Queue(maxsize=32)

    def reply(request_id, result):
        rpc.send({"jsonrpc": "2.0", "id": request_id, "result": result})

    def worker():
        while True:
            frame = work.get()
            if frame is None:
                return
            try:
                params = frame["params"]
                meta, name = params.get("_meta", {}), params.get("name")
                result = runtime.portfolio.call(meta.get(series.TRACK_KEY), name, params.get("arguments", {}),
                                                meta.get(series.CALLER_KEY))
                if name in RESEARCH_TOOLS:
                    # Statelessly onto the caller's own Track; a lost projection is redone on its next call.
                    for kind, payload in research_units(result).items():
                        try:
                            runtime.publish(meta[series.TRACK_KEY]["id"], kind, payload)
                        except Exception:
                            pass
                # Every write wakes the loop that owns broker access.
                if name not in VIEWS:
                    runtime.wake.set()
                reply(frame["id"], {"content": [{"type": "text", "text": json.dumps(result, ensure_ascii=True)}],
                                    "structuredContent": result})
            except Refused as error:
                rpc.send({"jsonrpc": "2.0", "id": frame["id"], "error": {"code": error.code, "message": str(error)}})
            except Exception as error:
                message = str(error) if isinstance(error, ValueError) else "invalid request or unavailable ledger"
                reply(frame["id"], {"isError": True, "content": [{"type": "text", "text": message}]})

    def series_worker():
        # Its own thread: a slow SDK read for a chart never delays a portfolio tool call.
        while True:
            frame = series_work.get()
            params = frame["params"]
            try:
                series.admit(params.get("_meta"))
                reply(frame["id"], series.show(runtime.portfolio.broker, params.get("arguments", {}),
                                               time.time_ns() // 1_000_000))
            except Refused as error:
                rpc.send({"jsonrpc": "2.0", "id": frame["id"], "error": {"code": error.code, "message": str(error)}})
            except Exception:
                reply(frame["id"], series.tool_error("invalid request or unavailable source"))

    series_work = queue.Queue(maxsize=32)
    for target, name in ((worker, "invest-tools"), (series_worker, "invest-series")):
        threading.Thread(target=target, name=name, daemon=True).start()
    try:
        for line in sys.stdin:
            request_id = None
            try:
                if len(line) > 1_100_000:
                    raise ValueError("request too large")
                frame = json.loads(line)
                if not isinstance(frame, dict):
                    raise ValueError("frame must be an object")
                request_id = frame.get("id")
                if "method" not in frame:
                    rpc.receive(frame)
                    continue
                if request_id is None:
                    continue
                params = frame.get("params", {})
                if not isinstance(params, dict):
                    raise ValueError("params must be an object")
                method = frame["method"]
                if method == "initialize":
                    if runtime is not None:
                        raise ValueError("already initialized")
                    meta = params.get("_meta", {})
                    values = meta.get("dev.neige/config", {}).get("values", {})
                    config = InvestConfig.parse(values)
                    portfolio = Portfolio(root, config, Broker(config, str(root)))
                    runtime = Runtime(portfolio, lambda track, kind, payload: rpc.call("neige.overlay.set", {
                        "entity_kind": "track", "entity_id": track, "kind": kind, "payload": payload}))
                    result = {"protocolVersion": params.get("protocolVersion", "2025-11-25"),
                              "serverInfo": {"name": "invest", "version": VERSION},
                              "capabilities": {"tools": {}, "experimental": {"dev.neige/kernel-callbacks": {"version": 1}}}}
                    auth = meta.get("dev.neige/auth", {})
                    if "expected_echo" in auth:
                        result["_meta"] = {"dev.neige/auth": {"echoed_token": auth["expected_echo"]}}
                    reply(request_id, result)
                    runtime.launch()
                elif method == "ping":
                    reply(request_id, {})
                elif runtime is None:
                    raise ValueError("initialize first")
                elif method == "tools/list":
                    reply(request_id, {"tools": [{"name": t["name"], "description": t["description"],
                                                   "inputSchema": t["input_schema"], "annotations": t["annotations"]}
                                                  for t in manifest["exposes_tools"]]})
                elif method == "tools/call":
                    (series_work if params.get("name") == series.TOOL else work).put_nowait(frame)
                else:
                    raise ValueError("unknown method")
            except Exception as error:
                message = str(error) if isinstance(error, ValueError) else "invalid request or server busy"
                rpc.send({"jsonrpc": "2.0", "id": request_id, "error": {"code": -32602, "message": message}})
    finally:
        if runtime:
            runtime.close()
