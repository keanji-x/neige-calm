"""Neige app MCP transport; callbacks are answered independently of computation."""
import fcntl
import json
import os
from pathlib import Path
import queue
import sys
import threading

from . import VERSION
from .runtime import Runtime


class Rpc:
    def __init__(self):
        self.lock = threading.Lock()
        self.pending = {}
        self.sequence = 1000

    def send(self, frame):
        line = json.dumps(frame, ensure_ascii=False, allow_nan=False)
        with self.lock:
            sys.stdout.write(line + "\n")
            sys.stdout.flush()

    def reply(self, request_id, result):
        self.send({"jsonrpc": "2.0", "id": request_id, "result": result})

    def call(self, method, params):
        with self.lock:
            self.sequence += 1
            request_id = f"barra-{self.sequence}"
            response = queue.Queue(maxsize=1)
            self.pending[request_id] = response
        try:
            self.send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
            frame = response.get(timeout=15)
            if "error" in frame:
                raise RuntimeError(f"{method}: {frame['error']}")
            return frame.get("result")
        except queue.Empty as error:
            raise RuntimeError(f"{method}: host acknowledgement timed out") from error
        finally:
            with self.lock:
                self.pending.pop(request_id, None)

    def receive(self, frame):
        with self.lock:
            receiver = self.pending.get(frame.get("id"))
            if receiver is not None:
                try:
                    receiver.put_nowait(frame)
                except queue.Full:
                    pass


def serve():
    root = Path(os.environ["NEIGE_PLUGIN_DATA_DIR"])
    root.mkdir(parents=True, exist_ok=True)
    lock_file = (root / ".process.lock").open("a")
    fcntl.flock(lock_file, fcntl.LOCK_EX | fcntl.LOCK_NB)
    rpc = Rpc()
    manifest = json.loads((Path(__file__).parents[1] / "manifest.json").read_text())
    runtime = None

    def publish(track, kind, payload):
        rpc.call("neige.overlay.set", {"entity_kind": "track", "entity_id": track,
                                       "kind": kind, "payload": payload})

    try:
        for line in sys.stdin:
            try:
                frame = json.loads(line)
                if not isinstance(frame, dict):
                    raise ValueError("frame must be an object")
            except (ValueError, TypeError):
                rpc.send({"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": "invalid JSON-RPC frame"}})
                continue
            if "method" not in frame:
                rpc.receive(frame)
                continue
            request_id = frame.get("id")
            if request_id is None:
                continue
            try:
                method = frame["method"]
                params = frame.get("params", {})
                if not isinstance(params, dict):
                    raise ValueError("params must be an object")
                if method == "initialize":
                    if runtime is not None:
                        raise ValueError("already initialized; reload the plugin to change host configuration")
                    meta = params.get("_meta", {})
                    values = meta.get("dev.neige/config", {}).get("values", {})
                    csv_path = values.get("prices_csv", "")
                    if not isinstance(csv_path, str) or (csv_path and not Path(csv_path).is_absolute()):
                        raise ValueError("prices_csv must be empty or an absolute path")
                    runtime = Runtime(root, publish, csv_path=csv_path)
                    result = {"protocolVersion": params.get("protocolVersion", "2025-11-25"),
                              "serverInfo": {"name": "barra", "version": VERSION},
                              "capabilities": {"tools": {}, "experimental": {"dev.neige/kernel-callbacks": {"version": 1}}}}
                    auth = meta.get("dev.neige/auth", {})
                    if "expected_echo" in auth:
                        result["_meta"] = {"dev.neige/auth": {"echoed_token": auth["expected_echo"]}}
                    rpc.reply(request_id, result)
                    runtime.launch()
                    continue
                if method == "ping":
                    rpc.reply(request_id, {})
                    continue
                if runtime is None:
                    raise ValueError("initialize first")
                if method == "tools/list":
                    rpc.reply(request_id, {"tools": [{"name": t["name"], "description": t["description"],
                                                     "inputSchema": t["input_schema"], "annotations": t["annotations"]}
                                                    for t in manifest["exposes_tools"]]})
                    continue
                if method != "tools/call":
                    rpc.send({"jsonrpc": "2.0", "id": request_id, "error": {"code": -32601, "message": "unknown method"}})
                    continue
                track = params.get("_meta", {}).get("dev.neige/track", {}).get("id")
                if not isinstance(track, str) or not track.strip():
                    raise ValueError("host-provided current Track context is required")
                args = params.get("arguments", {})
                if not isinstance(args, dict):
                    raise ValueError("arguments must be an object")
                tool = params.get("name")
                if tool == "barra.start":
                    result = runtime.start(track, args)
                elif tool == "barra.series":
                    result = runtime.series(track, args)
                else:
                    if args:
                        raise ValueError("this tool takes no arguments; Track comes from host context")
                    actions = {"barra.status": runtime.status, "barra.refresh": runtime.refresh, "barra.stop": runtime.stop}
                    if tool not in actions:
                        raise ValueError("unknown tool")
                    result = actions[tool](track)
                rpc.reply(request_id, {"content": [{"type": "text", "text": json.dumps(result, ensure_ascii=False)}],
                                       "structuredContent": result})
            except Exception as error:
                if frame.get("method") == "tools/call":
                    rpc.reply(request_id, {"isError": True, "content": [{"type": "text", "text": str(error)}]})
                else:
                    rpc.send({"jsonrpc": "2.0", "id": request_id, "error": {"code": -32602, "message": str(error)}})
    finally:
        if runtime:
            runtime.close()
        lock_file.close()
