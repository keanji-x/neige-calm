"""Exercise the production adapter through a deterministic external executable."""

import json
from decimal import Decimal
import os
from pathlib import Path
import sys
import time
import traceback

import pytest

sys.path.insert(0, str(Path(__file__).parents[1]))
from paper_trading.broker import Broker, BrokerError, MAX_OUTPUT_BYTES, cancel_args, order_args


# This fixture only transports prescribed bytes and records process inputs. It
# deliberately implements no broker parsing, identity or confirmation behavior.
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
        broker = Broker(str(executable), str(home), str(workdir), timeout_seconds=1)

        def reply(self, value=None, **options):
            (workdir / "response.json").write_text(json.dumps({"stdout": json.dumps(value), **options}))

        def calls(self):
            path = workdir / "calls.jsonl"
            return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    result = Harness()
    result.workdir = workdir
    result.home = home
    result.executable = executable
    result.reply([])
    return result


def identity_response():
    return {"token": {"status": "valid", "dc_region": "ap"},
            "account": {"account_channel": "lb_papertrading", "account_no": "001234"}}


def test_identity_valid(harness):
    expected = identity_response()
    harness.reply(expected)
    assert harness.broker.identity("001234") == expected
    assert harness.calls()[0]["argv"] == ["auth", "status", "--format", "json"]


def test_json_fractional_price_preserves_broker_decimal_token(harness):
    harness.reply(stdout='[{"time":"2026-09-21T15:00:00Z","price":100.1234567890123456789}]')
    price = harness.broker.intraday("TEST.US")[0]["price"]
    assert isinstance(price, Decimal)
    assert price == Decimal("100.1234567890123456789")


@pytest.mark.parametrize("field,value", [
    ("account_channel", "lb_live"), ("account_channel", "paper"),
    ("account_channel", None), ("account_no", "001235"),
    ("account_no", 1234), ("account_no", " 001234"),
])
def test_identity_rejects_wrong_account_or_channel(harness, field, value):
    response = identity_response()
    response["account"][field] = value
    harness.reply(response)
    with pytest.raises(BrokerError):
        harness.broker.identity("001234")


@pytest.mark.parametrize("value", [None, {}, [], "valid", {"token": {}},
    {"token": {"status": "valid"}, "account": None},
    {"token": {"status": "valid"}, "account": "001234"},
    {"token": {"status": "valid"}, "account": {}},
    {"token": {"status": "valid"}, "account": {"account_no": "001234"}},
    {"token": {"status": "valid"}, "account": {"account_channel": "lb_papertrading"}},
])
def test_identity_missing_metadata_fails_closed(harness, value):
    harness.reply(value)
    with pytest.raises(BrokerError):
        harness.broker.identity("001234")


@pytest.mark.parametrize("status", ["expired", "invalid", "VALID", True, None])
def test_identity_rejects_invalid_token(harness, status):
    response = identity_response()
    response["token"]["status"] = status
    harness.reply(response)
    with pytest.raises(BrokerError):
        harness.broker.identity("001234")


@pytest.mark.parametrize("region", ["us", "AP", "hk", None, True])
def test_identity_rejects_non_ap_region(harness, region):
    response = identity_response()
    response["token"]["dc_region"] = region
    harness.reply(response)
    with pytest.raises(BrokerError):
        harness.broker.identity("001234")


def test_identity_requires_region_metadata(harness):
    response = identity_response()
    del response["token"]["dc_region"]
    harness.reply(response)
    with pytest.raises(BrokerError):
        harness.broker.identity("001234")


def test_identity_is_never_cached(harness):
    harness.reply(identity_response())
    harness.broker.identity("001234")
    response = identity_response()
    response["account"]["account_no"] = "switched"
    harness.reply(response)
    with pytest.raises(BrokerError):
        harness.broker.identity("001234")
    assert len(harness.calls()) == 2


RECORDS = {
    "assets": {"currency": "USD", "net_assets": "1000.00", "total_cash": "900.00", "buy_power": "900.00"},
    "positions": {"symbol": "TEST.US", "name": "Fixture", "quantity": "3", "available": "3",
                  "cost_price": "10.00", "currency": "USD", "market": "US"},
    "orders": {"order_id": "order-1", "symbol": "TEST.US", "side": "Buy", "type": "LO",
               "status": "New", "price": "10.00", "quantity": "3",
               "created_at": "2026-09-21T14:00:00Z", "updated_at": "2026-09-21T14:00:00Z"},
    "executions": {"order_id": "order-1", "trade_id": "fill-1", "symbol": "TEST.US",
                   "price": "10.00", "quantity": "3", "time": "2026-09-21T14:00:00Z"},
    "intraday": {"time": "2026-09-21T14:00:00Z", "price": "10.00", "volume": 3,
                 "turnover": "30.00", "avg_price": "10.00"},
}


