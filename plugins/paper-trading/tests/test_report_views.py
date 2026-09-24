from copy import deepcopy
import pytest

from paper_trading.report import tables
from paper_trading.report_views import overview, activity
from .test_strategy import account, policy, portfolio, approve  # noqa: F401 - pytest fixtures


def test_unapproved_overview_does_not_invent_balances_or_returns(portfolio, policy):
    state = portfolio.call('track-owner', 'paper.strategy', policy)
    before = deepcopy(state)
    view = tables(state)['paper.overview']
    assert view['view'] == 'overview'
    assert all(metric['value'] == '—' for metric in view['metrics'])
    assert view['charts'][0]['points'] == []
    assert view['charts'][1]['used'] is None
    assert view['charts'][1]['limit'] is None
    assert state == before


def test_budget_includes_unfilled_buy_reservations_and_ignores_proposal(portfolio, policy, rig):
    approve(portfolio, policy)
    portfolio.call('track-owner', 'paper.ingest', {'week': '2026-09-21'})
    portfolio.call('track-owner', 'paper.decide', rig.plan())
    portfolio.call('track-owner', 'paper.strategy', policy | {'max_portfolio_usd': '20000'})
    [(track, state)] = portfolio.process_once()
    view = overview(state)
    assert view['charts'][1]['used'] == 1000
    assert view['charts'][1]['limit'] == 10000
    assert '待确认' in view['notices'][0]['title']
    assert view['charts'][0]['points'] == []
    assert state['strategy']['active']['settings']['max_portfolio_usd'] == '10000'


def test_events_are_readable_and_full_decision_evidence_is_unchanged(rig):
    rig.decide()
    rig.submit(rig.plan())
    state = rig.fill('order-1', 10, remaining=10)
    before = deepcopy(state)
    view = activity(state)
    fill = next(item for item in view['items'] if item['title'] == '买入已成交')
    assert '10 股' in fill['detail'] and '$100.00' in fill['detail']
    assert 'order_id' not in fill['detail'] and '{' not in fill['detail']
    assert not any('preflight' in item['title'] for item in view['items'])
    assert state == before
    legacy = tables(state)['paper.journal']
    assert all('"decision_id"' not in row['detail'] for row in legacy['rows'])


def test_error_notice_keeps_last_snapshot_visible_without_claiming_freshness(portfolio, policy, rig):
    approve(portfolio, policy)
    [(track, state)] = portfolio.process_once()
    state['error'] = 'broker positions differ from ledger; trading blocked'
    view = overview(state)
    assert view['notices'][0]['tone'] == 'negative'
    assert '最近一次' in view['notices'][0]['detail']
    assert view['updated'] == {'label': '最近对账', 'at': state['snapshot']['at']}


def test_budget_does_not_double_count_partial_entry_fills(portfolio, policy, rig):
    approve(portfolio, policy)
    active = portfolio.call('track-owner', 'paper.status', {})['strategy']['active']
    rig.engine = portfolio.engine(active)
    rig.source = portfolio.call('track-owner', 'paper.ingest', {'week': '2026-09-21'})
    rig.decide()
    rig.submit(rig.plan())
    rig.fill('order-1', 4, remaining=4, status='PartialFilled')
    state = portfolio.call('track-owner', 'paper.status', {})
    view = overview(state)
    assert view['charts'][1]['used'] == 1000  # 400 cost + 600 unfilled buy reservation.
    assert view['charts'][0]['points'] == []
    assert view['metrics'][2]['value'] == '1'
    assert view['metrics'][3]['value'] == '1'


@pytest.mark.parametrize('exit_price,gross,tone', [('105.00', 50, 'positive'), ('97.00', -30, 'negative')])
def test_profit_chart_uses_actual_gross_results_and_clears_closed_cost(portfolio, policy, rig, exit_price, gross, tone):
    approve(portfolio, policy)
    active = portfolio.call('track-owner', 'paper.status', {})['strategy']['active']
    rig.engine = portfolio.engine(active)
    rig.source = portfolio.call('track-owner', 'paper.ingest', {'week': '2026-09-21'})
    rig.decide()
    rig.submit(rig.plan())
    rig.fill('order-1', 10, remaining=10)
    sale = rig.plan(decision_id='exit-1', action='sell', limit_price=exit_price)
    del sale['stop_price'], sale['target_price']
    state = rig.state()
    state['quotes']['SOXX.US']['last'] = exit_price
    state['intraday']['SOXX.US'][0]['price'] = exit_price
    rig.write(state)
    rig.decide(sale)
    rig.submit(sale, 'order-2')
    rig.fill('order-2', 10, price=exit_price, fill_id='exit-fill', remaining=0)
    state = portfolio.call('track-owner', 'paper.status', {})
    view = overview(state)
    assert view['charts'][0]['points'][0]['value'] == gross
    assert view['charts'][0]['points'][0]['tone'] == tone
    assert view['metrics'][1]['tone'] == tone
    assert view['charts'][1]['used'] == 0
    assert state['trades'][0]['net_pnl_usd'] is None
