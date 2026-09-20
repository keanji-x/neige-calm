"""Completed-session adjusted closes; CSV is an explicit offline replay source."""
from datetime import datetime, timedelta
from pathlib import Path
from zoneinfo import ZoneInfo

import numpy as np
import pandas as pd


def load_prices(config, now: datetime, csv_path: str = ""):
    # Conservative: exclude today's New York bar, even after the closing bell.
    cutoff = now.astimezone(ZoneInfo("America/New_York")).date()
    symbols = [*config.symbols, config.benchmark]
    if csv_path:
        prices = pd.read_csv(Path(csv_path), index_col="date", parse_dates=True)
        source = "CSV replay (user-supplied adjusted closes)"
    else:
        import yfinance as yf

        bars = yf.download(
            symbols, start=(cutoff - timedelta(days=1800)).isoformat(),
            end=cutoff.isoformat(), auto_adjust=True, actions=False,
            progress=False, threads=False, timeout=20, multi_level_index=True,
        )
        if bars.empty or "Close" not in bars.columns.get_level_values(0):
            raise ValueError("Yahoo returned no adjusted closes")
        prices = bars["Close"].copy()
        source = "Yahoo Finance via yfinance; dividend/split-adjusted close"
    if not isinstance(prices.index, pd.DatetimeIndex) or prices.index.hasnans:
        raise ValueError("invalid price dates")
    if prices.index.tz is not None:
        prices.index = prices.index.tz_localize(None)
    if (prices.index != prices.index.normalize()).any() or prices.index.has_duplicates:
        raise ValueError("expected unique daily date rows")
    if set(symbols) - set(prices.columns):
        raise ValueError(f"missing series: {sorted(set(symbols) - set(prices.columns))}")
    prices = prices.loc[prices.index < pd.Timestamp(cutoff), symbols].sort_index()
    sessions = prices.index[prices[config.benchmark].notna()][-(252 + config.history_days + 65):]
    if sessions.empty:
        raise ValueError("benchmark has no completed sessions")
    # Ignore earlier unused history, but retain holes inside and after this window.
    prices = prices.loc[prices.index >= sessions[0]]
    # A missing benchmark bar amid stock observations is a hole, not a holiday.
    holes = prices[config.benchmark].isna() & prices[list(config.symbols)].notna().any(axis=1)
    if holes.any():
        raise ValueError(f"benchmark has missing sessions: {prices.index[holes][0].date()}")
    # Benchmark dates are the session grid. Never forward-fill missing stock bars.
    prices = prices.loc[prices[config.benchmark].notna()]
    if len(prices) < 252 + config.history_days + 61:
        raise ValueError("insufficient completed sessions for lookback, risk warmup and evaluation")
    bad = [s for s in symbols if not np.isfinite(prices[s].to_numpy(dtype=float)).all()
           or (prices[s] <= 0).any()]
    if bad:
        raise ValueError(f"missing/nonpositive/nonfinite adjusted closes: {', '.join(bad)}")
    if (cutoff - prices.index[-1].date()).days > 7:
        raise ValueError("source is more than seven calendar days behind; refusing stale refresh")
    return prices.astype(float), source
