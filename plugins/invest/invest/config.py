"""Strict decimal, time and identifier parsing."""
from datetime import datetime, timezone
from decimal import Decimal, InvalidOperation
import re

from .errors import CONFLICT, Refused


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
    if isinstance(value, str) and re.fullmatch(r"[0-9]{1,9}(\.0{1,8})?", value):
        value = Decimal(value)
    if isinstance(value, Decimal):
        if not value.is_finite() or not 0 <= value <= 100_000_000 or value != value.to_integral_value():
            raise ValueError("quantity must be an integer share count")
        value = int(value)
    if type(value) is not int or value < (0 if zero else 1) or value > 100_000_000:
        raise ValueError("quantity must be an integer share count")
    return value


def broker_money(value, *, zero=False):
    if type(value) not in (str, int, Decimal):
        raise ValueError("broker amount must be a decimal string or JSON number")
    try:
        result = Decimal(value)
    except InvalidOperation as error:
        raise ValueError("invalid broker amount") from error
    if not result.is_finite() or result < 0 or (result == 0 and not zero) or result > Decimal("1e12"):
        raise ValueError("broker amount outside supported range")
    return result


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


def exact(value, required, optional=()):
    if not isinstance(value, dict) or set(value) - set(required) - set(optional) or set(required) - set(value):
        raise ValueError("missing or unknown fields")


def text(value, field, limit):
    """A non-blank string of at most `limit` characters."""
    if not isinstance(value, str) or not value.strip() or len(value) > limit:
        raise ValueError(f'{field} must be non-blank text of at most {limit} characters')
    return value


def captured(refs):
    """Source references: 1-20 captured `neige://source/` URIs."""
    if not isinstance(refs, list) or not 1 <= len(refs) <= 20 or any(
            not isinstance(r, str) or not r.startswith('neige://source/') or len(r) > 512 for r in refs):
        raise ValueError('1-20 captured neige://source/ references required')
    return refs


def version(value, current):
    """The optimistic lock of a `set` or `rm`: `value` (checked by `arguments.parse`) must be the entry's
    current version."""
    if value != current:
        raise Refused(CONFLICT, f'expected_version {value} is stale: the current version is {current}; '
                                'reread and retry', 'stale_version', version=current)
