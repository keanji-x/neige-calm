#!/usr/bin/python3
"""Controlled Tailscale CLI fixture; never changes networking or a real daemon."""
import json
import os
from pathlib import Path
import sys
import time

config = Path(sys.argv[1].removeprefix("--socket="))
options = json.loads(config.read_text())
record = config.with_suffix(".record")
lease = config.with_suffix(".lease")
args = sys.argv[2:]
host = "pair.example.ts.net"

if args == ["status", "--json"]:
    print(json.dumps({"BackendState": "Running", "Self": {"DNSName": host + "."}}))
elif args == ["serve", "status", "--json"]:
    if options.get("occupied"):
        print(json.dumps({"TCP": {"10000": {"HTTPS": True}}}))
    elif lease.exists():
        current = json.loads(lease.read_text())
        try:
            os.kill(current["pid"], 0)
            port = str(current["port"])
            print(json.dumps({"Foreground": {"fixture": {
                "TCP": {port: {"HTTPS": True}},
                "Web": {host + ":" + port: {"Handlers": {"/": {"Proxy": current["target"]}}}},
                "AllowFunnel": {host + ":" + port: True},
            }}}))
        except ProcessLookupError:
            print("{}")
    else:
        print("{}")
elif args[0] == "funnel":
    port = int(args[1].removeprefix("--https="))
    record.write_text(json.dumps({"args": args, "environmentKeys": sorted(os.environ), "target": args[2]}))
    if options.get("fail"):
        sys.exit(1)
    pending = lease.with_suffix(".tmp")
    pending.write_text(json.dumps({"pid": os.getpid(), "port": port, "target": args[2]}))
    pending.replace(lease)
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline and not config.with_suffix(".exit").exists():
        time.sleep(0.05)
else:
    sys.exit(2)
