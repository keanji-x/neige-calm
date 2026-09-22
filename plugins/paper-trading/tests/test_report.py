"""Populated native tables must satisfy the frontend's declared-column contract."""
import pytest

from paper_trading.report import table, tables


@pytest.mark.parametrize('kind', ['paper.trades', 'paper.alerts', 'paper.reviews'])
def test_populated_report_rows_contain_only_declared_columns(rig, kind):
    rig.decide()
    rig.submit(rig.plan())
    status = rig.fill('order-1', 10, remaining=10)
    if kind == 'paper.alerts':
        state = rig.state()
        state['quotes']['SOXX.US']['last'] = '94.00'
        state['intraday']['SOXX.US'][0]['price'] = '94.00'
        rig.write(state)
        status = rig.engine.process_once()
    elif kind == 'paper.reviews':
        sale = rig.plan(decision_id='exit-1', action='sell', limit_price='105.00')
        del sale['stop_price'], sale['target_price']
        state = rig.state()
        state['quotes']['SOXX.US']['last'] = '105.00'
        state['intraday']['SOXX.US'][0]['price'] = '105.00'
        rig.write(state)
        rig.decide(sale)
        rig.submit(sale, 'order-2')
        status = rig.fill('order-2', 10, price='105.00', fill_id='fill-2', remaining=0)
        rig.engine.call('track-owner', 'paper.review', {
            'review_id': 'review', 'trade_id': 'trade-1',
            'evidence_revision': status['trades'][0]['evidence_revision'],
            'analysis': 'Fixture fills reconciled to gross USD 50.',
            'next_action': 'Retain the approved risk settings for the next research cycle.'})
        status = rig.engine.call('track-owner', 'paper.status', {})
    result = tables(status)[kind]
    keys = {column['key'] for column in result['columns']}
    assert result['rows']
    assert all(set(row) == keys for row in result['rows'])


def test_table_does_not_default_missing_required_cell():
    with pytest.raises(KeyError):
        table([('required', 'Required')], [{}], 'Missing data')
