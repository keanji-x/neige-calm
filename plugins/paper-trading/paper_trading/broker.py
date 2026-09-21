"""Bounded, supervised Longbridge CLI transport; no retries or automatic approval.

Schema evidence: installed CLI --help/--schema inspected without credentials on
2026-09-21. `assets`, `quote`, `intraday`, and `order executions` declare arrays
of objects. Quotes have no schema-proven timestamp; intraday rows have `time`.
`positions` and `order` use a string-typed schema whose description specifies AP
(HK/CN) arrays, versus US-account objects. Only the AP array contract is accepted
here: US-listed instruments do not imply a US-region account. No object envelope
is silently flattened. JSON fractional numbers decode as Decimal, preserving
the exact broker numeric token instead of rounding through a binary float.

`auth status --schema` declares token/object and account/unspecified, not their
nested fields. We strictly REQUIRE token.status == valid, token.dc_region == ap,
and account fields account_channel == lb_papertrading and account_no == the
configured string.
Those nested fields are a required integration assumption, not a schema-proven
guarantee. Missing/different metadata fails closed. `order detail` describes an
object despite its string-typed schema; `order buy` declares an object. `order
cancel` declares a status string, returned as {"message": <native string>}.

The caller must check identity before each snapshot or trading operation and
validate financial fields. Only the interactive operator may call execute().
Preview text (including any native confirmation) must not go to agent tools.
"""

import json
from decimal import Decimal
import os
import re
import selectors
import signal
import subprocess
import time


class BrokerError(RuntimeError):
    """Safe public error; never contains broker output, arguments or credentials."""


MAX_OUTPUT_BYTES = 1024 * 1024
_ENV_KEYS = (
    "PATH", "LANG", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY",
    "http_proxy", "https_proxy", "all_proxy", "no_proxy",
)


def _argument(value: str) -> str:
    if not isinstance(value, str) or not value or "\0" in value:
        raise BrokerError("Invalid broker argument")
    return value


def order_args(symbol: str, side: str, quantity: int, price: str, remark: str) -> list[str]:
    """Construct the supervised regular-session, day limit-order request."""
    if not isinstance(side, str) or side.lower() not in ("buy", "sell"):
        raise BrokerError("Invalid order side")
    if type(quantity) is not int or quantity <= 0:
        raise BrokerError("Invalid order quantity")
    return [
        "order", side.lower(), _argument(symbol), str(quantity),
        "--price", _argument(price), "--order-type", "LO", "--tif", "day",
        "--outside-rth", "RTH_ONLY", "--remark", _argument(remark), "--format", "json",
    ]


def cancel_args(order_id: str) -> list[str]:
    return ["order", "cancel", _argument(order_id), "--format", "json"]


def _object(value: object) -> dict:
    if not isinstance(value, dict):
        raise BrokerError("Unexpected broker response shape")
    return value


def _records(value: object) -> list:
    if not isinstance(value, list) or any(not isinstance(row, dict) for row in value):
        raise BrokerError("Unexpected broker response shape")
    return value


def _json_object(pairs: list) -> dict:
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("Duplicate JSON key")
        result[key] = value
    return result


def _invalid_constant(value: str) -> None:
    raise ValueError("Non-JSON constant")


