"""Interactive owner-only surface; never registered as an agent tool."""
import argparse
import json
from pathlib import Path
import sys

from .broker import Broker
from .config import AccountConfig
from .strategy import Portfolio


def main():
    parser = argparse.ArgumentParser(description="Review a native Longbridge paper order and explicitly confirm it.")
    parser.add_argument("--config", type=Path, required=True, help="Owner-managed account connection JSON")
    parser.add_argument("--data-dir", type=Path, required=True, help="Same private data directory used by the plugin")
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--decision")
    action.add_argument("--approve-strategy", metavar="REVISION", help="Approve the exact proposed strategy snapshot")
    action.add_argument("--import-legacy", type=Path, metavar="CONFIG", help="Import a saved v0.1 full configuration")
    parser.add_argument("--cancel", action="store_true", help="Cancel the decision's owned order instead of submitting")
    args = parser.parse_args()
    if args.cancel and not args.decision:
        parser.error("--cancel requires --decision")
    if not sys.stdin.isatty() or not sys.stdout.isatty():
        parser.error("interactive terminal required; agents must not pipe confirmation codes")
    try:
        config = AccountConfig.parse(json.loads(args.config.read_text()))
        portfolio = Portfolio(args.data_dir, config, Broker(config.cli_path, config.broker_home, str(args.data_dir)))
        if args.approve_strategy or args.import_legacy:
            legacy = json.loads(args.import_legacy.read_text()) if args.import_legacy else None
            preview = (portfolio.legacy_preview(legacy) if legacy is not None else
                       portfolio.approval_preview(args.approve_strategy))
            print(json.dumps(preview, indent=2), flush=True)
            word = 'IMPORT' if legacy is not None else 'APPROVE'
            answer = input(f"Review the account, Track and every limit. Type {word} to confirm, or leave blank to stop: ").strip()
            if answer != word:
                print("No strategy change approved.")
                return
            result = (portfolio.import_legacy(legacy) if legacy is not None else
                      portfolio.approve(args.approve_strategy))
            print(json.dumps({'approved_strategy': result, 'order_submitted': False}, indent=2))
            return
        request, preview = portfolio.operator_preview(args.decision, args.cancel)
        print(json.dumps({'approved_strategy_revision': request['strategy_revision']}, indent=2), flush=True)
        print(preview, flush=True)
        code = input("Review the native preview. Enter its confirmation code to proceed, or leave blank to stop: ").strip()
        if not code:
            print("No order action submitted.")
            return
        result = portfolio.operator_confirm(args.decision, request, code, args.cancel)
        print(json.dumps({"error": result["error"], "decisions": result["decisions"]}, indent=2))
    except Exception as error:
        print(str(error) if isinstance(error, ValueError) else "Operation failed; reconcile before retrying.", file=sys.stderr)
        raise SystemExit(1) from None


if __name__ == "__main__":
    main()
