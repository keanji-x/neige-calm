"""One polling worker, one in-flight calculation; no job recovery framework."""
from copy import deepcopy
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import tempfile
import threading

from .config import Config
from .data import load_prices
from .model import calculate
from .report import result_tables, status_table, overview_table
from .series import chart_series


def utc_now():
    return datetime.now(timezone.utc)


def atomic_text(path, text):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(dir=path.parent, prefix=".barra-")
    try:
        with os.fdopen(fd, "w") as stream:
            stream.write(text)
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def atomic_json(path, value):
    atomic_text(path, json.dumps(value, ensure_ascii=False, allow_nan=False, indent=2))


class Runtime:
    def __init__(self, root, publish, csv_path="", clock=utc_now, loader=load_prices):
        self.root = Path(root)
        self.publish = publish
        self.csv_path = csv_path
        self.clock = clock
        self.loader = loader
        self.lock = threading.RLock()
        self.wake = threading.Event()
        self.closed = threading.Event()
        self.thread = None
        path = self.root / "state.json"
        self.states = json.loads(path.read_text()) if path.exists() else {}
        for state in self.states.values():
            Config.parse(state["config"])
            if state["phase"] in ("queued", "running", "publishing"):
                state["phase"] = "interrupted"
                state["error"] = "上次运行被中断；可手动 refresh，或等待下一次每日更新。"
            state["pending"] = False
        self.dirty = set(self.states)
        self.republish = {track for track, state in self.states.items() if state.get("result")}

    def save(self):
        atomic_json(self.root / "state.json", self.states)

    def update(self, track, **changes):
        # A refused synchronous write must not leave accepted work in memory.
        previous = self.states.get(track)
        self.states[track] = (previous or {}) | changes
        try:
            self.save()
        except Exception:
            if previous is None:
                del self.states[track]
            else:
                self.states[track] = previous
            raise
        return self.states[track]

    def start(self, track, args):
        config = Config.parse(args)
        with self.lock:
            old = self.states.get(track)
            if old and old["phase"] in ("running", "publishing"):
                raise ValueError("calculation is active; stop it and wait before changing configuration")
            state = deepcopy(old) if old else {"result": None, "last_success": None}
            # Old output is kept, but its original configuration remains inside result.
            state.update(config=config.json(), enabled=True, phase="queued", error=None,
                         pending=True, last_attempt_day=None)
            self.update(track, **state)
            self.dirty.add(track)
        self.wake.set()
        return self.status(track)

    def refresh(self, track):
        with self.lock:
            state = self.require(track)
            if not state["enabled"]:
                raise ValueError("study is stopped; use barra.start to enable it")
            if state["phase"] not in ("running", "publishing"):
                self.update(track, pending=True, phase="queued", error=None)
                self.dirty.add(track)
        self.wake.set()
        return self.status(track)

    def stop(self, track):
        with self.lock:
            state = self.require(track)
            # A running calculation is allowed to return, but its output is discarded.
            phase = state["phase"] if state["phase"] in ("running", "publishing") else "stopped"
            self.update(track, enabled=False, pending=False, phase=phase)
            self.dirty.add(track)
        self.wake.set()
        return self.status(track)

    def require(self, track):
        if track not in self.states:
            raise ValueError("no study for this Track; use barra.start")
        return self.states[track]

    def status(self, track):
        with self.lock:
            state = deepcopy(self.require(track))
        result = state.pop("result")
        state.pop("pending", None)
        state["latest"] = ({k: result[k] for k in ("as_of", "run_id", "config", "validation", "portfolio", "limitations")}
                           if result else None)
        return state

    def send_status(self, track):
        with self.lock:
            payload = status_table(self.states[track])
            overview = overview_table(self.states[track])
        self.publish(track, "barra.status", payload)
        self.publish(track, "barra.overview", overview)

    def series(self, track, args):
        with self.lock:
            result = deepcopy(self.require(track).get("result"))
        return chart_series(result, args, int(self.clock().timestamp() * 1000))

    def process_once(self):
        now = self.clock()
        day = now.date().isoformat()
        with self.lock:
            dirty = sorted(self.dirty)
            self.dirty.clear()
        for track in dirty:
            try:
                with self.lock:
                    result = deepcopy(self.states[track].get("result")) if track in self.republish else None
                if result:
                    for kind, payload in result_tables(result).items():
                        self.publish(track, kind, payload)
                    with self.lock:
                        self.republish.discard(track)
                self.send_status(track)
            except Exception:
                with self.lock:
                    self.dirty.add(track)
        with self.lock:
            candidate = next((track for track, s in self.states.items() if s["enabled"]
                              and s["phase"] not in ("running", "publishing")
                              and (s["pending"] or (now.hour >= s["config"]["update_hour_utc"]
                                   and s["last_attempt_day"] != day))), None)
            if candidate is None:
                return False
            state = self.update(candidate, pending=False, phase="running", last_attempt_day=day, error=None)
            config = Config.parse(state["config"])
        try:
            self.send_status(candidate)
            prices, source = self.loader(config, now, self.csv_path)
            result = calculate(prices, config, source)
            folder = self.root / "studies" / hashlib.sha256(candidate.encode()).hexdigest()
            with self.lock:
                state = self.states[candidate]
                if not state["enabled"] or self.closed.is_set():
                    self.update(candidate, phase="stopped")
                    return True
                atomic_text(folder / "prices.csv", prices.to_csv(float_format="%.12g", index_label="date"))
                atomic_json(folder / "result.json", result)
                self.update(candidate, phase="publishing")
            # Each native overlay is one write; captions identify the shared result.
            # Status becomes succeeded only after every projection is acknowledged.
            for kind, payload in result_tables(result).items():
                self.publish(candidate, kind, payload)
            with self.lock:
                state = self.states[candidate]
                completed_view = state | {"result": result, "error": None,
                                          "phase": "succeeded" if state["enabled"] else "stopped"}
            # The primary reader-facing result must be acknowledged before success.
            self.publish(candidate, "barra.overview", overview_table(completed_view))
            with self.lock:
                state = self.states[candidate]
                self.update(candidate, result=result, last_success=self.clock().isoformat(),
                            phase="succeeded" if state["enabled"] else "stopped", error=None)
        except Exception as error:
            with self.lock:
                state = self.states[candidate]
                state.update(phase="failed" if state["enabled"] else "stopped", error=str(error)[:1000])
                try:
                    self.save()
                except Exception as save_error:
                    # Even with storage unavailable, the ended worker is not running.
                    state["error"] = f"{str(error)[:500]}; state write failed: {str(save_error)[:400]}"
        finally:
            with self.lock:
                self.dirty.add(candidate)
            try:
                self.send_status(candidate)
            except Exception:
                pass
        return True

    def run(self):
        while not self.closed.is_set():
            self.wake.clear()
            try:
                worked = self.process_once()
            except Exception as error:
                import sys
                print(f"barra runtime: {error}", file=sys.stderr)
                worked = False
            if not worked:
                self.wake.wait(60)

    def launch(self):
        if self.thread is None:
            self.thread = threading.Thread(target=self.run, name="barra-daily", daemon=True)
            self.thread.start()

    def close(self):
        self.closed.set()
        self.wake.set()
