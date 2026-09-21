"""Operator-owned limits and strict decimal/time parsing."""
from dataclasses import dataclass
from datetime import datetime, timezone
from decimal import Decimal, InvalidOperation
from pathlib import Path
import json
import re


def money(value, *, zero=False):
    if not isinstance(value, str) or not re.fullmatch(r"[0-9]{1,12}(\.[0-9]{1,8})?", value):
        raise ValueError("amount must be a positive decimal string, without exponent")
    try:
        result = Decimal(value)
    except InvalidOperation as error:
        raise ValueError("invalid decimal") from error
    if result < 0 or (result == 0 and not zero):
        raise ValueError("amount must be positive")
    return result


def integer(value, *, zero=False):
    if isinstance(value, str) and re.fullmatch(r"[0-9]{1,9}", value):
        value = int(value)
    if type(value) is not int or value < (0 if zero else 1) or value > 100_000_000:
        raise ValueError("quantity must be an integer share count")
    return value


def timestamp(value):
    if not isinstance(value, str):
        raise ValueError("timestamp must be an ISO-8601 string with timezone")
    try:
        result = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise ValueError("invalid ISO-8601 timestamp") from error
    if result.tzinfo is None:
        raise ValueError("timestamp timezone is required")
    return result.astimezone(timezone.utc)


def identifier(value):
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]{0,63}", value):
        raise ValueError("identifier must contain 1-64 letters, digits, hyphens or underscores")
    return value


def symbol(value):
    if not isinstance(value, str) or not re.fullmatch(r"[A-Z][A-Z0-9.]{0,12}\.US", value):
        raise ValueError("only explicit US stock/ETF symbols are supported")
    return value


def exact(value, required, optional=()):
    if not isinstance(value, dict) or set(value) - set(required) - set(optional) or set(required) - set(value):
        raise ValueError("missing or unknown fields")


@dataclass(frozen=True)
class Config:
    account_no: str
    owner_track_id: str
    broker_home: str
    research_root: str
    symbols_json: str
    max_order_usd: str
    max_portfolio_usd: str
    max_trade_risk_usd: str
    cli_path: str = "/usr/local/bin/longbridge"
    poll_seconds: int = 60
    quote_max_age_seconds: int = 180
    max_price_deviation_bps: int = 100

    @property
    def symbols(self):
        return tuple(json.loads(self.symbols_json))

    @classmethod
    def parse(cls, values):
        required = {"account_no", "owner_track_id", "broker_home", "research_root", "symbols_json",
                    "max_order_usd", "max_portfolio_usd", "max_trade_risk_usd"}
        exact(values, required, set(cls.__dataclass_fields__) - required)
        obj = cls(**values)
        identifier(obj.account_no)
        if not isinstance(obj.owner_track_id, str) or not obj.owner_track_id.strip():
            raise ValueError("owner_track_id is required")
        for field in ("broker_home", "research_root", "cli_path"):
            if not isinstance(getattr(obj, field), str) or not Path(getattr(obj, field)).is_absolute():
                raise ValueError(f"{field} must be an absolute path")
        if not isinstance(obj.symbols_json, str):
            raise ValueError("symbols_json must encode a JSON array of symbols")
        symbols = json.loads(obj.symbols_json)
        if not isinstance(symbols, list) or not 1 <= len(symbols) <= 30 or not all(isinstance(s, str) for s in symbols):
            raise ValueError("configure 1-30 allowed symbols")
        if len(set(obj.symbols)) != len(obj.symbols):
            raise ValueError("duplicate symbols")
        for item in obj.symbols:
            symbol(item)
        for field in ("max_order_usd", "max_portfolio_usd", "max_trade_risk_usd"):
            money(getattr(obj, field))
        if money(obj.max_order_usd) > money(obj.max_portfolio_usd):
            raise ValueError("order limit cannot exceed portfolio limit")
        for field, low, high in (("poll_seconds", 5, 3600), ("quote_max_age_seconds", 30, 300),
                                 ("max_price_deviation_bps", 1, 500)):
            v = getattr(obj, field)
            if type(v) is not int or not low <= v <= high:
                raise ValueError(f"invalid {field}")
        return obj
