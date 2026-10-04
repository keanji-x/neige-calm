"""Live SPY/cash overview: a pure projection of reconciled ledger state; no broker access."""
from datetime import date, timedelta
from decimal import Decimal

from .allocation import NEW_YORK, valuation_date
from .config import timestamp
from .report_text import bounded, money_text
from .report_views import metric, native_view, record, records, row, scalar, unknown


STATES = {'queued': '等待执行', 'requested': '已请求执行', 'submitting': '正在提交', 'working': '委托处理中',
          'settled': '已成交', 'noop': '无需调仓', 'unknown': '结果待核实',
          'rejected': '已拒绝', 'canceled': '已取消', 'expired': '已过期'}
TONES = {'settled': 'positive', 'unknown': 'negative', 'rejected': 'negative',
         'canceled': 'warning', 'expired': 'warning'}
SIDES = {'Buy': '买入', 'Sell': '卖出'}
SPY, BENCHMARK, CASH = 5, 6, 7  # renderer palettes for SPY, its price benchmark and cash
DECISIONS = 50
PENDING = '尚未完成账户对账'


def percent(bps):
    return f'{Decimal(bps) / 100:.2f}%'


def new_york(at):
    return f"{timestamp(at).astimezone(NEW_YORK):%Y-%m-%d %H:%M} 纽约时间"


def ratio(part, whole):
    return float(round(part / whole * 100, 4))


def change(current, previous):
    return float(round(current / previous * 100 - 100, 4))


def tone(amount):
    return 'positive' if amount > 0 else 'negative' if amount < 0 else 'neutral'


def performance(snapshot, previous, samples):
    if snapshot is None:
        items = [metric('nav', '总资产', unknown(PENDING), '', primary=True),
                 metric('previous', '上一交易日估值', unknown(PENDING), ''),
                 metric('pnl', '本日盈亏', unknown(PENDING), ''),
                 metric('change', '本日涨跌', unknown(PENDING), '')]
    else:
        equity, cash = Decimal(snapshot['equity_usd']), Decimal(snapshot['cash_usd'])
        items = [metric('nav', '总资产', scalar(float(equity), decimals=2),
                        f"SPY 市值 {money_text(equity - cash)} · 现金 {money_text(cash)}", primary=True)]
        if previous is None:
            reason = '暂无更早纽约交易日的对账估值'
            items += [metric('previous', '上一交易日估值', unknown(reason), '每个纽约交易日保留最后一次对账'),
                      metric('pnl', '本日盈亏', unknown(reason), '有上一交易日估值后计算'),
                      metric('change', '本日涨跌', unknown(reason), '有上一交易日估值后计算')]
        else:
            before = Decimal(previous['equity_usd'])
            delta = equity - before
            label = date.fromisoformat(previous['date']).strftime('%m.%d')
            items += [metric('previous', f'上一交易日估值 · {label}', scalar(float(before), decimals=2),
                             f"USD · 对账于 {new_york(previous['at'])}"),
                      metric('pnl', '本日盈亏', scalar(float(delta), decimals=2, signed=True),
                             '相对上一交易日估值 · 未扣除费用与出入金', tone(delta))]
            items.append(metric('change', '本日涨跌', scalar(change(equity, before), '%', 2, True, 'suffix'),
                                '相对上一交易日估值', tone(delta)) if before > 0 else
                         metric('change', '本日涨跌', unknown('上一交易日估值为零'), '相对上一交易日估值'))
    assets = {'kind': 'metrics', 'id': 'assets', 'title': '', 'items': items}
    datasets = []
    last = date.fromisoformat(samples[-1]['date']) if samples else None
    ranges = [('all', '全部', samples)]
    recent = [s for s in samples if last and s['date'] >= (last - timedelta(days=30)).isoformat()]
    if len(recent) < len(samples):
        ranges.append(('1m', '1M', recent))
    for key, label, window in ranges:
        datasets.append({'id': 'assets-' + key, 'label': '资产 · ' + label, 'unit': 'USD', 'style': 'line',
                         'series': [{'id': 'portfolio', 'label': '组合', 'palette': SPY}],
                         'points': [{'date': s['date'], 'values': [float(Decimal(s['equity_usd']))]} for s in window]})
        first = window[0] if window else None
        points = []
        for s in window:
            start_equity, start_price = Decimal(first['equity_usd']), Decimal(first['price'])
            points.append({'date': s['date'], 'values': [
                change(Decimal(s['equity_usd']), start_equity) if start_equity > 0 else None,
                change(Decimal(s['price']), start_price)]})
        datasets.append({'id': 'returns-' + key, 'label': '收益 · ' + label, 'unit': '%', 'style': 'line',
                         'series': [{'id': 'portfolio', 'label': '组合', 'palette': SPY},
                                    {'id': 'benchmark', 'label': 'SPY 价格', 'palette': BENCHMARK}],
                         'points': points})
    history = {'kind': 'time-series', 'id': 'nav-history', 'title': '总资产变化',
               'caption': '每个纽约交易日最后一次对账估值；收益视图区间起点归零，SPY 价格收益为基准；未扣除费用与出入金。',
               'emptyText': '完成首次对账后开始记录估值', 'datasets': datasets}
    return row('performance', [assets, history], '01 · 组合表现', 'two-wide-end')


