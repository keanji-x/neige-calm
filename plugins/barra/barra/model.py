"""Price-style subset: lagged cross-sectional OLS and expanding risk backtest."""
import hashlib
import json

import numpy as np
import pandas as pd

FACTORS = ("market", "beta", "momentum", "residual_volatility")
LOOKBACK = 252
RISK_WARMUP = 60
LIMITATIONS = (
    "Price-only Barra-style research subset, NOT the MSCI Barra model. "
    "No size, value, industry, fundamentals or official descriptors. "
    "Fixed user-selected universe: survivorship/selection bias; not a historical index. "
    "Adjusted history is today's vintage, not point-in-time archived market data. "
    "Equal-weight cross-sectional OLS; risk assumes diagonal specific covariance. "
    "Risk backtest is out-of-sample by date; attribution R-squared is in-sample. "
    "No trading strategy, transaction costs, executable returns or investment recommendation."
)


def standardized(values):
    values = np.asarray(values, dtype=float)
    scale = values.std(ddof=0)
    if not np.isfinite(values).all() or scale < 1e-10:
        raise ValueError("degenerate cross-sectional descriptor")
    clipped = np.clip(values, values.mean() - 3 * scale, values.mean() + 3 * scale)
    return (clipped - clipped.mean()) / clipped.std(ddof=0)


def exposures(prices, symbols, benchmark, position):
    window = prices.iloc[position - LOOKBACK:position + 1]
    if len(window) != LOOKBACK + 1:
        raise ValueError("252 return observations required")
    returns = window.pct_change(fill_method=None).iloc[1:]
    m = returns[benchmark].to_numpy()
    y = returns[list(symbols)].to_numpy()
    centered_m = m - m.mean()
    market_ss = centered_m @ centered_m
    if market_ss < 1e-12:
        raise ValueError("benchmark has no variance")
    beta = centered_m @ (y - y.mean(axis=0)) / market_ss
    residual = y - y.mean(axis=0) - centered_m[:, None] * beta
    rv = np.sqrt((residual ** 2).sum(axis=0) / (LOOKBACK - 2)) * np.sqrt(252)
    momentum = np.log(window.iloc[-22][list(symbols)].to_numpy()
                      / window.iloc[0][list(symbols)].to_numpy())
    x = np.column_stack([np.ones(len(symbols)), standardized(beta),
                         standardized(momentum), standardized(rv)])
    return x, np.column_stack([beta, momentum, rv])


def risk_covariance(factors, residuals):
    # Exposure estimation consumes parameters: correct the average residual variance.
    n = residuals.shape[1]
    covariance = np.cov(factors, rowvar=False, ddof=1)
    specific = np.var(residuals, axis=0, ddof=1) * n / (n - len(FACTORS))
    return covariance, specific