class Broker:
    def __init__(self, executable: str, home: str, workdir: str, timeout_seconds: int = 20):
        for path in (executable, home, workdir):
            if not os.path.isabs(_argument(path)):
                raise BrokerError("Broker paths must be absolute")
        if type(timeout_seconds) is not int or timeout_seconds <= 0:
            raise BrokerError("Invalid broker timeout")
        self.executable = executable
        self.home = home
        self.workdir = workdir
        self.timeout_seconds = timeout_seconds

    order_args = staticmethod(order_args)
    cancel_args = staticmethod(cancel_args)

    def _run(self, argv: list[str]) -> str:
        env = {key: os.environ[key] for key in _ENV_KEYS if key in os.environ}
        env["HOME"] = self.home
        deadline = time.monotonic() + self.timeout_seconds
        try:
            child = subprocess.Popen(
                [self.executable, *argv], cwd=self.workdir, env=env,
                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                shell=False, start_new_session=True,
            )
        except (OSError, ValueError):
            raise BrokerError("Could not start broker CLI") from None

        output = bytearray()
        total = 0
        try:
            # Drain both pipes concurrently, but retain only stdout. Bound their
            # combined size, so an unbounded stderr stream cannot exhaust memory.
            with selectors.DefaultSelector() as selector:
                for stream in (child.stdout, child.stderr):
                    os.set_blocking(stream.fileno(), False)
                    selector.register(stream, selectors.EVENT_READ)
                while selector.get_map():
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise BrokerError("Broker CLI timed out; outcome may be unknown")
                    for key, _ in selector.select(remaining):
                        chunk = os.read(key.fd, 65536)
                        if not chunk:
                            selector.unregister(key.fileobj)
                            continue
                        total += len(chunk)
                        if total > MAX_OUTPUT_BYTES:
                            raise BrokerError("Broker CLI output limit exceeded; outcome may be unknown")
                        if key.fileobj is child.stdout:
                            output.extend(chunk)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise BrokerError("Broker CLI timed out; outcome may be unknown")
            if child.wait(timeout=remaining) != 0:
                raise BrokerError("Broker CLI failed; outcome may be unknown")
            return output.decode("utf-8")
        except subprocess.TimeoutExpired:
            raise BrokerError("Broker CLI timed out; outcome may be unknown") from None
        except (OSError, UnicodeError, ValueError):
            raise BrokerError("Invalid broker CLI output; outcome may be unknown") from None
        finally:
            # A descendant can hold a pipe after the CLI exits. Kill the session's
            # process group on every path, including an already-exited leader.
            try:
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                child.wait(timeout=1)
            except (OSError, subprocess.TimeoutExpired):
                raise BrokerError("Could not stop broker CLI; outcome may be unknown") from None
            finally:
                child.stdout.close()
                child.stderr.close()

    def _json(self, argv: list[str]) -> object:
        raw = self._run(argv)
        try:
            return json.loads(raw, object_pairs_hook=_json_object, parse_constant=_invalid_constant,
                              parse_float=Decimal)
        except (ValueError, RecursionError):
            raise BrokerError("Invalid broker JSON response; outcome may be unknown") from None

    def identity(self, expected_account: str) -> dict:
        _argument(expected_account)
        response = _object(self._json(["auth", "status", "--format", "json"]))
        token = _object(response.get("token"))
        account = _object(response.get("account"))
        if (token.get("status") != "valid"
                or token.get("dc_region") != "ap"
                or account.get("account_channel") != "lb_papertrading"
                or account.get("account_no") != expected_account):
            raise BrokerError("Broker paper-account identity check failed")
        return response

    def assets(self) -> list:
        return _records(self._json(["assets", "--format", "json"]))

    def positions(self) -> list:
        return _records(self._json(["positions", "--format", "json"]))

    def quote(self, symbol: str) -> dict:
        rows = _records(self._json(["quote", _argument(symbol), "--format", "json"]))
        matches = [row for row in rows if row.get("symbol") == symbol]
        if len(matches) != 1:
            raise BrokerError("Broker quote must contain exactly one matching symbol")
        return matches[0]

    def orders(self, start: str | None = None) -> list:
        args = ["order"]
        if start is not None:
            args += ["--history", "--start", _argument(start)]
        return _records(self._json([*args, "--format", "json"]))

    def intraday(self, symbol: str) -> list:
        return _records(self._json([
            "intraday", _argument(symbol), "--session", "intraday", "--format", "json",
        ]))

    def executions(self, start: str | None = None) -> list:
        args = ["order", "executions"]
        if start is not None:
            args += ["--history", "--start", _argument(start)]
        return _records(self._json([*args, "--format", "json"]))

    def order_detail(self, order_id: str) -> dict:
        return _object(self._json(["order", "detail", _argument(order_id), "--format", "json"]))

    @staticmethod
    def _preview_args(argv: list[str]) -> list[str]:
        if not isinstance(argv, list) or len(argv) < 3:
            raise BrokerError("Invalid broker preview command")
        args = [_argument(arg) for arg in argv]
        if args[0] != "order" or args[1] not in ("buy", "sell", "cancel"):
            raise BrokerError("Invalid broker preview command")
        if any(arg.startswith("--exec") for arg in args):
            raise BrokerError("Execution flags are forbidden in preview arguments")
        return args

    def preview(self, argv: list[str]) -> str:
        """Return native stdout verbatim; never parse or redeem its code."""
        return self._run(self._preview_args(argv))

    def execute(self, argv: list[str], code: str) -> dict:
        """Operator-only explicit redemption, once; the caller owns reconciliation."""
        if not isinstance(code, str) or re.fullmatch(r"[0-9]{3}", code) is None:
            raise BrokerError("Invalid confirmation code")
        args = self._preview_args(argv)
        response = self._json([*args, "--execute", code])
        if args[1] == "cancel":
            if not isinstance(response, str) or not response:
                raise BrokerError("Unexpected broker cancellation response shape")
            return {"message": response}
        return _object(response)