@pytest.mark.parametrize("method,args,argv", [
    ("assets", (), ["assets", "--format", "json"]),
    ("positions", (), ["positions", "--format", "json"]),
    ("orders", (), ["order", "--format", "json"]),
    ("orders", ("2026-09-01",), ["order", "--history", "--start", "2026-09-01", "--format", "json"]),
    ("executions", (), ["order", "executions", "--format", "json"]),
    ("executions", ("2026-09-01",), ["order", "executions", "--history", "--start", "2026-09-01", "--format", "json"]),
    ("intraday", ("TEST.US",), ["intraday", "TEST.US", "--session", "intraday", "--format", "json"]),
])
def test_list_commands_preserve_records(harness, method, args, argv):
    rows = [RECORDS[method]]
    harness.reply(rows)
    assert getattr(harness.broker, method)(*args) == rows
    assert harness.calls()[0]["argv"] == argv
    harness.reply([])
    assert getattr(harness.broker, method)(*args) == []


@pytest.mark.parametrize("method", ["assets", "positions", "orders", "executions", "intraday"])
@pytest.mark.parametrize("value", [None, {}, {"orders": [], "has_more": True}, "[]", 0, [None], [1]])
def test_lists_reject_wrong_shapes(harness, method, value):
    harness.reply(value)
    with pytest.raises(BrokerError):
        getattr(harness.broker, method)(*(["TEST.US"] if method == "intraday" else []))


def test_quote_selects_exact_symbol_not_first(harness):
    wanted = {"symbol": "TEST.US", "last": "15.20", "status": "Normal"}
    harness.reply([{"symbol": "test.us"}, {"symbol": "OTHER.US"}, wanted])
    assert harness.broker.quote("TEST.US") == wanted
    assert harness.calls()[0]["argv"] == ["quote", "TEST.US", "--format", "json"]


@pytest.mark.parametrize("value", [[], {}, None, [None], [{"symbol": "test.us"}],
    [{"symbol": "TEST.US"}, {"symbol": "TEST.US"}]])
def test_quote_rejects_missing_ambiguous_or_wrong_shape(harness, value):
    harness.reply(value)
    with pytest.raises(BrokerError):
        harness.broker.quote("TEST.US")


def test_order_detail(harness):
    response = {"order_id": "order-1", "symbol": "TEST.US", "side": "Buy", "order_type": "LO",
                "status": "New", "quantity": "3", "price": "10.00",
                "submitted_at": "2026-09-21T14:00:00Z", "updated_at": "2026-09-21T14:00:00Z",
                "history": []}
    harness.reply(response)
    assert harness.broker.order_detail("order-1") == response
    assert harness.calls()[0]["argv"] == ["order", "detail", "order-1", "--format", "json"]
    harness.reply([])
    with pytest.raises(BrokerError):
        harness.broker.order_detail("order-1")


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
    harness.broker.assets()
    call = harness.calls()[0]
    assert call["env"] == {**allowed, "HOME": str(harness.home)}
    assert call["cwd"] == str(harness.workdir)
    assert call["stdin"] == ""


@pytest.mark.parametrize("side", ["BUY", "sell"])
def test_native_preview_and_explicit_execution_preserve_argv(harness, side):
    remark = "literal spaces; $(touch NEVER) 'quoted'"
    argv = order_args("TEST.US", side, 3, "10.00", remark)
    expected = ["order", side.lower(), "TEST.US", "3", "--price", "10.00",
                "--order-type", "LO", "--tif", "day", "--outside-rth", "RTH_ONLY",
                "--remark", remark, "--format", "json"]
    assert argv == expected == Broker.order_args("TEST.US", side, 3, "10.00", remark)
    native = "Exact native preview\r\nConfirm using code: 123\n\n"
    harness.reply(stdout=native, stderr="not part of preview")
    assert harness.broker.preview(argv) == native
    assert len(harness.calls()) == 1
    assert harness.calls()[0]["argv"] == expected
    harness.reply({"order_id": "fixture-order"})
    assert harness.broker.execute(argv, "123") == {"order_id": "fixture-order"}
    assert harness.calls()[1]["argv"] == expected + ["--execute", "123"]
    assert argv == expected
    assert not (harness.workdir / "NEVER").exists()


def test_cancel_preview_and_native_status_string(harness):
    argv = cancel_args("order-1")
    assert argv == Broker.cancel_args("order-1") == ["order", "cancel", "order-1", "--format", "json"]
    harness.reply(stdout="Cancel preview\n")
    assert harness.broker.preview(argv) == "Cancel preview\n"
    harness.reply("Cancellation requested")
    assert harness.broker.execute(argv, "001") == {"message": "Cancellation requested"}
    assert [call["argv"] for call in harness.calls()] == [argv, argv + ["--execute", "001"]]


@pytest.mark.parametrize("code", ["12", "1234", " 123", "123\n", "abc", "\uff11\uff12\uff13", 123, None])
def test_invalid_confirmation_does_not_spawn(harness, code):
    with pytest.raises(BrokerError):
        harness.broker.execute(cancel_args("order-1"), code)
    assert harness.calls() == []


