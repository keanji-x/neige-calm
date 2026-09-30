"""Account-only startup, human strategy authority, and durable Track binding."""
from dataclasses import asdict
import hashlib
import json
import sqlite3

import pytest

from paper_trading.broker import Broker
from paper_trading.config import AccountConfig
from paper_trading.strategy import Portfolio
from .conftest import NOW


@pytest.fixture
def account(rig):
    return AccountConfig.parse({key: rig.values[key] for key in
                                ('account_no', 'broker_home', 'cli_path', 'poll_seconds')})


@pytest.fixture
def policy(rig):
    return {'research_root': str(rig.research), 'symbols': ['SOXX.US'],
            'max_order_usd': '5000', 'max_portfolio_usd': '10000', 'max_trade_risk_usd': '100'}


@pytest.fixture
def portfolio(tmp_path, account, rig):
    root = tmp_path / 'portfolio'
    return Portfolio(root, account, Broker(account.cli_path, account.broker_home, str(root)), clock=lambda: NOW)


def approve(portfolio, policy, track='track-owner'):
    proposed = portfolio.call(track, 'paper.strategy', policy)
    portfolio.approve(proposed['strategy']['proposal']['revision'])
    return proposed['strategy']['proposal']['revision']


def test_account_only_startup_has_no_strategy_or_broker_calls(portfolio, rig):
    status = portfolio.call('track-owner', 'paper.status', {})
    assert status['strategy']['phase'] == 'unconfigured'
    assert status['snapshot'] is None
    assert portfolio.process_once() == []
    assert rig.calls() == []


def test_proposal_is_idempotent_and_cannot_grant_authority(portfolio, policy, rig):
    first = portfolio.call('track-owner', 'paper.strategy', policy)
    assert first == portfolio.call('track-owner', 'paper.strategy', policy)
    assert first['strategy']['phase'] == 'awaiting_approval'
    assert first['strategy']['proposal']['track_id'] == 'track-owner'
    for tool, args in [('paper.ingest', {'week': '2026-09-21'}), ('paper.decide', rig.plan()),
                       ('paper.pause', {'paused': False}), ('paper.refresh', {})]:
        with pytest.raises(ValueError, match='approval required'):
            portfolio.call('track-owner', tool, args)
    for name in ['paper.approve', 'paper.strategy.approve', 'paper.execute']:
        with pytest.raises(ValueError, match='unknown tool'):
            portfolio.call('track-owner', name, {})
    assert rig.calls() == []
    assert not (portfolio.root / 'ledger.sqlite3').exists()


@pytest.mark.parametrize('field,value', [('owner_track_id', 'other'), ('account_no', 'OTHER'),
                                       ('approved', True), ('broker_home', '/tmp/other')])
def test_proposal_rejects_forged_identity_and_authority(portfolio, policy, field, value):
    with pytest.raises(ValueError, match='fields'):
        portfolio.call('track-owner', 'paper.strategy', policy | {field: value})


@pytest.mark.parametrize('field', ['max_order_usd', 'max_portfolio_usd', 'max_trade_risk_usd', 'symbols'])
def test_strategy_requires_explicit_risk_and_symbol_choices(portfolio, policy, field):
    del policy[field]
    with pytest.raises(ValueError, match='fields'):
        portfolio.call('track-owner', 'paper.strategy', policy)


def test_human_approval_survives_restart_and_fences_other_tracks(portfolio, policy, account):
    revision = approve(portfolio, policy)
    restarted = Portfolio(portfolio.root, account, portfolio.broker, clock=lambda: NOW)
    state = restarted.call('track-owner', 'paper.status', {})
    assert state['strategy']['active']['revision'] == revision
    assert state['strategy']['phase'] == 'approved'
    assert restarted.call('track-owner', 'paper.ingest', {'week': '2026-09-21'})['week'] == '2026-09-21'
    for name, args in [('paper.status', {}), ('paper.strategy', policy), ('paper.journal', {})]:
        with pytest.raises(ValueError, match='another Track'):
            restarted.call('foreign-track', name, args)
    assert state['strategy']['active']['settings']['symbols'] == ['SOXX.US']


def test_approval_refuses_superseded_proposal_and_does_not_rebind(portfolio, policy):
    first = portfolio.call('track-owner', 'paper.strategy', policy)['strategy']['proposal']['revision']
    foreign = portfolio.call('foreign-track', 'paper.strategy', policy)['strategy']['proposal']['revision']
    second = portfolio.call('track-owner', 'paper.strategy', policy | {'max_order_usd': '4000'})
    with pytest.raises(ValueError, match='superseded'):
        portfolio.approve(first)
    portfolio.approve(second['strategy']['proposal']['revision'])
    with pytest.raises(ValueError, match='another Track'):
        portfolio.approve(foreign)


