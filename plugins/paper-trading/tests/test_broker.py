"""Drive the production SDK subprocess runner through AllocationBroker.request.

The executable is a deterministic fixture standing in for the SDK interpreter.
"""

import dataclasses
import json
from decimal import Decimal
import os
from pathlib import Path
import sys
import time
import traceback

import pytest

from paper_trading.allocation_broker import AllocationBroker, BrokerError, MAX_OUTPUT_BYTES
from paper_trading.allocation_config import AllocationConfig

# Argv secrets: the account number and OAuth client ID reach the child as arguments.
ACCOUNT_NO = "SECRET-ACCOUNT-7731"
OAUTH_CLIENT_ID = "secret-oauth-client-4417"


# This fixture only transports prescribed bytes and records process inputs. It
# deliberately implements no broker parsing, identity or order behavior.
FIXTURE = r'''
import json
import os
from pathlib import Path
import sys
import time

root = Path.cwd()
config = json.loads((root / "response.json").read_text())
with (root / "calls.jsonl").open("a") as log:
    log.write(json.dumps({"argv": sys.argv[1:], "env": dict(os.environ),
                          "cwd": str(root), "stdin": sys.stdin.read(),
                          "pid": os.getpid()}) + "\n")
if config.get("fork"):
    pid = os.fork()
    if pid == 0:
        time.sleep(30)
        os._exit(0)
    (root / "descendant.pid").write_text(str(pid))
    sys.exit(0)
if config.get("close_pipes"):
    os.close(1)
    os.close(2)
time.sleep(config.get("sleep", 0))
if not config.get("close_pipes"):
    for fd, field in ((1, "stdout"), (2, "stderr")):
        data = config.get(field, "").encode("utf-8")
        if config.get("invalid_utf8") and fd == 1:
            data = b"\xff"
        for _ in range(config.get("repeat", 1)):
            remaining = data
            while remaining:
                remaining = remaining[os.write(fd, remaining):]
sys.exit(config.get("exit", 0))
'''


def config_for(home, executable):
    return AllocationConfig.parse({
        "profile": "spy_cash", "account_no": ACCOUNT_NO, "broker_home": str(home),
        "owner_track_id": "owner", "oauth_client_id": OAUTH_CLIENT_ID,
        "sdk_python_path": str(executable)})


@pytest.fixture
def harness(tmp_path):
    workdir = tmp_path / "data"
    home = tmp_path / "home"
    workdir.mkdir()
    home.mkdir()
    executable = tmp_path / "broker fixture"
    executable.write_text(f"#!{sys.executable}\n" + FIXTURE)
    executable.chmod(0o700)

    class Harness:
        broker = AllocationBroker(config_for(home, executable), str(workdir), timeout_seconds=1)

        def reply(self, value=None, **options):
            (workdir / "response.json").write_text(json.dumps({"stdout": json.dumps(value), **options}))

        def calls(self):
            path = workdir / "calls.jsonl"
            return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

        def snapshot(self):
            return self.broker.request("snapshot", {"since": None})

    result = Harness()
    result.workdir = workdir
    result.home = home
    result.executable = executable
    result.reply({})
    return result


def test_request_passes_typed_configuration_and_one_exact_operation(harness):
    assert harness.broker.request("submit", {"quantity": 3}) == {}
    bridge = str(Path(__file__).parents[1] / "paper_trading/sdk_bridge.py")
    assert harness.calls()[0]["argv"] == [
        "-I", bridge, "--access-region", "global", "--client-id", OAUTH_CLIENT_ID,
        "--account", ACCOUNT_NO, "--cash-buffer-bps", "200", "--max-order-bps", "1000",
        "--quote-max-age-seconds", "60", "submit", "--request", '{"quantity": 3}']


def test_json_fractional_price_preserves_broker_decimal_token(harness):
    harness.reply(stdout='{"price":100.1234567890123456789}')
    price = harness.snapshot()["price"]
    assert isinstance(price, Decimal)
    assert price == Decimal("100.1234567890123456789")


@pytest.mark.parametrize("value", [[], [{}], "text", 1, None, True])
def test_non_object_response_is_refused(harness, value):
    harness.reply(value)
    with pytest.raises(BrokerError, match="Unexpected broker response shape"):
        harness.snapshot()
    assert len(harness.calls()) == 1


