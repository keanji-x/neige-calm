"""Bounded official-SDK subprocess for invest paper execution; no retries. Reused unchanged from the
paper plugin (`allocation_broker.py`) except for the multi-symbol snapshot request.

Each call runs `sdk_bridge.py` under the configured SDK interpreter with typed
configuration and one exact operation. The child gets an explicit environment:
the PATH/LANG/proxy allowlist with HOME pinned to the broker home, so inherited
Longbridge endpoint overrides, model keys and import paths never reach it. The
child's process group is killed on every path, which also stops descendants that
stay in that group. The combined stdout and stderr size is bounded; only stdout
is kept. JSON fractional numbers decode as Decimal, preserving the exact broker
numeric token instead of rounding through a binary float; duplicate keys and
non-JSON constants are refused. A failed, timed-out or malformed call raises
BrokerError, whose message never contains output, arguments or credentials, and
is never retried: once a submission may have started, its outcome stays unknown
for reconciliation.
"""
import json
from decimal import Decimal
import os
from pathlib import Path
import selectors
import signal
import subprocess
import time


class BrokerError(RuntimeError):
    """Safe public error; never contains broker output, arguments or credentials."""


class OrderNotSubmitted(BrokerError):
    """The trusted SDK bridge proved the broker write was never invoked."""


MAX_OUTPUT_BYTES = 1024 * 1024
_ENV_KEYS = (
    "PATH", "LANG", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY",
    "http_proxy", "https_proxy", "all_proxy", "no_proxy",
)


def _argument(value: str) -> str:
    if not isinstance(value, str) or not value or "\0" in value:
        raise BrokerError("Invalid broker argument")
    return value


def _object(value: object) -> dict:
    if not isinstance(value, dict):
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
    def __init__(self, config, workdir, timeout_seconds=30):
        for path in (config.sdk_python_path, config.broker_home, workdir):
            if not os.path.isabs(_argument(path)):
                raise BrokerError("Broker paths must be absolute")
        if type(timeout_seconds) is not int or timeout_seconds <= 0:
            raise BrokerError("Invalid broker timeout")
        self.config = config
        self.workdir = workdir
        self.timeout_seconds = timeout_seconds

    def _run(self, argv: list[str]) -> str:
        env = {key: os.environ[key] for key in _ENV_KEYS if key in os.environ}
        env["HOME"] = self.config.broker_home
        deadline = time.monotonic() + self.timeout_seconds
        try:
            child = subprocess.Popen(
                [self.config.sdk_python_path, *argv], cwd=self.workdir, env=env,
                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                shell=False, start_new_session=True,
            )
        except (OSError, ValueError):
            raise BrokerError("Could not start broker SDK process") from None

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
                        raise BrokerError("Broker SDK process timed out; outcome may be unknown")
                    for key, _ in selector.select(remaining):
                        chunk = os.read(key.fd, 65536)
                        if not chunk:
                            selector.unregister(key.fileobj)
                            continue
                        total += len(chunk)
                        if total > MAX_OUTPUT_BYTES:
                            raise BrokerError("Broker SDK process output limit exceeded; outcome may be unknown")
                        if key.fileobj is child.stdout:
                            output.extend(chunk)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise BrokerError("Broker SDK process timed out; outcome may be unknown")
            if child.wait(timeout=remaining) != 0:
                raise BrokerError("Broker SDK process failed; outcome may be unknown")
            return output.decode("utf-8")
        except subprocess.TimeoutExpired:
            raise BrokerError("Broker SDK process timed out; outcome may be unknown") from None
        except (OSError, UnicodeError, ValueError):
            raise BrokerError("Invalid broker SDK output; outcome may be unknown") from None
        finally:
            # A descendant can hold a pipe after the SDK process exits. Kill the session's
            # process group on every path, including an already-exited leader.
            try:
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                child.wait(timeout=1)
            except (OSError, subprocess.TimeoutExpired):
                raise BrokerError("Could not stop broker SDK process; outcome may be unknown") from None
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

    def request(self, method, body):
        # Typed configuration and exact operations only; no model-selected paths or endpoints.
        args = ['-I', str(Path(__file__).with_name('sdk_bridge.py')),
                '--access-region', self.config.access_region,
                '--client-id', self.config.oauth_client_id, '--account', self.config.account_no,
                '--cash-buffer-bps', str(self.config.cash_buffer_bps),
                '--max-order-bps', str(self.config.max_order_bps),
                '--quote-max-age-seconds', str(self.config.quote_max_age_seconds),
                method, '--request', json.dumps(body, allow_nan=False)]
        return _object(self._json(args))

    def snapshot(self, since, symbols):
        return self.request('snapshot', {'since': since, 'symbols': symbols})

    def submit(self, request):
        result = self.request('submit', request)
        if result == {'status': 'not_submitted'}:
            raise OrderNotSubmitted('SDK preflight refused before sending an order')
        if not isinstance(result.get('order_id'), str) or not result['order_id']:
            raise BrokerError('SDK returned no broker order identity; outcome may be unknown')
        return result['order_id']
