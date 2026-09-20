"""Run the identical calculation outside Neige for inspection and replay."""
import argparse
from pathlib import Path

from .config import Config
from .data import load_prices
from .model import calculate
from .report import result_tables
from .runtime import atomic_json, atomic_text, utc_now


def main():
    parser = argparse.ArgumentParser(description="Barra-style US price-factor research, NOT MSCI Barra")
    parser.add_argument("--symbols", help="Comma-separated US tickers; 12 to 40")
    parser.add_argument("--benchmark", default="SPY")
    parser.add_argument("--history-days", type=int, default=126)
    parser.add_argument("--prices-csv", default="", help="Explicit offline adjusted-close replay source")
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    config_args = {"benchmark": args.benchmark, "history_days": args.history_days}
    if args.symbols:
        config_args["symbols"] = args.symbols.split(",")
    config = Config.parse(config_args)
    prices, source = load_prices(config, utc_now(), args.prices_csv)
    result = calculate(prices, config, source)
    atomic_text(args.output / "prices.csv", prices.to_csv(float_format="%.12g", index_label="date"))
    atomic_json(args.output / "result.json", result)
    atomic_json(args.output / "tables.json", result_tables(result))
    print(f"{result['model']}: {result['as_of']}, run {result['run_id']}, output {args.output}")


if __name__ == "__main__":
    main()