def test_environment_allowlist_home_cwd_and_closed_stdin(harness, monkeypatch):
    allowed = {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8",
               **{key: "http://fixture.invalid:8080" for key in (
                   "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY",
                   "http_proxy", "https_proxy", "all_proxy", "no_proxy")}}
    for key in list(os.environ):
        monkeypatch.delenv(key)
    for key, value in {**allowed, "HOME": "/wrong-home", "PYTHONPATH": "/secret",
                       "LONGBRIDGE_ACCESS_TOKEN": "secret-token",
                       "LONGBRIDGE_HTTP_URL": "https://wrong.invalid",
                       "LONGPORT_APP_SECRET": "secret-key", "OPENAI_API_KEY": "secret-key",
                       "LD_PRELOAD": "/secret.so", "BASH_ENV": "/secret.sh"}.items():
        monkeypatch.setenv(key, value)
    harness.snapshot()
    call = harness.calls()[0]
    assert call["env"] == {**allowed, "HOME": str(harness.home)}
    assert call["cwd"] == str(harness.workdir)
    assert call["stdin"] == ""


@pytest.mark.parametrize("options", [
    {"exit": 1, "stdout": "secret-stdout", "stderr": "secret-credential"},
    {"stdout": "not-json secret-credential"}, {"invalid_utf8": True},
    {"stdout": '{"secret-credential": NaN}'},
    {"stdout": '{"secret-credential": 1, "secret-credential": 2}'},
])
def test_safe_errors_do_not_leak_or_retry(harness, options):
    harness.reply(**options)
    body = {"remark": "secret-order-id"}
    with pytest.raises(BrokerError) as error:
        harness.broker.request("submit", body)
    rendered = "".join(traceback.format_exception(error.value))
    for secret in ("secret-credential", "secret-stdout", "secret-order-id", ACCOUNT_NO, OAUTH_CLIENT_ID):
        assert secret not in str(error.value)
        assert secret not in rendered
    assert len(harness.calls()) == 1


@pytest.mark.parametrize("options", [{"sleep": 30}, {"sleep": 30, "close_pipes": True}, {"fork": True}])
def test_timeout_is_bounded_and_process_is_reaped(harness, options):
    harness.reply(**options)
    started = time.monotonic()
    with pytest.raises(BrokerError, match="timed out"):
        harness.snapshot()
    assert time.monotonic() - started < 4
    assert len(harness.calls()) == 1
    with pytest.raises(ProcessLookupError):
        os.kill(harness.calls()[0]["pid"], 0)
    descendant = harness.workdir / "descendant.pid"
    if descendant.exists():
        stat = Path(f"/proc/{descendant.read_text()}/stat")
        deadline = time.monotonic() + 1
        while stat.exists() and stat.read_text().split()[2] != "Z":
            assert time.monotonic() < deadline
            time.sleep(0.01)


@pytest.mark.parametrize("stream", ["stdout", "stderr"])
def test_output_bound_covers_both_streams(harness, stream):
    harness.reply(**{stream: "x" * 65536, "repeat": MAX_OUTPUT_BYTES // 65536 + 2})
    with pytest.raises(BrokerError, match="output limit"):
        harness.snapshot()
    assert len(harness.calls()) == 1


def test_output_limit_is_combined_not_per_stream(harness):
    harness.reply(stdout="x" * 65536, stderr="y" * 65536, repeat=9)
    with pytest.raises(BrokerError, match="output limit"):
        harness.snapshot()


def test_output_exactly_at_limit_is_not_truncated(harness):
    filler = "a" * (MAX_OUTPUT_BYTES - len('{"x":""}'))
    stdout = json.dumps({"x": filler}, separators=(",", ":"))
    assert len(stdout.encode()) == MAX_OUTPUT_BYTES
    harness.reply(stdout=stdout)
    assert harness.snapshot() == {"x": filler}


def test_launch_failure_is_safe(harness):
    harness.executable.unlink()
    with pytest.raises(BrokerError, match="Could not start") as error:
        harness.snapshot()
    assert str(harness.executable) not in str(error.value)


@pytest.mark.parametrize("field,value", [("sdk_python_path", "relative"), ("broker_home", "relative"),
    ("workdir", "relative"), ("timeout_seconds", 0), ("timeout_seconds", -1),
    ("timeout_seconds", True), ("timeout_seconds", 1.5)])
def test_invalid_configuration(tmp_path, field, value):
    config = config_for(tmp_path, tmp_path / "python")
    arguments = {"workdir": str(tmp_path), "timeout_seconds": 20}
    AllocationBroker(config, **arguments)  # the unchanged configuration is accepted
    if field in arguments:
        arguments[field] = value
    else:
        # Bypass AllocationConfig.parse, which refuses these first: the runner checks on its own.
        config = dataclasses.replace(config, **{field: value})
    with pytest.raises(BrokerError):
        AllocationBroker(config, **arguments)
