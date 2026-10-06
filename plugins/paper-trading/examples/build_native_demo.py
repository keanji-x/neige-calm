"""Build the example SPY Report data through the production allocation path. No broker access.

A scripted, simulated paper account supplies quotes, cash, SPY shares, orders and
executions. Everything else runs in production code: Planner targets and Worker
requests enter through ``Allocation.call``, the background pass is
``Allocation.process_once`` (reconciliation, sizing, submission, valuation), and
the overlays are ``allocation_views.units(...)``. The views are the template views
of ``spy-recipe.md``; only their descriptions are replaced, so the example never
claims a real account.
"""
import argparse
from copy import deepcopy
from datetime import date, datetime, time, timedelta, timezone
from decimal import Decimal
import json
from pathlib import Path
import re
import sys
import tempfile

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT.parent))
from paper_trading.allocation import NEW_YORK, Allocation  # noqa: E402
from paper_trading.allocation_config import AllocationConfig  # noqa: E402
from paper_trading.allocation_views import units  # noqa: E402
from paper_trading.config import timestamp  # noqa: E402

DESCRIPTION = '示例数据 · 脚本化模拟账户，非真实账户；行情、订单与成交均由脚本生成。数值为对账估值，盈亏未扣除费用与出入金。'
TRACK = 'example-track'
PLANNER = {'role': 'planner', 'card_id': 'example-planner-card', 'session_id': 'example-planner-session'}
WORKER = {'role': 'worker', 'card_id': 'example-worker-card', 'session_id': 'example-worker-session'}
# The broker is in-process, so the SDK interpreter and broker home are never used.
CONFIG = {'profile': 'spy_cash', 'account_no': 'EXAMPLE-PAPER', 'broker_home': '/example/unused-broker-home',
          'owner_track_id': TRACK, 'oauth_client_id': 'example-client', 'sdk_python_path': '/example/unused-sdk-python',
          # One order may reach the target, so each decision shows one complete outcome (the default caps a step at 10%).
          'max_order_bps': 10000}
FIRST, LAST = date(2026, 7, 1), date(2026, 9, 30)
HOLIDAYS = {date(2026, 7, 3), date(2026, 9, 7)}  # NYSE: Independence Day (observed), Labor Day
SOURCES = ['neige://source/example-market-breadth', 'neige://source/example-macro-calendar']
AFTER_CLOSE = date(2026, 8, 19)
# Planner decisions: day -> (target bps, executions per order, rationale).
DECISIONS = {
    date(2026, 7, 1): (5000, 1, '示例理由：趋势与波动率处于常态区间，先建立 50% SPY 基础仓位，其余保留现金。'),
    date(2026, 7, 22): (5000, 1, '示例理由：证据没有实质变化，维持 50% 目标；实际比例在偏离阈值内则无需调仓。'),
    date(2026, 8, 5): (9000, 2, '示例理由：盈利修正与市场宽度同步改善，把 SPY 目标比例提高到 90%。'),
    AFTER_CLOSE: (6000, 1, '示例理由：收盘后出现回撤信号，拟降到 60%；只在下一次开盘前有效，过期后由 Planner 重新判断。'),
    date(2026, 9, 16): (7500, 1, '示例理由：议息会议前降低集中度，把 SPY 目标比例调到 75%。'),
}
CENT = Decimal('0.01')


class ScriptedBroker:
    """A simulated paper account: it keeps records and fills accepted orders; it has no sizing or valuation."""
    def __init__(self, cash, clock):
        self.cash, self.shares, self.orders, self.fills, self.pending = Decimal(cash), 0, [], [], {}
        self.price, self.quote_at, self.market_open, self.tranches = None, None, False, 1
        self.clock, self.submitted = clock, {}

    def quote(self, price, at, market_open=True):
        self.price, self.quote_at, self.market_open = price.quantize(CENT), at, market_open

    def snapshot(self, since):
        for order in self.orders:
            if self.pending.get(order['order_id']) and self.market_open:
                self.execute(order, self.pending[order['order_id']].pop(0))
        # Window records like the SDK bridge: today's orders and executions, plus history from `since`
        # (None once nothing is unresolved, so older resolved orders take reconcile's archived path).
        now = self.clock()
        day = now.astimezone(NEW_YORK).date()
        start = timestamp(since) if since is not None else None
        today = lambda at: at.astimezone(NEW_YORK).date() == now.astimezone(NEW_YORK).date()
        recent = lambda at: start is not None and start <= at <= now
        orders = [o for o in self.orders if today(self.submitted[o['order_id']]) or recent(self.submitted[o['order_id']])]
        fills = [f for f in self.fills if today(timestamp(f['time']))]
        fills += [f for f in self.fills if recent(timestamp(f['time']))]
        return deepcopy({'identity': {'account_no': CONFIG['account_no'], 'account_channel': 'lb_papertrading'},
                         'cash_usd': str(self.cash), 'available_cash_usd': str(self.cash),
                         'shares': self.shares, 'available_shares': self.shares,
                         'quote': {'price': str(self.price), 'at': self.quote_at.isoformat(), 'status': 'Normal',
                                   # Observed on full sessions only.
                                   'calendar_date': day.isoformat(), 'trading_day': True, 'half_day': False,
                                   'regular_close_at': new_york(day, 16, 0).isoformat()},
                         'market_open': self.market_open, 'orders': orders, 'fills': fills})

    def submit(self, request):
        key = f'SIM-{len(self.orders) + 1:04}'
        self.submitted[key] = self.clock()
        self.orders.append(request | {'order_id': key, 'quantity': str(request['quantity']),
                                      'executed_quantity': '0', 'status': 'New'})
        first = request['quantity'] // self.tranches
        self.pending[key] = [first] * (self.tranches - 1) + [request['quantity'] - first * (self.tranches - 1)]
        return key

    def execute(self, order, quantity):
        """One execution at the current quote; the next observation reports it."""
        sign = 1 if order['side'] == 'Buy' else -1
        self.shares += sign * quantity
        self.cash -= sign * quantity * self.price
        assert self.cash >= 0 and self.shares >= 0, 'the simulated account cannot go negative'
        executed = int(order['executed_quantity']) + quantity
        order['executed_quantity'] = str(executed)
        order['status'] = 'Filled' if executed == int(order['quantity']) else 'PartialFilled'
        self.fills.append({'trade_id': f'SIM-T{len(self.fills) + 1:04}', 'order_id': order['order_id'],
                           'symbol': 'SPY.US', 'quantity': str(quantity), 'price': str(self.price),
                           'time': self.quote_at.isoformat()})


