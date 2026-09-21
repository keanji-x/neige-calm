"""Interactive owner-only surface; never registered as an agent tool."""
import argparse
import json
from pathlib import Path
import sys

from .broker import Broker
from .config import Config
from .engine import Engine


def main():
    parser = argparse.ArgumentParser(description="Review a native Longbridge paper order and explicitly confirm it.")
    parser.add_argument("--config", type=Path, required=True, help="Owner-managed plugin configuration JSON")
    parser.add_argument("--data-dir", type=Path, required=True, help="Same private data directory used by the plugin")
    parser.add_argument("--decision", required=True)
    parser.add_argument("--cancel", action="store_true", help="Cancel the decision's owned order instead of submitting")
    args = parser.parse_args()
    if not sys.stdin.isatty() or not sys.stdout.isatty():
        parser.error("interactive terminal required; agents must not pipe confirmation codes")
    config = Config.parse(json.loads(args.config.read_text()))
    engine = Engine(args.data_dir, config, Broker(config.cli_path, config.broker_home, str(args.data_dir)))
    try:
        request, preview = engine.operator_preview(args.decision, args.cancel)
        print(preview, flush=True)
        code = input("Review the native preview. Enter its confirmation code to proceed, or leave blank to stop: ").strip()
        if not code:
            print("No order action submitted.")
            return
        result = engine.operator_confirm(args.decision, request, code, args.cancel)
        print(json.dumps({"error": result["error"], "decisions": result["decisions"]}, indent=2))
    except Exception as error:
        print(str(error) if isinstance(error, ValueError) else "Operation failed; reconcile before retrying.", file=sys.stderr)
        raise SystemExit(1) from None


if __name__ == "__main__":
    main()
