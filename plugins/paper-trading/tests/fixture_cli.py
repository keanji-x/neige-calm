#!/usr/bin/python3
"""Deterministic broker boundary: prescribed records, not strategy/accounting logic."""
import json
import os
from pathlib import Path
import sys

root = Path(os.environ["HOME"])
path = root / "broker.json"
state = json.loads(path.read_text())
args = sys.argv[1:]
with (root / "calls.jsonl").open("a") as stream:
    stream.write(json.dumps(args) + "\n")

def emit(value):
    print(json.dumps(value))

if args[:2] == ["auth", "status"]:
    emit(state["identity"])
elif args[0] == "assets":
    emit(state["assets"])
elif args[0] == "positions":
    emit(state["positions"])
elif args[0] == "quote":
    emit([state["quotes"][args[1]]])
elif args[0] == "intraday":
    emit(state["intraday"][args[1]])
elif args[:2] == ["order", "detail"]:
    emit(state["orders"][args[2]])
elif args[:2] == ["order", "executions"]:
    emit(state["fills"])
elif len(args) >= 2 and args[0] == "order" and args[1] in ("buy", "sell", "cancel"):
    if "--execute" not in args:
        emit({"preview": args, "confirmation_code": "731"})
    elif args[args.index("--execute") + 1] != "731":
        sys.exit(2)
    else:
        response = state["next_response"]
        if state.get("publish_on_submit"):
            order = state["publish_on_submit"]
            state["orders"][order["order_id"]] = order
            path.write_text(json.dumps(state))
        if state.get("fail_after_submit"):
            sys.exit(3)
        emit(response)
elif args[0] == "order":
    emit(list(state["orders"].values()))
else:
    sys.exit(4)