def allocation(snapshot, previous, samples):
    slices, rows = [], []
    if snapshot is not None:
        price, cash, shares = Decimal(snapshot['price']), Decimal(snapshot['cash_usd']), snapshot['shares']
        # Whole dollars keep the renderer's float total exact; the table carries cents.
        slices = [{'id': 'spy', 'label': 'SPY', 'value': float(round(shares * price)), 'palette': SPY},
                  {'id': 'cash', 'label': '现金', 'value': float(round(cash)), 'palette': CASH}]
        rows = [{'name': f'SPY · {shares} 股', 'price': f'{price:,.2f}', 'value': f'{shares * price:,.2f}',
                 'change': f"{change(price, Decimal(previous['price'])):+.2f}%" if previous else '—'},
                {'name': '现金', 'price': '—', 'value': f'{cash:,.2f}', 'change': '—'}]
    distribution = {'kind': 'distribution', 'id': 'weights', 'title': '当前占比', 'unit': 'USD',
                    'emptyText': PENDING, 'slices': slices}
    weighted = [s for s in samples if Decimal(s['equity_usd']) > 0]
    weights = {'kind': 'time-series', 'id': 'weight-history', 'title': '历史仓位', 'caption': '按市值 / 总资产，包含现金。',
               'emptyText': '完成首次对账后开始记录仓位', 'datasets': [
                   {'id': style, 'label': label, 'unit': '%', 'style': style,
                    'series': [{'id': 'spy', 'label': 'SPY', 'palette': SPY},
                               {'id': 'cash', 'label': '现金', 'palette': CASH}],
                    'points': [{'date': s['date'], 'values': [
                        ratio(s['shares'] * Decimal(s['price']), Decimal(s['equity_usd'])),
                        ratio(Decimal(s['cash_usd']), Decimal(s['equity_usd']))]} for s in weighted]}
                   for style, label in [('stacked', '堆叠'), ('line', '折线')]]}
    caption = (f"行情时间 {new_york(snapshot['quote_at'])} · 本日涨跌相对上一交易日估值中的 SPY 价格"
               if snapshot else PENDING)
    holdings = {'kind': 'table', 'id': 'holdings', 'title': '持仓明细', 'table': {
        'columns': [{'key': key, 'label': label, 'align': align} for key, label, align in [
            ('name', '标的', 'left'), ('price', '现价 / USD', 'right'),
            ('value', '市值 / USD', 'right'), ('change', '本日涨跌', 'right')]],
        'rows': rows, 'caption': caption}}
    return row('allocation', [distribution, weights, holdings], '02 · 资金投向', 'three')


def decision_record(decision, fills):
    body, request, state = decision['body'], decision['order_request'], decision['state']
    order = f"{SIDES[request['side']]} {request['quantity']} 股" if request else '未下单'
    owned = [f for f in fills if decision['broker_id'] and f['order_id'] == decision['broker_id']]
    summary = f"{order} SPY · 已成交 {decision['filled_quantity']} 股" if request else STATES[state]
    if decision['error']:
        summary += f" · 需关注：{decision['error']}"
    facts = [('目标 SPY 比例', percent(body['target_spy_bps'])), ('有效期至', new_york(body['valid_until'])),
             ('订单', order), ('委托编号', decision['broker_id'] or '—'),
             ('券商状态', decision['broker_status'] or '—'), ('创建时间', new_york(decision['created_at']))]
    return record(decision['id'], f"SPY 目标 {percent(body['target_spy_bps'])}", bounded(summary, 8000),
                  subtitle='SPY／现金目标比例',
                  badges=[{'label': '执行状态', 'value': STATES[state], 'tone': TONES.get(state, 'neutral')}],
                  facts=[{'label': label, 'value': bounded(value)} for label, value in facts],
                  sections=[{'label': '理由', 'body': body['rationale']},
                            {'label': '来源', 'body': bounded('\n'.join(body['source_refs']), 8000)}],
                  disclosures=[{'id': f'fill-{index + 1}', 'label': f"成交 · {new_york(fill['time'])}",
                                'body': bounded(f"{fill['quantity']} 股 @ ${fill['price']} · 成交编号 {fill['trade_id']}", 8000),
                                'note': '以券商成交记录为准；未计费用。', 'tone': 'neutral'}
                               for index, fill in enumerate(owned[:20])])


def overview(state):
    snapshot, samples = state['snapshot'], state['valuations']
    today = valuation_date(snapshot['quote_at']) if snapshot else None
    previous = next((s for s in reversed(samples) if today and s['date'] < today), None)
    items = [decision_record(d, state['fills']) for d in reversed(state['decisions'][-DECISIONS:])]
    decisions = records('decisions', '', 'Planner 保存目标比例后，调仓决策会显示在这里。', items, label='调仓决策',
                        description=f'最近 {DECISIONS} 项决策，最新在前；每项最多列出 20 笔成交。')
    description = '长桥官方模拟账户 · USD · 数值为对账估值，盈亏未扣除费用与出入金。'
    if state['error']:
        description += ' 最近一次对账失败，当前显示上次成功对账的数据。'
    return native_view(state, 'SPY 与现金组合', [
        performance(snapshot, previous, samples), allocation(snapshot, previous, samples),
        row('decisions', [decisions], '03 · 调仓决策', 'one')], description)