@pytest.mark.parametrize("argv", [[], "order cancel x", ["auth", "login", "x"],
    ["order", "replace", "x"], ["order", "cancel", "x", "--execute", "123"],
    ["order", "cancel", "x", "--execute=123"], ["order", "cancel", "x", "--exec=123"],
    ["order", "cancel", "x", None], ["order", "cancel", "x\0"]])
def test_preview_and_execute_refuse_embedded_authority(harness, argv):
    with pytest.raises(BrokerError):
        harness.broker.preview(argv)
    with pytest.raises(BrokerError):
        harness.broker.execute(argv, "123")
    assert harness.calls() == []


@pytest.mark.parametrize("options", [
    {"exit": 1, "stdout": "secret-stdout", "stderr": "secret-credential"},
    {"stdout": "not-json secret-credential"}, {"invalid_utf8": True},
    {"stdout": '{"secret-credential": NaN}'},
    {"stdout": '{"secret-credential": 1, "secret-credential": 2}'},
])
def test_safe_errors_do_not_leak_or_retry(harness, options):
    harness.reply(**options)
    argv = cancel_args("secret-order-id")
    with pytest.raises(BrokerError) as error:
        harness.broker.execute(argv, "123")
    rendered = "".join(traceback.format_exception(error.value))
    for secret in ("secret-credential", "secret-stdout", "secret-order-id"):
        assert secret not in str(error.value)
        assert secret not in rendered
    assert len(harness.calls()) == 1


@pytest.mark.parametrize("options", [{"sleep": 30}, {"sleep": 30, "close_pipes": True}, {"fork": True}])
def test_timeout_is_bounded_and_process_is_reaped(harness, options):
    harness.reply(**options)
    started = time.monotonic()
    with pytest.raises(BrokerError, match="timed out"):
        harness.broker.execute(cancel_args("order-1"), "123")
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
        harness.broker.assets()
    assert len(harness.calls()) == 1


def test_output_limit_is_combined_not_per_stream(harness):
    harness.reply(stdout="x" * 65536, stderr="y" * 65536, repeat=9)
    with pytest.raises(BrokerError, match="output limit"):
        harness.broker.preview(cancel_args("order-1"))


def test_output_exactly_at_limit_is_not_truncated(harness):
    harness.reply(stdout="x" * 65536, repeat=MAX_OUTPUT_BYTES // 65536)
    assert harness.broker.preview(cancel_args("order-1")) == "x" * MAX_OUTPUT_BYTES


def test_launch_failure_is_safe(harness):
    harness.executable.unlink()
    with pytest.raises(BrokerError, match="Could not start") as error:
        harness.broker.assets()
    assert str(harness.executable) not in str(error.value)


@pytest.mark.parametrize("field,value", [("executable", "relative"), ("home", "relative"),
    ("workdir", "relative"), ("timeout_seconds", 0), ("timeout_seconds", -1),
    ("timeout_seconds", True), ("timeout_seconds", 1.5)])
def test_invalid_configuration(field, value):
    config = dict(executable="/bin/broker", home="/tmp/home", workdir="/tmp", timeout_seconds=20)
    config[field] = value
    with pytest.raises(BrokerError):
        Broker(**config)


@pytest.mark.parametrize("quantity", [True, 0, -1, 1.5, "1"])
def test_order_args_rejects_nonpositive_or_noninteger_quantity(quantity):
    with pytest.raises(BrokerError):
        order_args("TEST.US", "buy", quantity, "10", "owned")


@pytest.mark.parametrize("side", ["short", "replace", "", None])
def test_order_args_rejects_invalid_side(side):
    with pytest.raises(BrokerError):
        order_args("TEST.US", side, 1, "10", "owned")


@pytest.mark.parametrize("part,field", [("token", "status"), ("token", "dc_region"),
    ("account", "account_channel"), ("account", "account_no")])
def test_identity_requires_each_field(harness, part, field):
    response = identity_response()
    del response[part][field]
    harness.reply(response)
    with pytest.raises(BrokerError):
        harness.broker.identity("001234")


@pytest.mark.parametrize("expected", [None, "", 1234, True])
def test_identity_invalid_expected_account_never_spawns(harness, expected):
    with pytest.raises(BrokerError):
        harness.broker.identity(expected)
    assert harness.calls() == []


@pytest.mark.parametrize("method", ["orders", "executions"])
def test_empty_history_start_is_not_silently_today(harness, method):
    with pytest.raises(BrokerError):
        getattr(harness.broker, method)("")
    assert harness.calls() == []


@pytest.mark.parametrize("value", [None, [], "Submitted", 1, False])
def test_order_execution_requires_object_response_without_retry(harness, value):
    harness.reply(value)
    with pytest.raises(BrokerError):
        harness.broker.execute(order_args("TEST.US", "buy", 1, "10", "owned"), "123")
    assert len(harness.calls()) == 1


@pytest.mark.parametrize("value", [None, [], "", 1, False, {}, {"message": "cancelled"}])
def test_cancel_execution_rejects_invalid_response_without_retry(harness, value):
    harness.reply(value)
    with pytest.raises(BrokerError):
        harness.broker.execute(cancel_args("order-1"), "123")
    assert len(harness.calls()) == 1
