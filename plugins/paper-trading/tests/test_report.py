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


@pytest.mark.parametrize('text', ['x' * 1900, '\U0001f642' * 2500], ids=['json-overhead', 'unicode'])
def test_journal_bounds_readable_text_without_changing_decision(rig, text):
    rig.decide(rig.plan(rationale=text))
    state = rig.engine.call('track-owner', 'paper.status', {})
    assert state['decisions'][0]['body']['rationale'] == text
    detail = next(row['detail'] for row in tables(state)['paper.journal']['rows']
                  if row['kind'] == 'decision_recorded')
    assert len(detail) <= 2048
    assert '"decision_id"' not in detail
    if len(text) > 2048:
        assert detail.endswith('[truncated]')
    else:
        assert detail.endswith(text)
    assert next(event['body']['rationale'] for event in state['journal']
                if event['kind'] == 'decision_recorded') == text


@pytest.mark.parametrize('text', ['Review ' + 'x' * 2500, '\U0001f642' * 2500], ids=['ascii', 'unicode'])
def test_long_review_display_is_bounded_and_original_is_retained(rig, text):
    rig.decide()
    rig.submit(rig.plan())
    rig.fill('order-1', 10, remaining=10)
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
        'review_id': 'long-review', 'trade_id': 'trade-1',
        'evidence_revision': status['trades'][0]['evidence_revision'],
        'analysis': text, 'next_action': text})
    status = rig.engine.call('track-owner', 'paper.status', {})
    for kind in ('paper.reviews', 'paper.journal'):
        assert all(len(value) <= 2048 for row in tables(status)[kind]['rows']
                   for value in row.values() if isinstance(value, str))
    assert tables(status)['paper.reviews']['rows'][0]['analysis'].endswith('[truncated]')
    assert status['reviews'][0]['analysis'] == text
    assert next(event['body']['analysis'] for event in status['journal'] if event['kind'] == 'review_added') == text


@pytest.mark.parametrize('size', [2047, 2048, 2049])
def test_table_cell_unicode_boundary(size):
    text = '\U0001f642' * size
    displayed = table([('text', 'Text')], [{'text': text}], 'Boundary')['rows'][0]['text']
    assert len(displayed) <= 2048
    assert (displayed == text) is (size <= 2048)
