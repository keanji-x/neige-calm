"""Live portfolio data units: pure projections of reconciled ledger state; no broker access.

Each published kind is one cell with its labels, units, tones and empty text; the recipe's template
views place them. Any configured number of held symbols fits the unit contracts by aggregation
(#2104 §3.7): the top N entries by value, then one 其他 entry that is the exact sum of what it
replaces, so every projection conserves value. Amounts are apportioned in cents and shares of
equity in 0.0001%, so the published entries sum exactly to equity and to 100%.
"""
from datetime import date, timedelta
from decimal import Decimal, ROUND_FLOOR

from .config import timestamp
from .reconcile import NEW_YORK
from .report_text import bounded, money_text
from .report_views import metric, record, records, scalar, table, unit, unknown
from .symbols import unit_id

STATES = {'queued': '等待执行', 'requested': '已请求执行', 'working': '执行中', 'done': '已完成',
          'noop': '无需调仓', 'expired': '已过期'}
LEG_STATES = {'submitting': '正在提交', 'working': '委托处理中', 'settled': '已成交', 'canceled': '已取消',
              'rejected': '已拒绝', 'expired': '已过期', 'unknown': '结果待核实'}
TONES = {'done': 'positive', 'settled': 'positive', 'unknown': 'negative', 'rejected': 'negative',
         'canceled': 'warning', 'expired': 'warning'}
SIDES = {'Buy': '买入', 'Sell': '卖出'}
SYMBOL_PALETTES, OTHER_PALETTE, CASH_PALETTE = (1, 2, 3, 4, 5), 6, 7
SLICES, SERIES, FACTS, DISCLOSURES = 10, 4, 11, 19  # top N; one more slot holds 其他 (§3.7)
DECISIONS, FILLS = 50, 500
PENDING = '尚未完成账户对账'
OTHER, CASH = '其他', '现金'
FILL_NOTE = '以券商成交记录为准；未计费用。'


def ranked(values, n):
    """`(top, rest)` by value, then key: every entry when n + 1 slots hold them all."""
    order = sorted(values.items(), key=lambda kv: (-kv[1], kv[0]))
    return (order, []) if len(order) <= n + 1 else (order[:n], order[n:])


CENT, PERCENT_STEP = Decimal('0.01'), Decimal('0.0001')
CASH_KEY = 'cash'  # never a `VENUE:CODE` symbol


def apportion(values, total, step):
    """`values` in multiples of `step` summing exactly to `total`, by largest remainder: each entry
    moves by less than one step, and none is rounded on its own."""
    floors = {k: (v / step).to_integral_value(rounding=ROUND_FLOOR) for k, v in values.items()}
    missing = int(total / step - sum(floors.values()))
    for key in sorted(values, key=lambda k: (-(values[k] / step - floors[k]), k))[:missing]:
        floors[key] += 1
    return {k: n * step for k, n in floors.items()}


def percent(bps):
    return f'{Decimal(bps) / 100:.2f}%'


def new_york(at):
    return f"{timestamp(at).astimezone(NEW_YORK):%Y-%m-%d %H:%M} 纽约时间"


def change(current, previous):
    return float(round(current / previous * 100 - 100, 4))


def tone(amount):
    return 'positive' if amount > 0 else 'negative' if amount < 0 else 'neutral'


def palette(index):
    return SYMBOL_PALETTES[index % len(SYMBOL_PALETTES)]


def values_of(positions):
    return {s: Decimal(p['shares']) * Decimal(p['price']) for s, p in positions.items()}


def nav(snapshot, previous):
    if snapshot is None:
        items = [metric('nav', '总资产', unknown(PENDING), '', primary=True),
                 metric('previous', '上一交易日估值', unknown(PENDING), ''),
                 metric('pnl', '本日盈亏', unknown(PENDING), ''),
                 metric('change', '本日涨跌', unknown(PENDING), '')]
        return {'kind': 'metrics', 'id': 'nav', 'title': '', 'items': items}
    equity, cash = Decimal(snapshot['equity_usd']), Decimal(snapshot['cash_usd'])
    items = [metric('nav', '总资产', scalar(float(equity), decimals=2),
                    f"持仓市值 {money_text(equity - cash)} · 现金 {money_text(cash)}", primary=True)]
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
    return {'kind': 'metrics', 'id': 'nav', 'title': '', 'items': items}