def trading_days():
    day = FIRST
    while day <= LAST:
        if day.weekday() < 5 and day not in HOLIDAYS:
            yield day
        day += timedelta(days=1)


def sessions():
    """Deterministic integer-cent opening and closing SPY prices for each trading day."""
    seed, close = 20260810, 60000
    for day in trading_days():
        seed = (seed * 1103515245 + 12345) % 2 ** 31
        opening = close + seed % 201 - 95
        seed = (seed * 1103515245 + 12345) % 2 ** 31
        close = opening + seed % 1401 - 680
        yield day, Decimal(opening) / 100, Decimal(close) / 100


def new_york(day, hour, minute, second=0):
    return datetime.combine(day, time(hour, minute, second), NEW_YORK).astimezone(timezone.utc)


def simulate(root):
    """Drive the production Allocation through the script and return its publication input."""
    clock = {'now': None}
    broker = ScriptedBroker('100000', lambda: clock['now'])
    app = Allocation(root, AllocationConfig.parse(CONFIG), broker, clock=lambda: clock['now'])

    def at(day, hour, minute, second=0):
        clock['now'] = new_york(day, hour, minute, second)
        return clock['now']

    def decide(day, valid_until):
        target, broker.tranches, rationale = DECISIONS[day]
        key = f'spy-{day:%Y%m%d}'
        app.call(TRACK, 'spy.plan', {'decision_id': key, 'target_spy_bps': target, 'rationale': rationale,
                                     'source_refs': SOURCES, 'valid_until': valid_until.isoformat()}, PLANNER)
        clock['now'] += timedelta(seconds=20)
        app.call(TRACK, 'spy.execute', {'decision_id': key}, WORKER)

    def observe():
        """One background-loop pass; every scripted observation must reconcile cleanly."""
        [(track, state)] = app.process_once()
        assert track == TRACK and state['error'] is None, state['error']
        return state

    for day, opening, close in sessions():
        broker.quote(opening, at(day, 9, 30))
        at(day, 9, 31); observe()
        if day in DECISIONS and day != AFTER_CLOSE:
            at(day, 10, 0); decide(day, clock['now'] + timedelta(hours=6))
            broker.quote(opening, at(day, 10, 0, 30))
            at(day, 10, 1); observe()  # reconcile, size and submit (or record a no-op)
            for step in range(broker.tranches):
                broker.quote(opening + (close - opening) * (step + 1) / 4, at(day, 10, 5 + 25 * step))
                at(day, 10, 6 + 25 * step); observe()  # reconcile the next execution
        broker.quote(close, at(day, 15, 58, 30))
        at(day, 15, 59); state = observe()
        if day == AFTER_CLOSE:
            # Requested after the close and valid only until before the next open: it waits, then expires.
            at(day, 16, 30); decide(day, new_york(day + timedelta(days=1), 9, 25))
            broker.quote(close, at(day, 16, 0), market_open=False)
            at(day, 16, 31); state = observe()
    return state


def template_views():
    """The recipe's template views, each with only its description marked as example data."""
    fences = re.findall(r'^```neige-block view\n(.*?)\n```$', (ROOT.parent / 'spy-recipe.md').read_text(), flags=re.M | re.S)
    return [json.loads(fence) | {'description': DESCRIPTION} for fence in fences]


def create_example(state):
    """The template views and every production data unit of the scripted run, by overlay kind."""
    return {'views': template_views(), 'overlays': units(state)}


def build():
    with tempfile.TemporaryDirectory() as root:
        return create_example(simulate(root))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    example = build()
    encoded = json.dumps(example, ensure_ascii=False, indent=2, allow_nan=False) + '\n'
    destination = ROOT / 'native-demo.json'
    if args.check:
        assert destination.read_text() == encoded, 'Native Demo fixture drift'
    else:
        destination.write_text(encoded)
    print(json.dumps({'views': len(example['views']), 'overlays': len(example['overlays']), 'bytes': len(encoded.encode())}))


if __name__ == '__main__':
    main()
