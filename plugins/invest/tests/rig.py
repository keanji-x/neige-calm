"""The production Portfolio, its SDK subprocess runner and a prescribed broker transport."""
from datetime import datetime, timedelta, timezone
import json
from pathlib import Path

from invest import instruments
from invest.broker import Broker
from invest.portfolio import Portfolio
from invest.settings import InvestConfig

ROOT = Path(__file__).parents[1]

NOW = datetime(2026, 9, 30, 15, tzinfo=timezone.utc)  # 11:00 New York, regular session
PLANNER = {'role': 'planner', 'card_id': 'planner-card', 'session_id': 'planner-session'}
WORKER = {'role': 'worker', 'card_id': 'worker-card', 'session_id': 'worker-session'}
SOURCES = ['neige://source/research-1', 'neige://source/market-1']


def sdk(symbol):
    venue, code = symbol.split(':')
    return f'{code}.{venue}'


class Rig:
    """One portfolio Track ('owner') over a prescribed paper account."""

    def __init__(self, tmp_path):
        self.home = tmp_path / 'home'; self.home.mkdir()
        self.root = tmp_path / 'data'; self.root.mkdir()
        self.path = self.home / 'invest-broker.json'
        self.values = {'account_no': 'PAPER123', 'broker_home': str(self.home), 'portfolio_track_id': 'owner',
                       'oauth_client_id': 'sdk-client', 'sdk_python_path': str(ROOT / 'tests/broker_fixture.py'),
                       'max_held': 8, 'max_watched': 8, 'max_weight_bps': 10000,
                       'max_order_bps': 10000, 'poll_seconds': 5, 'drift_bps': 0}
        self.write({'snapshot': {
            'identity': {'account_no': 'PAPER123', 'account_channel': 'lb_papertrading'},
            'cash_usd': '10000', 'available_cash_usd': '10000', 'positions': {}, 'quotes': {},
            'market_open': True, 'orders': [], 'fills': []}})
        self.clock = lambda: NOW
        self.configure()

    # Configuration and process lifetime.
    def configure(self, **changes):
        self.values = self.values | changes
        self.config = InvestConfig.parse(self.values)
        self.broker = Broker(self.config, str(self.root))
        self.restart()

    def restart(self):
        self.app = Portfolio(self.root, self.config, self.broker, clock=lambda: self.clock())

    def opening(self, **shares):
        """Acknowledge `shares` as opening positions; the broker holds exactly them."""
        positions = [{'symbol': f'US:{code}', 'shares': n} for code, n in shares.items()]
        state = self.read()
        for code, n in shares.items():
            state['snapshot']['positions'][f'{code}.US'] = {'shares': n, 'available_shares': n}
        self.write(state)
        self.configure(opening_positions=json.dumps(positions))

    # The prescribed broker.
    def read(self):
        return json.loads(self.path.read_text())

    def write(self, state):
        self.path.write_text(json.dumps(state))

    def account(self, **fields):
        state = self.read(); state['snapshot'].update(fields); self.write(state)

    def quote(self, symbol, price, at=None, status='Normal'):
        state = self.read()
        state['snapshot']['quotes'][sdk(symbol)] = {'price': price, 'at': (at or self.clock()).isoformat(),
                                                    'status': status}
        self.write(state)

    def calls(self):
        log = self.home / 'invest-calls.jsonl'
        return [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []

    def submits(self):
        return [c['request'] for c in self.calls() if c['method'] == 'submit']

    def publish(self, status='New'):
        """The broker lists the latest submitted order under the identity the submit returned."""
        request = self.submits()[-1]
        state = self.read()
        state['snapshot']['orders'].append(request | {
            'order_id': f'order-{len(self.submits())}', 'quantity': str(request['quantity']),
            'executed_quantity': '0', 'status': status})
        self.write(state)

    def fill(self, price='100', settled=True):
        """The latest listed order fills completely; positions and cash follow. Unsettled sale
        proceeds raise cash but not available cash."""
        state = self.read(); snapshot = state['snapshot']
        order = snapshot['orders'][-1]
        qty, amount = int(order['quantity']), int(order['quantity']) * float(price)
        order['status'], order['executed_quantity'] = 'Filled', order['quantity']
        position = snapshot['positions'].setdefault(order['symbol'], {'shares': 0, 'available_shares': 0})
        sign = 1 if order['side'] == 'Buy' else -1
        position['shares'] += sign * qty; position['available_shares'] += sign * qty
        if not position['shares']:
            del snapshot['positions'][order['symbol']]
        cash = float(snapshot['cash_usd']) - sign * amount
        available = float(snapshot['available_cash_usd']) - (amount if sign > 0 else (-amount if settled else 0))
        snapshot['cash_usd'], snapshot['available_cash_usd'] = f'{cash:.2f}', f'{available:.2f}'
        snapshot['fills'].append({'trade_id': f"fill-{order['order_id']}", 'order_id': order['order_id'],
                                  'symbol': order['symbol'], 'quantity': order['quantity'], 'price': price,
                                  'time': self.clock().isoformat()})
        self.write(state)
        return self.step()

    # Production entry points.
    def watch(self, *symbols):
        """Admit instruments through the ledger entry point, then let the loop verify them."""
        with self.app.ledger.session() as db:
            for symbol in symbols:
                instruments.admit(self.app.ledger, db, symbol, self.clock())
        return self.step()

    def decide(self, targets, decision_id='d-1', **changes):
        """`targets` `{symbol: bps}` as the Planner's weights; `changes` override any argument."""
        args = {'decision_id': decision_id,
                'weights': [{'symbol': s, 'bps': b} for s, b in targets.items()],
                'message': 'Captured research and market evidence support these weights.',
                'source_refs': SOURCES, 'valid_until': (NOW + timedelta(hours=1)).isoformat(), **changes}
        return self.app.call('owner', 'decision_add', args, PLANNER)

    def request(self, decision_id='d-1'):
        return self.app.call('owner', 'execution_add', {'decision_id': decision_id}, WORKER)

    def step(self):
        """One background-loop pass: reconcile, verify, then act on the requested decision."""
        [(track, state)] = self.app.process_once()
        assert track == 'owner'
        return state

    def execute(self, decision_id='d-1'):
        assert self.request(decision_id)['decisions'][-1]['state'] == 'requested'
        return self.step()

    def status(self):
        return self.app.call('owner', 'portfolio_status', {}, PLANNER)

