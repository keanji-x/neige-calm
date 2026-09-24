#!/usr/bin/env python3
"""Refuse a dev stack that runs with autologin and publishes a port beyond loopback.

Reads `docker compose config --format json` on stdin: the configuration compose will actually run,
after its own `.env` parsing, quoting and `${VAR:-default}` interpolation. Deciding from that, and
not from make's reading of the same variables, is the point: make and compose parse `.env`
differently, and only compose's reading reaches the containers.

Autologin makes every request the owner, and the server container mounts the host home, so with
autologin on every published port of every service must be bound to 127.0.0.1 or ::1. A port with
no host IP is published on every interface. Anything unreadable fails closed.
"""

import json
import sys

LOOPBACK = {"127.0.0.1", "::1"}
# calm-server's bool parse for CALM_DEV_AUTOLOGIN: anything else turns autologin on.
FALSEY = {"", "0", "n", "no", "f", "false", "off"}


def refuse(lines):
    for line in lines:
        print(line, file=sys.stderr)
    sys.exit(1)


def main():
    try:
        config = json.load(sys.stdin)
        services = config["services"]
        environment = services["server"].get("environment") or {}
    except (ValueError, KeyError, TypeError, AttributeError) as error:
        refuse([f"Refusing: could not read the resolved compose config ({error})."])

    value = environment.get("CALM_DEV_AUTOLOGIN")
    if value is None or str(value).strip().lower() in FALSEY:
        return

    exposed = []
    for name, service in sorted(services.items()):
        for port in service.get("ports") or []:
            host_ip = port.get("host_ip") if isinstance(port, dict) else None
            if host_ip not in LOOPBACK:
                published = port.get("published") if isinstance(port, dict) else port
                exposed.append(f"{name} port {published} on {host_ip or 'every interface'}")
    if exposed:
        refuse([
            f"Refusing: CALM_DEV_AUTOLOGIN={value} makes every request the owner, but compose would publish",
            *(f"  {entry}" for entry in exposed),
            "Set CALM_PUBLISH_ADDR=127.0.0.1 (or [::1]) to publish on loopback only, or turn autologin off.",
        ])


if __name__ == "__main__":
    main()
