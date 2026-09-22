"""Read-only report hierarchy; no decisions, approvals, or broker operations."""
from decimal import Decimal
from fractions import Fraction

from .portfolio import TERMINAL, cost_exposure
from .report_text import bounded, event_text, money_text, state_text


def overview(state):
    strategy, snapshot = state['strategy'], state['snapshot']
    active, proposal = strategy['active'], strategy['proposal']
    known = active is not None and snapshot is not None
    gross = sum((Decimal(trade['realized_gross_usd']) for trade in state['trades']), Decimal(0))
    pending = [decision for decision in state['decisions'] if decision['state'] not in TERMINAL]
    held = {trade['symbol'] for trade in state['trades'] if trade['quantity'] > 0}
    metrics = [
        {'label': '账户权益', 'value': money_text(snapshot['account_equity_usd']) if snapshot else '—',
         'detail': f"可用现金 {money_text(snapshot['available_cash_usd'])} · 券商账户总额" if snapshot else '尚未完成账户对账', 'tone': 'neutral'},
        {'label': '已实现毛收益', 'value': money_text(gross, signed=True) if known else '—',
         'detail': '根据本策略成交计算，未计费用',
         'tone': 'positive' if known and gross > 0 else 'negative' if known and gross < 0 else 'neutral'},
        {'label': '持仓标的', 'value': str(len(held)) if known else '—',
         'detail': '当前仍持有的策略标的', 'tone': 'neutral'},
        {'label': '待处理计划', 'value': str(len(pending)) if active else '—',
         'detail': '等待确认、对账或成交', 'tone': 'warning' if pending else 'neutral'},
    ]
    notices = []
    if state['error']:
        notices.append({'title': '账户对账需要关注', 'detail': '当前保留最近一次成功对账的数据，新计划需等待对账恢复。', 'tone': 'negative'})
    unknown = sum(decision['state'] in ('unknown', 'submitting') for decision in pending)
    if unknown:
        notices.append({'title': f'{unknown} 笔订单提交结果待核实', 'detail': '正在核对券商记录，请勿重复提交。', 'tone': 'warning'})
    if active is None:
        notices.append({'title': state_text(strategy['phase']), 'detail': '策略经人工确认后才会生效；当前不会产生新的交易决策。', 'tone': 'warning'})
    elif proposal is not None and proposal['revision'] != active['revision']:
        labels = {'research_root': '研究来源', 'symbols': '交易标的', 'max_order_usd': '单笔订单上限',
                  'max_portfolio_usd': '组合成本上限', 'max_trade_risk_usd': '单笔计划风险',
                  'quote_max_age_seconds': '报价时效', 'max_price_deviation_bps': '价格偏离上限'}
        changed = [labels[key] for key in labels if proposal['settings'][key] != active['settings'][key]]
        notices.append({'title': f'{len(changed)} 项策略调整待确认',
                        'detail': f"{'、'.join(changed)}有修改。原有已确认策略继续生效。", 'tone': 'warning'})
    if state['paused'] and active:
        notices.append({'title': '新建仓已暂停', 'detail': '已有订单、持仓和人工确认的退出操作仍按原有流程处理。', 'tone': 'warning'})
    if state['alerts']:
        names = {trade['trade_id']: trade['symbol'] for trade in state['trades']}
        reasons = {'stop_crossed': '触及止损提醒价', 'target_crossed': '触及目标提醒价',
                   'market_data_unavailable': '行情暂不可用'}
        detail = '；'.join(f"{names.get(alert['trade_id'], '持仓')}：{reasons.get(alert['reason'], '需要核对')}"
                          for alert in state['alerts'][:3])
        if len(state['alerts']) > 3:
            detail += f"；另有 {len(state['alerts']) - 3} 项提醒"
        notices.append({'title': f"{len(state['alerts'])} 项持仓提醒",
                        'detail': detail + '。提醒不是保护性订单，不会自动平仓。', 'tone': 'warning'})
    points = [{'label': f"{trade['symbol']} · 交易{index + 1}（{state_text(trade['state'])}）",
               'value': float(Decimal(trade['realized_gross_usd']))}
              for index, trade in enumerate(state['trades']) if trade['sold'] > 0][-12:]
    used, limit = None, None
    if known:
        exposure = cost_exposure(state['trades'], state['decisions'], state['fills'])
        reserved = sum((Fraction(Decimal(decision['body']['limit_price'])) *
                        (decision['body']['quantity'] - sum(fill['quantity'] for fill in state['fills']
                         if fill['order_id'] == decision['broker_id']))
                        for decision in pending if decision['body']['action'] == 'buy'), Fraction(0))
        # Chart numbers are display-only. Order admission still uses the exact
        # Decimal/Fraction calculations in the execution engine.
        used, limit = float(exposure + reserved), float(Decimal(active['settings']['max_portfolio_usd']))
    return {'version': 1, 'view': 'overview', 'asOf': snapshot['at'] if snapshot else None,
            'metrics': metrics, 'notices': notices,
            'charts': [
                {'kind': 'bars', 'title': '逐笔已实现毛收益', 'unit': 'USD · 最近 12 笔有退出成交的交易 · 未计费用',
                 'emptyText': '暂无退出成交，尚无已实现收益。', 'points': points},
                {'kind': 'budget', 'title': '策略预算使用', 'unit': 'USD', 'used': used, 'limit': limit,
                 'detail': '持仓成本加未完成买单预留金额；不代表市值或最大亏损。'},
            ]}


