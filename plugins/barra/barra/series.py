"""Research-result time series for Neige's existing chart.series protocol."""
from datetime import date, datetime, time, timezone
import math
import statistics

ASSETS = {
    "BARRA:Beta": "beta",
    "BARRA:Momentum": "momentum",
    "BARRA:ResidualVol": "residual_volatility",
    "BARRA:Forecast": "forecast",
    "BARRA:Realized21D": "realized",
    "BARRA:EqualWeight": "equity",
}


def parse_day(value):
    if not isinstance(value, str):
        raise ValueError("dates must be YYYY-MM-DD strings")
    parsed = date.fromisoformat(value)
    if parsed.isoformat() != value:
        raise ValueError("dates must be YYYY-MM-DD strings")
    return parsed


def realized_volatility(history, end):
    if end < 20:
        return None
    return statistics.stdev(row["portfolio_return"] for row in history[end - 20:end + 1]) * math.sqrt(252) * 100


def chart_series(result, args, now_ms):
    required = {"series", "fields", "period", "mode", "start", "as_of", "deadline_ms"}
    if not isinstance(args, dict) or set(args) != required:
        raise ValueError("chart request must provide series, fields, period, mode, start, as_of, deadline_ms")
    assets = args["series"]
    if not isinstance(assets, list) or not 1 <= len(assets) <= 8 or any(not isinstance(a, str) for a in assets):
        raise ValueError("series must contain 1 to 8 names")
    if len(set(assets)) != len(assets):
        raise ValueError("duplicate series")
    if args["fields"] != ["close"] or args["period"] != "day":
        raise ValueError("research charts support daily close-valued series only; these are metrics, not market OHLC")
    if args["mode"] not in ("live", "frozen"):
        raise ValueError("mode must be live or frozen")
    if type(args["deadline_ms"]) is not int or args["deadline_ms"] <= now_ms:
        raise ValueError("chart request deadline expired")
    start, end = parse_day(args["start"]), parse_day(args["as_of"])
    if start > end:
        raise ValueError("start must not follow as_of")
    output = []
    for asset in assets:
        entry = {"asset": asset}
        if asset not in ASSETS:
            output.append(entry | {"status": "unknown_asset", "reason": "unknown Barra research series"})
            continue
        if result is None:
            output.append(entry | {"status": "unavailable", "reason": "Waiting for the first successful study result"})
            continue
        complete = parse_day(result["as_of"])
        history = result["history"]
        points = []
        cumulative = 0.0
        field = ASSETS[asset]
        for index, row in enumerate(history):
            day = parse_day(row["date"])
            if day < start or day > end or (args["mode"] == "frozen" and day >= complete):
                continue
            if field in ("beta", "momentum", "residual_volatility"):
                cumulative += row[field] * 100
                value = cumulative
            elif field == "forecast":
                value = row["predicted_daily_volatility"] * math.sqrt(252) * 100
            elif field == "realized":
                value = realized_volatility(history, index)
            else:
                value = row["equal_weight_equity"] * 100
            if value is not None:
                if not math.isfinite(value):
                    raise ValueError("nonfinite research chart value")
                stamp = int(datetime.combine(day, time(), timezone.utc).timestamp() * 1000)
                points.append([stamp, value])
        if len(points) < 2:
            output.append(entry | {"status": "unavailable", "reason": "Fewer than two observations in this window"})
        else:
            output.append(entry | {"status": "ok", "complete_through": result["as_of"], "points": points})
    return {"series": output}
