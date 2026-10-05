"""The production Portfolio, its SDK subprocess runner and a prescribed broker transport."""
from contextlib import contextmanager
from datetime import datetime, timedelta, timezone
import json
from pathlib import Path
import re

import pytest

from invest import instruments
from invest.broker import Broker
from invest.errors import Refused
from invest.ledger import encoded
from invest.portfolio import Portfolio
from invest.settings import InvestConfig

ROOT = Path(__file__).parents[1]

NOW = datetime(2026, 9, 30, 15, tzinfo=timezone.utc)  # 11:00 New York, regular session
PLANNER = {'role': 'planner', 'card_id': 'planner-card', 'session_id': 'planner-session'}
WORKER = {'role': 'worker', 'card_id': 'worker-card', 'session_id': 'worker-session'}
SOURCES = ['neige://source/research-1', 'neige://source/market-1']


@contextmanager
def refusal(code, match=None):
    """The call is refused with JSON-RPC `code`; the message names the served tool and matches `match`."""
    with pytest.raises(Refused) as caught:
        yield caught
    message = str(caught.value)
    assert caught.value.code == code, (caught.value.code, message)
    assert message.startswith('plugin_invest_'), message
    assert match is None or re.search(match, message), message


def track(track_id, creator=None, key=None):
    """The host's `_meta["dev.neige/track"]`: the Track and its creator provenance (#2104 K1)."""
    return {'id': track_id, 'creator_track_id': creator, 'creator_key': key}


OWNER = track('owner')  # the portfolio Track: an ordinary Track, created by no Planner


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
                       'instrument_recipe_id': 'recipe-instrument',
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
        return self.app.call(OWNER, 'decision_add', args, PLANNER)

    def request(self, decision_id='d-1'):
        return self.app.call(OWNER, 'execution_add', {'decision_id': decision_id}, WORKER)

    def step(self):
        """One background-loop pass: reconcile, verify, then act on the requested decision."""
        [(track, state)] = self.app.process_once()
        assert track == 'owner'
        return state

    def execute(self, decision_id='d-1'):
        assert self.request(decision_id)['decisions'][-1]['state'] == 'requested'
        return self.step()

    def status(self):
        return self.app.call(OWNER, 'portfolio_status', {}, PLANNER)

    # Instruments, research Tracks and theses.
    def add(self, symbol, message='Cover this symbol with a research Track.'):
        return self.app.call(OWNER, 'instrument_add', {'symbol': symbol, 'message': message}, PLANNER)

    def cover(self, *symbols, price='100'):
        """`instrument_add` each symbol, quote it, and let the loop verify it: live, key 1 issued."""
        for symbol in symbols:
            self.add(symbol)
            self.quote(symbol, price)
        return self.step()

    def instrument(self, symbol):
        return next(i for i in self.status()['instruments'] if i['symbol'] == symbol)

    def renew(self, symbol, message='Renew: the research Track went silent.'):
        version = self.instrument(symbol)['version']
        return self.app.call(OWNER, 'instrument_set', {'symbol': symbol, 'expected_version': version,
                                                       'message': message}, PLANNER)

    def remove(self, symbol, message='No longer worth covering.'):
        version = self.instrument(symbol)['version']
        return self.app.call(OWNER, 'instrument_rm', {'symbol': symbol, 'expected_version': version,
                                                      'message': message}, PLANNER)

    def research(self, key, name, args=None, creator='owner'):
        """`name` called by the Planner of the Track that `creator` added under `key`."""
        return self.app.call(track(f'research-{key}', creator, key), name, {} if args is None else args, PLANNER)

    def thesis(self, thesis_id, symbol, **changes):
        args = {'thesis_id': thesis_id, 'symbol': symbol, 'stance': 'bullish', 'title': f'Thesis {thesis_id}',
                'summary': 'Demand outruns supply through next year.', 'body': 'The full argument, sourced.',
                'source_refs': SOURCES} | changes
        return self.app.call(OWNER, 'thesis_add', args, PLANNER)

    def open_thesis(self, thesis_id):
        return next(t for t in self.status()['theses'] if t['thesis_id'] == thesis_id)

    def assess(self, key, thesis_id, assessment, **changes):
        thesis = self.open_thesis(thesis_id)
        args = {'thesis_id': thesis_id, 'assessment': assessment, 'summary': thesis['summary'],
                'source_refs': thesis['source_refs'], 'expected_version': thesis['version']} | changes
        return self.research(key, 'thesis_set', args)

    def retire(self, thesis_id, message='The catalyst has passed.'):
        version = self.open_thesis(thesis_id)['version']
        return self.app.call(OWNER, 'thesis_rm', {'thesis_id': thesis_id, 'expected_version': version,
                                                  'message': message}, PLANNER)

    def dump(self):
        """Every ledger row as stored, except the access stamp `last_seen_at`: the domain state."""
        with self.app.ledger.session() as db:
            tables = [row[0] for row in db.execute(
                "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")]
            return {name: [encoded({k: row[k] for k in row.keys() if k != 'last_seen_at'})
                           for row in db.execute(f'SELECT * FROM {name} ORDER BY rowid')] for name in tables}