def activity(state):
    important = {'decision_recorded', 'fill', 'review_added', 'source_ingested', 'pause_changed',
                 'submission_unknown', 'cancel_unknown', 'reconciliation_error', 'cancel_acknowledged'}
    items = [{'id': str(event['seq']), 'at': event['at'], **event_text(event, state['decisions'])}
             for event in state['journal'] if event['kind'] in important][:100]
    return {'version': 1, 'view': 'activity', 'emptyText': '暂无交易动态。', 'items': items}


def reviews(state):
    trades = {trade['trade_id']: trade for trade in state['trades']}
    numbers = {trade['trade_id']: index + 1 for index, trade in enumerate(state['trades'])}
    items = []
    for review in reversed(state['reviews'][-50:]):
        trade = trades.get(review['trade_id'])
        items.append({'id': review['review_id'], 'title': f"{trade['symbol']} · 交易{numbers[review['trade_id']]}复盘" if trade else '交易复盘',
                      'body': bounded(review['analysis'], 8000), 'next': bounded(review['next_action'], 8000),
                      'footer': f"已实现毛收益 {money_text(trade['realized_gross_usd'], signed=True)} · 未计费用" if trade else ''})
    return {'version': 1, 'view': 'cards', 'emptyText': '交易完成后，复盘会显示在这里。', 'items': items}


def details(title, payload, labels):
    settings = {'Status': '策略状态', 'Paper account': '模拟账户', 'Revision (short)': '版本摘要',
                'Research': '研究来源', 'Symbols': '允许交易标的', 'Order limit / USD': '单笔订单上限 / USD',
                'Cost limit / USD': '组合成本上限 / USD', 'Price risk / USD': '单笔计划风险 / USD',
                'Quote age / sec': '报价时效 / 秒', 'Deviation / bps': '价格偏离 / 基点'}
    rows = []
    for row in payload['rows']:
        display = dict(row)
        if 'state' in display:
            display['state'] = state_text(display['state'])
        if 'action' in display:
            display['action'] = {'buy': '买入', 'sell': '卖出', 'hold': '观望'}.get(display['action'], display['action'])
        if 'setting' in display:
            display['setting'] = settings.get(row['setting'], row['setting'])
            if row['setting'] == 'Status':
                display['approved'] = state_text(row['approved'])
        rows.append(display)
    return {'version': 1, 'view': 'details', 'title': title,
            'table': payload | {'rows': rows, 'columns': [column | {'label': labels.get(column['key'], column['label'])}
                                           for column in payload['columns']]}}


def view_payloads(state, legacy):
    return {
        'paper.overview': overview(state),
        'paper.activity': activity(state),
        'paper.review_cards': reviews(state),
        'paper.strategy_details': details('策略参数与待确认修改', legacy['paper.strategy'],
                                         {'setting': '参数', 'approved': '当前已确认', 'proposed': '待确认'}),
        'paper.order_details': details('订单明细', legacy['paper.decisions'],
                                      {'id': '计划', 'symbol': '标的', 'action': '方向', 'quantity': '股数',
                                       'price': '限价', 'state': '状态', 'order': '券商订单', 'error': '需关注'}),
        'paper.trade_details': details('持仓与历史交易', legacy['paper.trades'],
                                      {'trade_id': '交易', 'symbol': '标的', 'quantity': '剩余股数',
                                       'average_entry': '买入均价', 'realized_gross_usd': '已实现毛收益 / USD', 'state': '状态'}),
    }