def calculate(prices, config, source):
    symbols = list(config.symbols)
    required = [*symbols, config.benchmark]
    if list(prices.columns) != required or prices.index.has_duplicates or not prices.index.is_monotonic_increasing:
        raise ValueError("prices must have ordered configuration columns and unique ascending dates")
    if not np.isfinite(prices.to_numpy()).all() or (prices <= 0).any().any():
        raise ValueError("prices must be finite and positive")
    if len(prices) < LOOKBACK + config.history_days + RISK_WARMUP + 1:
        raise ValueError("insufficient model history")
    returns = prices[symbols].pct_change(fill_method=None).to_numpy()
    weights = np.full(len(symbols), 1 / len(symbols))
    factor_returns, specific_returns, rows = [], [], []
    for t in range(LOOKBACK + 1, len(prices)):
        # Today's return must be explained with YESTERDAY's observable exposure.
        x, _ = exposures(prices, symbols, config.benchmark, t - 1)
        y = returns[t]
        f, _, rank, singular = np.linalg.lstsq(x, y, rcond=None)
        if rank != len(FACTORS) or singular[0] / singular[-1] > 1e6:
            raise ValueError(f"ill-conditioned factor regression at {prices.index[t].date()}")
        e = y - x @ f
        denom = ((y - y.mean()) ** 2).sum()
        if denom <= 1e-14:
            raise ValueError("cross-sectional return variance is zero")
        row = {"date": prices.index[t].date().isoformat(),
               **dict(zip(FACTORS, f.tolist())),
               "r_squared": float(1 - e @ e / denom),
               "portfolio_return": float(weights @ y)}
        if len(factor_returns) >= RISK_WARMUP:
            fcov, svar = risk_covariance(np.array(factor_returns[-252:]), np.array(specific_returns[-252:]))
            exposure = weights @ x
            predicted_var = float(exposure @ fcov @ exposure + (weights ** 2) @ svar)
            if predicted_var <= 0:
                raise ValueError("nonpositive predicted variance")
            row["predicted_daily_volatility"] = float(np.sqrt(predicted_var))
        factor_returns.append(f)
        specific_returns.append(e)
        rows.append(row)
    history = [r for r in rows if "predicted_daily_volatility" in r][-config.history_days:]
    if len(history) < config.history_days:
        raise ValueError("insufficient out-of-sample risk observations")
    latest_x, descriptors = exposures(prices, symbols, config.benchmark, len(prices) - 1)
    fcov, svar = risk_covariance(np.array(factor_returns[-252:]), np.array(specific_returns[-252:]))
    pex = weights @ latest_x
    common_var = float(pex @ fcov @ pex)
    specific_var = float((weights ** 2) @ svar)
    realized = np.array([r["portfolio_return"] for r in history])
    predicted = np.array([r["predicted_daily_volatility"] for r in history])
    z = realized / predicted
    cumulative = np.cumprod(1 + realized)
    peaks = np.maximum.accumulate(np.r_[1.0, cumulative])[1:]
    for row, equity in zip(history, cumulative):
        row["equal_weight_equity"] = float(equity)
    snapshot = prices.to_csv(float_format="%.12g", index_label="date")
    data_hash = hashlib.sha256(snapshot.encode()).hexdigest()
    config_hash = hashlib.sha256(json.dumps(config.json(), sort_keys=True).encode()).hexdigest()
    from . import VERSION

    return {
        "model": "Barra-style US price subset", "version": VERSION,
        "as_of": prices.index[-1].date().isoformat(), "source": source,
        "data_sha256": data_hash, "config_sha256": config_hash,
        "run_id": hashlib.sha256((data_hash + config_hash + VERSION).encode()).hexdigest()[:16],
        "config": config.json(), "limitations": LIMITATIONS,
        "exposures": [dict(symbol=s, beta=float(descriptors[i, 0]),
                           momentum=float(descriptors[i, 1]), residual_volatility=float(descriptors[i, 2]),
                           beta_z=float(latest_x[i, 1]), momentum_z=float(latest_x[i, 2]),
                           residual_volatility_z=float(latest_x[i, 3])) for i, s in enumerate(symbols)],
        "factor_names": list(FACTORS), "daily_factor_covariance": fcov.tolist(),
        "daily_specific_variance": dict(zip(symbols, svar.tolist())),
        "portfolio": {"construction": "daily equal-weight rebalanced, gross, zero costs",
                      "exposures": dict(zip(FACTORS, pex.tolist())),
                      "predicted_annual_volatility": float(np.sqrt(common_var + specific_var) * np.sqrt(252)),
                      "common_variance_fraction": common_var / (common_var + specific_var)},
        "validation": {"observations": len(history), "rms_standardized_return": float(np.sqrt(np.mean(z ** 2))),
                       "within_1_96_sigma": float(np.mean(np.abs(z) <= 1.96)),
                       "realized_annual_volatility": float(realized.std(ddof=1) * np.sqrt(252)),
                       "gross_cumulative_return": float(cumulative[-1] - 1),
                       "max_drawdown": float((cumulative / peaks - 1).min()),
                       "mean_attribution_r_squared": float(np.mean([r["r_squared"] for r in history]))},
        "history": history,
    }