def nav_history(samples):
    last = date.fromisoformat(samples[-1]['date']) if samples else None
    ranges = [('all', '全部', samples)]
    recent = [s for s in samples if last and s['date'] >= (last - timedelta(days=30)).isoformat()]
    if len(recent) < len(samples):
        ranges.append(('1m', '1M', recent))
    series = [{'id': 'portfolio', 'label': '组合', 'palette': 1}]
    datasets = []
    for key, label, window in ranges:
        start = Decimal(window[0]['equity_usd']) if window else None
        datasets.append({'id': 'assets-' + key, 'label': '资产 · ' + label, 'unit': 'USD', 'style': 'line',
                         'series': series,
                         'points': [{'date': s['date'], 'values': [float(Decimal(s['equity_usd']))]} for s in window]})
        datasets.append({'id': 'returns-' + key, 'label': '收益 · ' + label, 'unit': '%', 'style': 'line',
                         'series': series,
                         'points': [{'date': s['date'], 'values': [
                             change(Decimal(s['equity_usd']), start) if start > 0 else None]} for s in window]})
    return {'kind': 'time-series', 'id': 'nav-history', 'title': '总资产变化',
            'caption': '每个纽约交易日最后一次对账估值；收益视图区间起点归零；未扣除费用与出入金。',
            'emptyText': '完成首次对账后开始记录估值', 'datasets': datasets}


def weights(snapshot):
    slices = []
    if snapshot is not None:
        exact = values_of(snapshot['positions'])
        equity = Decimal(snapshot['equity_usd'])
        cents = apportion(exact | {CASH_KEY: Decimal(snapshot['cash_usd'])}, equity.quantize(CENT), CENT)
        top, rest = ranked(exact, SLICES)
        slices = [{'id': unit_id(s), 'label': s, 'value': float(cents[s]), 'palette': palette(i)}
                  for i, (s, _) in enumerate(top)]
        if rest:
            slices.append({'id': 'other', 'label': f'{OTHER} · {len(rest)} 个标的',
                           'value': float(sum(cents[s] for s, _ in rest)), 'palette': OTHER_PALETTE})
        slices.append({'id': 'cash', 'label': CASH, 'value': float(cents[CASH_KEY]), 'palette': CASH_PALETTE})
    return {'kind': 'distribution', 'id': 'weights', 'title': '当前占比', 'unit': 'USD',
            'emptyText': PENDING, 'slices': slices}


def weight_history(samples, snapshot):
    current = values_of(snapshot['positions']) if snapshot else {}
    everyone = set(current) | {s for sample in samples for s in sample['positions']}
    top, rest = ranked({s: current.get(s, Decimal(0)) for s in everyone}, SERIES)
    shown = [s for s, _ in top]
    series = [{'id': unit_id(s), 'label': s, 'palette': palette(i)} for i, s in enumerate(shown)]
    series += ([{'id': 'other', 'label': OTHER, 'palette': OTHER_PALETTE}] if rest else [])
    series.append({'id': 'cash', 'label': CASH, 'palette': CASH_PALETTE})
    points = []
    for sample in samples:
        equity = Decimal(sample['equity_usd'])
        if equity <= 0:
            continue
        held = values_of(sample['positions']) | {CASH_KEY: Decimal(sample['cash_usd'])}
        share = apportion({s: v / equity * 100 for s, v in held.items()}, Decimal(100), PERCENT_STEP)
        values = [float(share.get(s, Decimal(0))) for s in shown]
        if rest:
            values.append(float(sum((v for s, v in share.items() if s != CASH_KEY and s not in shown), Decimal(0))))
        points.append({'date': sample['date'], 'values': values + [float(share[CASH_KEY])]})
    return {'kind': 'time-series', 'id': 'weight-history', 'title': '历史仓位',
            'caption': f'按市值 / 总资产，包含现金；当前市值前 {SERIES} 名单列，其余合并为{OTHER}。',
            'emptyText': '完成首次对账后开始记录仓位', 'datasets': [
                {'id': style, 'label': label, 'unit': '%', 'style': style, 'series': series, 'points': points}
                for style, label in [('stacked', '堆叠'), ('line', '折线')]]}


