from copy import deepcopy
from datetime import datetime, timezone, timedelta
import threading

import pytest

from barra.runtime import Runtime

NOW = datetime(2026, 9, 20, 9, tzinfo=timezone.utc)


def make_runtime(tmp_path, prices, calls, clock=lambda: NOW, loader=None):
    return Runtime(tmp_path, lambda *args: calls.append(args), clock=clock,
                   loader=loader or (lambda *args: (prices.copy(), "fixture")))


def test_daily_refresh_coalesces_and_stop_preserves_result(tmp_path, prices, config):
    calls = []
    clock = [NOW]
    runtime = make_runtime(tmp_path, prices, calls, clock=lambda: clock[0])
    runtime.start("track-a", config.json())
    runtime.refresh("track-a")
    runtime.refresh("track-a")
    assert runtime.process_once()
    assert not runtime.process_once()
    assert runtime.status("track-a")["phase"] == "succeeded"
    assert len([c for c in calls if c[1] == "barra.exposures"]) == 1
    clock[0] += timedelta(days=1)
    assert runtime.process_once()
    assert len([c for c in calls if c[1] == "barra.exposures"]) == 2
    old = runtime.status("track-a")["latest"]
    runtime.stop("track-a")
    clock[0] += timedelta(days=1)
    assert not runtime.process_once()
    assert runtime.status("track-a")["latest"] == old


def test_failure_keeps_previous_success_and_reports_error(tmp_path, prices, config):
    calls = []
    runtime = make_runtime(tmp_path, prices, calls)
    runtime.start("track-a", config.json())
    runtime.process_once()
    before = deepcopy(runtime.states["track-a"]["result"])

    def failed(*args):
        raise ValueError("market data missing")

    runtime.loader = failed
    runtime.refresh("track-a")
    runtime.process_once()
    assert runtime.status("track-a")["phase"] == "failed"
    assert runtime.status("track-a")["error"] == "market data missing"
    assert runtime.states["track-a"]["result"] == before
    assert len([c for c in calls if c[1] == "barra.exposures"]) == 1


def test_stop_during_calculation_discards_output(tmp_path, prices, config):
    entered, release = threading.Event(), threading.Event()

    def loader(*args):
        entered.set()
        assert release.wait(10)
        return prices.copy(), "fixture"

    calls = []
    runtime = make_runtime(tmp_path, prices, calls, loader=loader)
    runtime.start("track-a", config.json())
    worker = threading.Thread(target=runtime.process_once)
    worker.start()
    try:
        assert entered.wait(5)
        assert runtime.status("track-a")["phase"] == "running"
        with pytest.raises(ValueError, match="active"):
            runtime.start("track-a", config.json())
        runtime.stop("track-a")
    finally:
        release.set()
        worker.join(10)
    assert not worker.is_alive()
    assert runtime.status("track-a")["phase"] == "stopped"
    assert not [c for c in calls if c[1] not in ("barra.status", "barra.overview")]
    assert runtime.status("track-a")["latest"] is None


def test_two_tracks_have_separate_state(tmp_path, prices, config):
    calls = []
    runtime = make_runtime(tmp_path, prices, calls)
    runtime.start("track-a", config.json())
    runtime.start("track-b", config.json())
    runtime.stop("track-b")
    runtime.process_once()
    assert runtime.status("track-a")["latest"]
    assert runtime.status("track-b")["latest"] is None
    assert all(track == "track-a" for track, kind, _ in calls if kind not in ("barra.status", "barra.overview"))


def test_publication_failure_is_not_success(tmp_path, prices, config):
    calls = []
    runtime = make_runtime(tmp_path, prices, calls)

    def publish(track, kind, payload):
        if kind == "barra.history":
            raise ValueError("host refused overlay")
        calls.append((track, kind, payload))

    runtime.publish = publish
    runtime.start("track-a", config.json())
    runtime.process_once()
    assert runtime.status("track-a")["phase"] == "failed"
    assert runtime.status("track-a")["latest"] is None
    assert runtime.status("track-a")["last_success"] is None


