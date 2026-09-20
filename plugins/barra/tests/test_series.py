from datetime import datetime, timezone

import numpy as np
import pytest

from barra.model import calculate
from barra.report import overview_table, leaders_table, result_tables
from barra.series import ASSETS, chart_series

NOW = 1789862400000


@pytest.fixture
def result(prices, config):
    return calculate(prices, config, "fixture")


def query(**overrides):
    return {"series": list(ASSETS), "fields": ["close"], "period": "day", "mode": "live",
            "start": "2026-06-01", "as_of": "2026-09-19", "deadline_ms": NOW + 10000} | overrides


def test_series_values_match_underlying_research(result):
    series = chart_series(result, query(), NOW)["series"]
    history = result["history"]
    assert all(s["status"] == "ok" for s in series)
    assert series[0]["points"][-1][1] == pytest.approx(sum(r["beta"] for r in history) * 100)
    assert series[1]["points"][-1][1] == pytest.approx(sum(r["momentum"] for r in history) * 100)
    assert series[3]["points"][-1][1] == pytest.approx(history[-1]["predicted_daily_volatility"] * np.sqrt(252) * 100)
    assert series[4]["points"][-1][1] == pytest.approx(np.std([r["portfolio_return"] for r in history[-21:]], ddof=1) * np.sqrt(252) * 100)
    assert series[5]["points"][-1][1] == pytest.approx(history[-1]["equal_weight_equity"] * 100)
    assert len(series[4]["points"]) == len(history) - 20
    for entry in series:
        stamps = [p[0] for p in entry["points"]]
        assert stamps == sorted(set(stamps))
        assert all(stamp % 86400000 == 0 for stamp in stamps)
        assert entry["complete_through"] == result["as_of"]


def test_window_sum_and_no_future_realized_data(result):
    entries = chart_series(result, query(start="2026-09-01", as_of="2026-09-10"), NOW)["series"]
    included = [r for r in result["history"] if "2026-09-01" <= r["date"] <= "2026-09-10"]
    assert entries[0]["points"][0][1] == pytest.approx(included[0]["beta"] * 100)
    assert entries[0]["points"][-1][1] == pytest.approx(sum(r["beta"] for r in included) * 100)
    result["history"][-1]["portfolio_return"] = 1000
    changed = chart_series(result, query(start="2026-09-01", as_of="2026-09-10"), NOW)["series"]
    assert entries == changed


def test_frozen_inclusion_matches_host_contract(result):
    frozen = chart_series(result, query(mode="frozen", as_of=result["as_of"]), NOW)["series"]
    cutoff = int(datetime.fromisoformat(result["as_of"]).replace(tzinfo=timezone.utc).timestamp() * 1000)
    assert all(p[0] < cutoff for s in frozen for p in s["points"])
    live = chart_series(result, query(), NOW)["series"]
    assert all(s["points"][-1][0] == cutoff for s in live)


@pytest.mark.parametrize("override", [{"period": "week"}, {"fields": ["volume"]}, {"mode": "bad"},
    {"deadline_ms": NOW}, {"deadline_ms": True}, {"start": "20260901"}, {"series": ["BARRA:Beta"] * 2},
    {"as_of": "2020-01-01"}, {"track_id": "other"}])
def test_invalid_chart_requests(override):
    with pytest.raises(ValueError):
        chart_series(None, query(**override), NOW)


def test_unavailable_and_unknown_are_explicit(result):
    assert chart_series(None, query(series=["BARRA:Beta"]), NOW)["series"][0]["status"] == "unavailable"
    assert chart_series(result, query(series=["BARRA:Missing"]), NOW)["series"][0]["status"] == "unknown_asset"
    assert all(s["status"] == "unavailable" for s in chart_series(result, query(start="2026-09-18"), NOW)["series"])


def test_overview_is_compact_and_only_surfaces_actionable_status(result):
    state = {"result": result, "enabled": True, "phase": "succeeded"}
    overview = overview_table(state)
    assert len(overview["rows"]) == 1
    assert len(overview["columns"]) == 3
    assert result["as_of"] in overview["caption"]
    assert "succeeded" not in str(overview) and "最近错误" not in str(overview)
    assert "不同批次" in overview_table(state | {"phase": "failed"})["caption"]
    assert "暂停" in overview_table(state | {"enabled": False})["caption"]
    assert len(leaders_table(result)["rows"]) == 3
    assert result["as_of"] in leaders_table(result)["caption"]
    assert result["run_id"][:6] in leaders_table(result)["caption"]
    replay = result | {"source": "CSV replay (user-supplied adjusted closes)"}
    assert "离线回放" in overview_table(state | {"result": replay})["caption"]
    for kind in ("barra.leaders", "barra.validation"):
        caption = result_tables(result)[kind]["caption"]
        assert result["as_of"] in caption and result["run_id"][:6] in caption
