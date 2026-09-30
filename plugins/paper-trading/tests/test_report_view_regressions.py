from datetime import timedelta
import json

import pytest

from paper_trading.ledger import encoded
from paper_trading.report import tables
from .conftest import NOW
from .test_strategy import account, policy, portfolio, approve  # noqa: F401


def owned_engine(portfolio, policy, rig):
    approve(portfolio, policy)
    rig.engine = portfolio.engine(portfolio.call('track-owner', 'paper.status', {})['strategy']['active'])
    rig.source = portfolio.call('track-owner', 'paper.ingest', {'week': '2026-09-21'})


def test_every_position_alert_remains_accessible_after_summary_limit(portfolio, policy, rig):
    names = ['AAA', 'BBB', 'CCC', 'DDD']
    predictions = [{'id': name, 'issued': '2026-09-21', 'symbol': name, 'direction': 'long',
                    'conviction_tier': 'satellite', 'horizon_end': '2026-09-25'} for name in names]
    (rig.research / 'weekly/2026/predictions.jsonl').write_text('\n'.join(json.dumps(row) for row in predictions))
    policy = policy | {'symbols': [name + '.US' for name in names]}
    owned_engine(portfolio, policy, rig)
    for name in names:
        symbol = name + '.US'
        plan = rig.plan(decision_id='entry-' + name, trade_id='trade-' + name, prediction_id=name, symbol=symbol)
        state = rig.state()
        state['quotes'][symbol] = {'symbol': symbol, 'last': '100.00', 'status': 'Normal'}
        state['intraday'][symbol] = [{'time': NOW.isoformat(), 'price': '100.00'}]
        rig.write(state)
        rig.decide(plan)
        rig.submit(plan, 'order-' + name)
        state = rig.state()
        state['orders']['order-' + name]['status'] = 'Filled'
        state['fills'].append({'trade_id': 'fill-' + name, 'order_id': 'order-' + name,
                               'symbol': symbol, 'quantity': '10', 'price': '100.00', 'time': NOW.isoformat()})
        state['positions'].append({'symbol': symbol, 'quantity': '10', 'available': '10', 'currency': 'USD'})
        rig.write(state)
        assert rig.engine.process_once()['error'] is None
    state = rig.state()
    for name in names[:3]:
        state['intraday'][name + '.US'][0]['time'] = (NOW - timedelta(hours=1)).isoformat()
    state['quotes']['DDD.US']['last'] = '90.00'
    state['intraday']['DDD.US'][0]['price'] = '90.00'
    rig.write(state)
    rig.engine.process_once()
    status = portfolio.call('track-owner', 'paper.status', {})
    assert len(status['alerts']) == 4 and status['alerts'][3]['reason'] == 'stop_crossed'
    view = tables(status)['paper.alert_details']
    assert len(view['table']['rows']) == 4
    assert any(row['symbol'] == 'DDD.US' and '止损' in row['reason'] for row in view['table']['rows'])


def closed_review_parameters(portfolio, policy, rig):
    owned_engine(portfolio, policy, rig)
    rig.decide()
    rig.submit(rig.plan())
    rig.fill('order-1', 10, remaining=10)
    sale = rig.plan(decision_id='exit', action='sell', limit_price='105.00')
    del sale['stop_price'], sale['target_price']
    state = rig.state()
    state['quotes']['SOXX.US']['last'] = '105.00'
    state['intraday']['SOXX.US'][0]['price'] = '105.00'
    rig.write(state)
    rig.decide(sale)
    rig.submit(sale, 'order-exit')
    status = rig.fill('order-exit', 10, price='105.00', fill_id='exit-fill', remaining=0)
    return {'trade_id': 'trade-1', 'evidence_revision': status['trades'][0]['evidence_revision'],
            'analysis': 'Completed fixture trade; gross return excludes fees.',
            'next_action': 'Retain the approved settings and wait for fresh research.'}


def test_newest_review_is_not_selected_by_its_identifier(portfolio, policy, rig):
    review = closed_review_parameters(portfolio, policy, rig)
    for index in range(50):
        rig.engine.call('track-owner', 'paper.review', review | {'review_id': f'z-{index:03}'})
    rig.engine.call('track-owner', 'paper.review', review | {'review_id': 'a-newest'})
    status = portfolio.call('track-owner', 'paper.status', {})
    cards = tables(status)['paper.review_cards']['items']
    assert len(cards) == 50
    assert cards[0]['id'] == 'a-newest'
    assert status['reviews'][-1]['review_id'] == 'a-newest'


@pytest.mark.parametrize('corruption', ['missing', 'mismatch'])
def test_review_audit_integrity_does_not_silently_drop_or_rewrite_records(portfolio, policy, rig, corruption):
    review = closed_review_parameters(portfolio, policy, rig) | {'review_id': 'review'}
    rig.engine.call('track-owner', 'paper.review', review)
    with rig.engine.ledger.session() as db:
        if corruption == 'missing':
            db.execute("DELETE FROM journal WHERE kind='review_added'")
        else:
            db.execute("UPDATE journal SET body=? WHERE kind='review_added'", (encoded(review | {'analysis': 'Mismatched audit text'}),))
    with pytest.raises(ValueError, match='audit|chronology'):
        rig.engine.call('track-owner', 'paper.status', {})
    with rig.engine.ledger.session() as db:
        assert db.execute("SELECT body FROM reviews WHERE id='review'").fetchone()[0] == encoded(review)


@pytest.mark.parametrize('broker_status,title', [('Canceled', '订单取消已确认'), ('Rejected', '订单已被拒绝'), ('Expired', '计划已过期')])
def test_terminal_order_outcomes_are_present_in_activity(portfolio, policy, rig, broker_status, title):
    owned_engine(portfolio, policy, rig)
    rig.decide()
    rig.submit(rig.plan())
    state = rig.state()
    state['orders']['order-1']['status'] = broker_status
    rig.write(state)
    rig.engine.process_once()
    status = portfolio.call('track-owner', 'paper.status', {})
    assert any(item['title'] == title for item in tables(status)['paper.activity']['items'])


def test_unsubmitted_plan_expiry_is_present_in_activity(portfolio, policy, rig):
    owned_engine(portfolio, policy, rig)
    rig.decide()
    portfolio.clock = lambda: NOW + timedelta(hours=2)
    portfolio.process_once()
    status = portfolio.call('track-owner', 'paper.status', {})
    assert status['decisions'][0]['state'] == 'expired'
    assert any(item['title'] == '计划已过期' for item in tables(status)['paper.activity']['items'])


def test_failed_risk_check_is_present_in_activity(portfolio, policy, rig):
    owned_engine(portfolio, policy, rig)
    rig.decide(rig.plan(quantity=30))
    portfolio.process_once()
    status = portfolio.call('track-owner', 'paper.status', {})
    assert 'price risk' in status['decisions'][0]['error']
    assert any(item['title'] == '计划暂未通过检查' for item in tables(status)['paper.activity']['items'])