@pytest.mark.parametrize("prior_success", [False, True])
def test_completed_overview_requires_ack_before_success(tmp_path, prices, config, prior_success):
    runtime = make_runtime(tmp_path, prices, [])
    runtime.start("track-a", config.json())
    if prior_success:
        runtime.process_once()
        changed = prices.copy()
        changed.iloc[-1, 0] *= 1.01
        runtime.loader = lambda *args: (changed, "fixture")
        runtime.refresh("track-a")
    previous = deepcopy(runtime.states["track-a"]["result"])
    previous_success = runtime.states["track-a"]["last_success"]
    calls = []

    def reject_completed_overview(track, kind, payload):
        if kind == "barra.overview" and payload["rows"][0]["forecast"] != "—" and "更新" not in payload["caption"]:
            raise ValueError("completed overview rejected")
        calls.append((track, kind, payload))

    runtime.publish = reject_completed_overview
    runtime.process_once()
    assert runtime.status("track-a")["phase"] == "failed"
    assert runtime.status("track-a")["error"] == "completed overview rejected"
    assert runtime.states["track-a"]["result"] == previous
    assert runtime.states["track-a"]["last_success"] == previous_success
    runtime.publish = lambda *args: calls.append(args)
    runtime.refresh("track-a")
    runtime.process_once()
    assert runtime.status("track-a")["phase"] == "succeeded"


def test_restart_does_not_repeat_same_day_or_resume_interrupted_job(tmp_path, prices, config):
    runtime = make_runtime(tmp_path, prices, [])
    runtime.start("track-a", config.json())
    runtime.process_once()
    runtime.states["track-a"]["phase"] = "running"
    runtime.save()
    restarted = make_runtime(tmp_path, prices, [])
    assert restarted.status("track-a")["phase"] == "interrupted"
    assert not restarted.process_once()
    assert restarted.status("track-a")["latest"] is not None


def test_failed_start_write_does_not_start_in_background(tmp_path, prices, config, monkeypatch):
    runtime = make_runtime(tmp_path, prices, [])
    save = runtime.save

    def failed():
        raise OSError("disk full")

    monkeypatch.setattr(runtime, "save", failed)
    with pytest.raises(OSError):
        runtime.start("track-a", config.json())
    monkeypatch.setattr(runtime, "save", save)
    assert "track-a" not in runtime.states
    assert not runtime.process_once()


def test_failed_claim_write_does_not_wedge_running(tmp_path, prices, config, monkeypatch):
    runtime = make_runtime(tmp_path, prices, [])
    runtime.start("track-a", config.json())
    save = runtime.save

    def failed():
        raise OSError("temporary I/O error")

    monkeypatch.setattr(runtime, "save", failed)
    with pytest.raises(OSError):
        runtime.process_once()
    monkeypatch.setattr(runtime, "save", save)
    assert runtime.status("track-a")["phase"] == "queued"
    assert runtime.process_once()
    assert runtime.status("track-a")["phase"] == "succeeded"


def test_failed_completion_write_is_not_left_running(tmp_path, prices, config, monkeypatch):
    runtime = make_runtime(tmp_path, prices, [])
    runtime.start("track-a", config.json())
    save = runtime.save

    def fail_at_completion():
        if runtime.states["track-a"]["phase"] in ("succeeded", "failed"):
            raise OSError("state unavailable")
        save()

    monkeypatch.setattr(runtime, "save", fail_at_completion)
    runtime.process_once()
    assert runtime.status("track-a")["phase"] == "failed"
    assert runtime.status("track-a")["latest"] is None
    monkeypatch.setattr(runtime, "save", save)
    runtime.refresh("track-a")
    assert runtime.process_once()
    assert runtime.status("track-a")["phase"] == "succeeded"


def test_saved_input_matches_result_hash(tmp_path, prices, config):
    import hashlib

    runtime = make_runtime(tmp_path, prices, [])
    runtime.start("track-a", config.json())
    runtime.process_once()
    snapshot = next((tmp_path / "studies").glob("*/prices.csv"))
    assert hashlib.sha256(snapshot.read_bytes()).hexdigest() == runtime.states["track-a"]["result"]["data_sha256"]


def test_reload_republishes_views_without_recalculating(tmp_path, prices, config):
    runtime = make_runtime(tmp_path, prices, [])
    runtime.start("track-a", config.json())
    runtime.process_once()
    calls = []

    def forbidden(*args):
        raise AssertionError("reload should not fetch")

    restarted = make_runtime(tmp_path, prices, calls, loader=forbidden)
    assert not restarted.process_once()
    assert {kind for _, kind, _ in calls} >= {"barra.leaders", "barra.overview", "barra.validation"}
    calls.clear()
    assert not restarted.process_once()
    assert not calls
