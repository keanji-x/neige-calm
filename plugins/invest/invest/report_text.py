"""Plain-language display copy. Structured ledger facts remain unchanged."""
from decimal import Decimal

from .config import timestamp
from .reconcile import NEW_YORK


def money_text(value):
    amount = Decimal(value)
    prefix = '-' if amount < 0 else ''
    return f'{prefix}${abs(amount):,.2f}'


def bounded(value, limit=2048):
    suffix = '... [truncated]'
    return value if len(value) <= limit else value[:limit - len(suffix)] + suffix


def new_york(at):
    return f"{timestamp(at).astimezone(NEW_YORK):%Y-%m-%d %H:%M} 纽约时间"