def test_proposal_cannot_change_active_limits(portfolio, policy, rig):
    revision = approve(portfolio, policy)
    portfolio.call('track-owner', 'paper.strategy', policy | {'max_trade_risk_usd': '1000'})
    portfolio.call('track-owner', 'paper.ingest', {'week': '2026-09-21'})
    portfolio.call('track-owner', 'paper.decide', rig.plan(quantity=30))
    [(track, result)] = portfolio.process_once()
    assert track == 'track-owner'
    assert result['strategy']['active']['revision'] == revision
    assert result['decisions'][0]['strategy_revision'] == revision
    assert 'price risk' in result['decisions'][0]['error']
    candidate = result['strategy']['proposal']['revision']
    with pytest.raises(ValueError, match='unresolved decisions or open trades'):
        portfolio.approve(candidate)


def test_approval_revalidates_paper_account_and_external_activity(portfolio, policy, rig):
    proposal = portfolio.call('track-owner', 'paper.strategy', policy)['strategy']['proposal']['revision']
    state = rig.state()
    state['identity']['account']['account_channel'] = 'lb_live'
    rig.write(state)
    with pytest.raises(Exception):
        portfolio.approve(proposal)
    assert portfolio.call('track-owner', 'paper.status', {})['strategy']['active'] is None
    state['identity']['account']['account_channel'] = 'lb_papertrading'
    state['positions'] = [{'symbol': 'SOXX.US', 'quantity': '1', 'available': '1', 'currency': 'USD'}]
    rig.write(state)
    with pytest.raises(ValueError, match='reconciliation'):
        portfolio.approve(proposal)
    assert not any('--execute' in call for call in rig.calls())


def test_account_binding_cannot_change_on_restart(portfolio, account, policy):
    approve(portfolio, policy)
    with pytest.raises(ValueError, match='account binding'):
        Portfolio(portfolio.root, AccountConfig.parse(asdict(account) | {'account_no': 'OTHER'}), portfolio.broker)


def test_unapproved_foreign_proposal_does_not_reserve_account(portfolio, policy):
    portfolio.call('foreign-track', 'paper.strategy', policy)
    revision = approve(portfolio, policy)
    assert portfolio.call('track-owner', 'paper.status', {})['strategy']['active']['revision'] == revision


def test_order_preview_cannot_cross_policy_revision(portfolio, policy, rig):
    first = approve(portfolio, policy)
    second = approve(portfolio, policy | {'max_order_usd': '4500'})
    assert second != first
    before = len(rig.calls())
    with pytest.raises(ValueError, match='changed since preview'):
        portfolio.operator_confirm('entry-1', {'strategy_revision': first, 'arguments': []}, '731')
    assert len(rig.calls()) == before


def test_policy_snapshot_corruption_is_refused(portfolio, policy):
    proposal = portfolio.call('track-owner', 'paper.strategy', policy)['strategy']['proposal']
    with sqlite3.connect(portfolio.root / 'strategy.sqlite3') as db:
        db.execute('UPDATE proposals SET settings=? WHERE revision=?',
                   (json.dumps(proposal['settings'] | {'max_trade_risk_usd': '99999'}), proposal['revision']))
    with pytest.raises(ValueError, match='integrity'):
        portfolio.approve(proposal['revision'])


def test_legacy_import_cannot_invent_or_default_strategy_values(rig, account):
    portfolio = Portfolio(rig.data, account, rig.broker)
    values = dict(rig.values)
    del values['max_trade_risk_usd']
    with pytest.raises(ValueError, match='fields'):
        portfolio.import_legacy(values)
    with pytest.raises(ValueError, match='JSON array'):
        portfolio.import_legacy(rig.values | {'symbols_json': '{"SOXX.US":true}'})


def test_explicit_legacy_import_preserves_history_and_requires_exact_binding(rig, account):
    rig.decide()
    before = hashlib.sha256((rig.data / 'ledger.sqlite3').read_bytes()).hexdigest()
    portfolio = Portfolio(rig.data, account, rig.broker, clock=lambda: NOW)
    assert portfolio.call('track-owner', 'paper.status', {})['strategy']['phase'] == 'migration_required'
    with pytest.raises(ValueError, match='binding'):
        portfolio.import_legacy(rig.values | {'owner_track_id': 'wrong'})
    portfolio.import_legacy(rig.values)
    assert hashlib.sha256((rig.data / 'ledger.sqlite3').read_bytes()).hexdigest() == before
    status = portfolio.call('track-owner', 'paper.status', {})
    assert status['decisions'][0]['id'] == 'entry-1'
    assert 'strategy_revision' not in status['decisions'][0]
    assert status['strategy']['active']['settings']['max_order_usd'] == '5000'