def holdings(snapshot, previous, targets):
    rows = []
    if snapshot is not None:
        equity = Decimal(snapshot['equity_usd'])
        before = previous['positions'] if previous else {}
        for symbol, value in sorted(values_of(snapshot['positions']).items(), key=lambda kv: (-kv[1], kv[0])):
            held, price = snapshot['positions'][symbol], Decimal(snapshot['positions'][symbol]['price'])
            rows.append({'name': f"{symbol} · {held['shares']} 股", 'price': f'{price:,.2f}', 'value': f'{value:,.2f}',
                         'weight': percent(value / equity * 10000) if equity else '—',
                         'target': percent(targets[symbol]) if symbol in targets else '—',
                         'change': f"{change(price, Decimal(before[symbol]['price'])):+.2f}%" if symbol in before else '—'})
        cash = Decimal(snapshot['cash_usd'])
        rows.append({'name': CASH, 'price': '—', 'value': f'{cash:,.2f}',
                     'weight': percent(cash / equity * 10000) if equity else '—', 'target': '—', 'change': '—'})
    caption = (f"按市值排序 · 对账于 {new_york(snapshot['at'])} · 本日涨跌相对上一交易日估值中的价格 · "
               '以券商记录为准；整数股与成交价格可能使实际比例偏离目标。' if snapshot else PENDING)
    return {'kind': 'table', 'id': 'holdings', 'title': '持仓明细', 'table': {
        'columns': [{'key': key, 'label': label, 'align': align} for key, label, align in [
            ('name', '标的', 'left'), ('price', '现价 / USD', 'right'), ('value', '市值 / USD', 'right'),
            ('weight', '实际占比', 'right'), ('target', '目标占比', 'right'), ('change', '本日涨跌', 'right')]],
        'rows': rows, 'caption': caption}}


def leg_disclosure(order):
    amount = Decimal(order['filled_amount_usd'])  # every fill of the leg, not the fill log's window
    request, state = order['request'], order['state']
    body = (f"委托 {request['quantity']} 股 · 已成交 {order['filled_quantity']} 股 · 成交金额 {money_text(amount)}"
            f" · 委托编号 {order['broker_id'] or '—'}" + (f" · 需关注：{order['error']}" if order['error'] else ''))
    return {'id': order['id'], 'label': f"{SIDES[request['side']]} {order['symbol']} · {LEG_STATES[state]}",
            'body': bounded(body, 8000), 'note': FILL_NOTE, 'tone': TONES.get(state, 'neutral')}, amount


def decision_record(decision):
    body, state, orders = decision['body'], decision['state'], decision['orders']
    top, rest = ranked(body['weights'], FACTS)
    facts = [{'label': s, 'value': percent(bps)} for s, bps in top]
    if rest:
        facts.append({'label': f'{OTHER} · {len(rest)} 个标的', 'value': percent(sum(b for _, b in rest))})
    legs = {order['id']: leg_disclosure(order) for order in orders}
    shown, hidden = ranked({key: amount for key, (_, amount) in legs.items()}, DISCLOSURES)
    disclosures = [legs[key][0] for key, _ in shown]
    if hidden:
        disclosures.append({'id': 'other', 'label': f'{OTHER} {len(hidden)} 笔委托',
                            'body': f"成交金额合计 {money_text(sum(a for _, a in hidden))}",
                            'note': FILL_NOTE, 'tone': 'neutral'})
    settled = sum(o['state'] == 'settled' for o in orders)
    # `done` only says every leg resolved; a decision that filled nothing is not a success.
    traded = any(o['filled_quantity'] for o in orders)
    label, badge_tone = STATES[state], TONES.get(state, 'neutral')
    if state == 'done' and not traded:
        label, badge_tone = '已结束 · 未成交', 'warning'
    summary = f'{len(orders)} 笔委托 · 已成交 {settled} 笔' if orders else STATES[state]
    if decision['error']:
        summary += f" · 需关注：{decision['error']}"
    return record(decision['id'], f"目标 · {len(body['weights'])} 个标的 · 合计 {percent(sum(body['weights'].values()))}",
                  bounded(summary, 8000), subtitle='目标权重',
                  badges=[{'label': '执行状态', 'value': label, 'tone': badge_tone},
                          {'label': '创建时间', 'value': new_york(decision['created_at']), 'tone': 'neutral'},
                          {'label': '有效期至', 'value': new_york(body['valid_until']), 'tone': 'neutral'}],
                  facts=facts,
                  sections=[{'label': '理由', 'body': bounded(body['message'], 8000)},
                            {'label': '来源', 'body': bounded('\n'.join(body['source_refs']), 8000)}],
                  disclosures=disclosures)


