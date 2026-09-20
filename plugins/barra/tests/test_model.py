import json

import numpy as np
import pytest

from barra.config import Config
from barra.data import load_prices
from barra.model import FACTORS, calculate, exposures
from barra.report import result_tables
from barra.runtime import utc_now


def test_lagged_exposures_match_independent_regression(prices, config):
    result = calculate(prices, config, "fixture")
    x, _ = exposures(prices, config.symbols, config.benchmark, len(prices) - 2)
    y = prices[list(config.symbols)].iloc[-1].to_numpy() / prices[list(config.symbols)].iloc[-2].to_numpy() - 1
    expected = np.linalg.solve(x.T @ x, x.T @ y)
    actual = [result["history"][-1][name] for name in FACTORS]
    np.testing.assert_allclose(actual, expected, rtol=1e-9, atol=1e-12)


def test_future_prices_do_not_change_earlier_results(prices, config):
    baseline = calculate(prices, config, "fixture")
    edited = prices.copy()
    edited.iloc[-1, :-1] *= np.linspace(0.85, 1.2, len(config.symbols))
    changed = calculate(edited, config, "fixture")
    assert [r["date"] for r in baseline["history"]] == [r["date"] for r in changed["history"]]
    keys = set(baseline["history"][0]) - {"date"}
    for key in keys:
        np.testing.assert_allclose([r[key] for r in baseline["history"][:-1]],
                                   [r[key] for r in changed["history"][:-1]], rtol=1e-10, atol=1e-12)
    assert baseline["history"][-1]["predicted_daily_volatility"] == pytest.approx(
        changed["history"][-1]["predicted_daily_volatility"], rel=1e-10, abs=1e-12)
    assert baseline["history"][-1]["portfolio_return"] != changed["history"][-1]["portfolio_return"]


def test_model_is_finite_reproducible_and_covariance_psd(prices, config):
    result = calculate(prices, config, "fixture")
    assert result == calculate(prices, config, "fixture")
    json.dumps(result, allow_nan=False)
    covariance = np.array(result["daily_factor_covariance"])
    assert np.linalg.eigvalsh(covariance).min() >= -1e-14
    assert all(v >= 0 for v in result["daily_specific_variance"].values())
    assert result["validation"]["observations"] == config.history_days
    assert 0 <= result["validation"]["within_1_96_sigma"] <= 1
    assert result["validation"]["max_drawdown"] <= 0
    for name in ("beta_z", "momentum_z", "residual_volatility_z"):
        values = np.array([e[name] for e in result["exposures"]])
        assert abs(values.mean()) < 1e-10
        assert abs(values.std() - 1) < 1e-10
    assert "NOT the MSCI" in result["limitations"]


def test_raw_beta_and_residual_volatility_match_single_stock_ols(prices, config):
    x, raw = exposures(prices, config.symbols, config.benchmark, len(prices) - 1)
    returns = prices.iloc[-253:].pct_change(fill_method=None).iloc[1:]
    design = np.column_stack([np.ones(252), returns[config.benchmark]])
    y = returns[config.symbols[0]].to_numpy()
    fitted, *_ = np.linalg.lstsq(design, y, rcond=None)
    assert raw[0, 0] == pytest.approx(fitted[1])
    assert raw[0, 2] == pytest.approx(np.sqrt(np.sum((y - design @ fitted) ** 2) / 250 * 252))
    assert raw[0, 1] == pytest.approx(np.log(prices.iloc[-22, 0] / prices.iloc[-253, 0]))


def test_degenerate_data_refused(prices, config):
    prices[config.benchmark] = 100
    with pytest.raises(ValueError, match="variance"):
        calculate(prices, config, "fixture")


def test_missing_data_is_not_filled(prices, config, tmp_path):
    prices.iloc[-3, 0] = np.nan
    path = tmp_path / "prices.csv"
    prices.to_csv(path, index_label="date")
    with pytest.raises(ValueError, match="missing/nonpositive"):
        load_prices(config, utc_now(), str(path))


def test_missing_benchmark_session_is_not_silently_compressed(prices, config, tmp_path):
    from datetime import datetime, timezone

    prices.loc["2026-09-16", config.benchmark] = np.nan
    path = tmp_path / "prices.csv"
    prices.to_csv(path, index_label="date")
    with pytest.raises(ValueError, match="benchmark.*missing"):
        load_prices(config, datetime(2026, 9, 20, tzinfo=timezone.utc), str(path))


def test_missing_benchmark_before_retained_window_is_irrelevant(prices, config, tmp_path):
    from datetime import datetime, timezone

    path = tmp_path / "prices.csv"
    now = datetime(2026, 9, 20, tzinfo=timezone.utc)
    prices.to_csv(path, index_label="date")
    original, _ = load_prices(config, now, str(path))
    assert original.index[0] > prices.index[0]
    prices.iloc[0, -1] = np.nan
    prices.to_csv(path, index_label="date")
    changed, _ = load_prices(config, now, str(path))
    assert changed.equals(original)


def test_csv_roundtrip_replay_and_excludes_current_day(prices, config, tmp_path):
    from datetime import datetime, timezone

    path = tmp_path / "prices.csv"
    prices.to_csv(path, index_label="date")
    now = datetime(2026, 9, 18, 20, 0, tzinfo=timezone.utc)
    loaded, source = load_prices(config, now, str(path))
    assert loaded.index[-1].date().isoformat() == "2026-09-17"
    assert "replay" in source
    result = calculate(loaded, config, source)
    assert result["as_of"] == "2026-09-17"


def test_stale_prices_refused(prices, config, tmp_path):
    from datetime import datetime, timezone

    path = tmp_path / "prices.csv"
    prices.to_csv(path, index_label="date")
    with pytest.raises(ValueError, match="seven"):
        load_prices(config, datetime(2026, 10, 1, tzinfo=timezone.utc), str(path))


@pytest.mark.parametrize("args", [{"track_id": "other"}, {"symbols": ["A"]},
    {"update_hour_utc": True}, {"history_days": 500}, {"benchmark": "SHOP.TO"}])
def test_invalid_config(args):
    with pytest.raises(ValueError):
        Config.parse(args)


def test_report_payloads_fit_native_table_contract(prices, config):
    result = calculate(prices, config, "fixture")
    for kind, payload in result_tables(result).items():
        assert kind.startswith("barra.")
        assert set(payload) == {"columns", "rows", "caption"}
        assert len(payload["rows"]) <= 500
        assert len(payload["columns"]) <= 32
        keys = {c["key"] for c in payload["columns"]}
        assert all(set(row) <= keys for row in payload["rows"])
        if kind in ("barra.summary", "barra.exposures", "barra.history"):
            assert result["run_id"] in payload["caption"]
        assert all(isinstance(v, (str, int, float)) for row in payload["rows"] for v in row.values())
        assert len(json.dumps(payload).encode()) < 256 * 1024
