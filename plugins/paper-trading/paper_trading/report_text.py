"""Plain-language display copy. Structured ledger facts remain unchanged."""
from decimal import Decimal


def money_text(value):
    amount = Decimal(value)
    prefix = '-' if amount < 0 else ''
    return f'{prefix}${abs(amount):,.2f}'


def bounded(value, limit=2048):
    suffix = '... [truncated]'
    return value if len(value) <= limit else value[:limit - len(suffix)] + suffix
