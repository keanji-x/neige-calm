"""The production `run` stdio entry point driven as the kernel would drive it."""
import json
import os
import queue
import subprocess
import threading

from rig import ROOT


class Host:
    def __init__(self, home, data_dir, config):
        data_dir.mkdir(exist_ok=True)
        env = {"PATH": os.defpath, "HOME": str(home), "LANG": "C.UTF-8",
               "NEIGE_PLUGIN_DATA_DIR": str(data_dir)}
        self.proc = subprocess.Popen([str(ROOT / "run")], cwd=data_dir, env=env,
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.frames = queue.Queue()
        self.sequence = 0
        self.overlays = []
        self.thread = threading.Thread(target=self.read, daemon=True)
        self.thread.start()
        init = self.request("initialize", {"protocolVersion": "2025-11-25", "_meta": {
            "dev.neige/auth": {"expected_echo": "fixture-token"},
            "dev.neige/config": {"values": config}}})
        assert init["_meta"]["dev.neige/auth"]["echoed_token"] == "fixture-token"

    def read(self):
        for line in self.proc.stdout:
            self.frames.put(json.loads(line))

    def send(self, frame):
        self.proc.stdin.write(json.dumps(frame) + "\n")
        self.proc.stdin.flush()

    def receive(self):
        frame = self.frames.get(timeout=15)
        if frame.get("method") == "neige.overlay.set":
            self.overlays.append(frame["params"])
            self.send({"jsonrpc": "2.0", "id": frame["id"], "result": {"overlay_id": "fixture"}})
        return frame

    def response(self, method, params):
        """The whole response frame, a JSON-RPC error included."""
        self.sequence += 1
        key = self.sequence
        self.send({"jsonrpc": "2.0", "id": key, "method": method, "params": params})
        while True:
            frame = self.receive()
            if frame.get("id") == key and "method" not in frame:
                return frame

    def request(self, method, params):
        frame = self.response(method, params)
        assert "error" not in frame, frame
        return frame["result"]

    def tool(self, name, args, track, caller=None):
        params = {"name": name, "arguments": args, "_meta": {"dev.neige/track": {"id": track}}}
        if caller is not None:
            params["_meta"]["dev.neige/caller"] = caller
        return self.request("tools/call", params)

    def close(self):
        self.proc.stdin.close()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait(timeout=5)
        self.thread.join(timeout=5)
        self.proc.stdout.close()
        self.proc.stderr.close()