def decision_log(decisions):
    items = [decision_record(d) for d in reversed(decisions[-DECISIONS:])]
    return records('decisions', '', 'Planner 保存目标权重后，调仓决策会显示在这里。', items, label='调仓决策',
                   description=f'最近 {DECISIONS} 项决策，最新在前；每项按成交金额列出委托，其余合并为{OTHER}。'
                               '收到委托编号表示券商已受理；成交状态以对账为准。')


def fill_log(fills):
    latest = fills[-FILLS:]
    listed = f'最近 {len(latest)} 笔成交，最新在前' if latest else '尚无成交记录'
    return {'kind': 'table', 'id': 'fills', 'title': '', 'table': table(
        [('trade_id', '成交编号'), ('order_id', '委托编号'), ('symbol', '标的'), ('quantity', '股数'),
         ('price', '成交价 / 美元'), ('time', '成交时间')],
        [f | {'time': new_york(f['time'])} for f in reversed(latest)],
        f'{listed}；以券商成交记录为准；本版本不计算费用和净收益。')}


def policy(key, label, bps, detail):
    return metric(key, label, scalar(float(Decimal(bps) / 100), '%', 2, placement='suffix'), detail)


def account(state):
    snapshot, error, limits = state['snapshot'], state['error'], state['limits']
    if error:
        kept = f"当前显示 {new_york(snapshot['at'])} 对账的数据" if snapshot else '尚无成功对账的数据'
        reconciled = metric('reconciliation', '对账状态', {'state': 'text', 'text': bounded(error)},
                            f'最近一次对账失败；{kept}。', 'negative')
    elif snapshot:
        reconciled = metric('reconciliation', '对账状态', {'state': 'text', 'text': new_york(snapshot['at'])},
                            '最近一次成功对账；数值为对账估值')
    else:
        reconciled = metric('reconciliation', '对账状态', unknown(PENDING), '')
    available = scalar(float(Decimal(snapshot['available_cash_usd'])), decimals=2) if snapshot else unknown(PENDING)
    return {'kind': 'metrics', 'id': 'account', 'title': '', 'items': [
        reconciled,
        metric('held', '持有标的', {'state': 'text', 'text': f"{limits['held']} / {limits['max_held']}"},
               '目标权重大于零或仍有持仓；上限为 max_held'),
        metric('watched', '关注标的', {'state': 'text', 'text': f"{limits['watched']} / {limits['max_watched']}"},
               '已覆盖但未持有；上限为 max_watched'),
        policy('max-order', '单笔调仓上限', state['policy']['max_order_bps'], '占账户总值；每个标的每项决策最多一笔订单'),
        policy('cash-buffer', '现金保留', state['policy']['cash_buffer_bps'], '占账户总值；目标权重合计不超过 100% 减此值'),
        metric('available-cash', '可用现金', available, 'USD · 券商报告的已交收可用现金')]}


def units(state):
    """Every portfolio Track overlay: one data unit per kind, from the newest reconciled state."""
    snapshot, samples = state['snapshot'], state['valuations']
    today = snapshot['date'] if snapshot else None
    previous = next((s for s in reversed(samples) if today and s['date'] < today), None)
    cells = {'portfolio.nav': nav(snapshot, previous), 'portfolio.nav_history': nav_history(samples),
             'portfolio.weights': weights(snapshot), 'portfolio.weight_history': weight_history(samples, snapshot),
             'portfolio.holdings': holdings(snapshot, previous, state['targets']),
             'portfolio.decision_log': decision_log(state['decisions']),
             'portfolio.fill_log': fill_log(state['fills']), 'portfolio.account': account(state)}
    return {kind: unit(state, cell) for kind, cell in cells.items()}
