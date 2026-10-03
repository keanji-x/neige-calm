"""Every SPY/cash Track overlay: the live overview plus detail tables of targets and broker outcomes."""
from .allocation_views import SIDES, STATES, overview, percent
from .report import table


def tables(state):
    snapshot = state['snapshot'] or {}
    decisions = []
    for decision in state['decisions']:
        request = decision['order_request']
        decisions.append({'id': decision['id'], 'target': percent(decision['body']['target_spy_bps']),
            'state': STATES[decision['state']], 'order': decision['broker_id'] or '',
            'action': SIDES[request['side']] if request else '',
            'quantity': request['quantity'] if request else '', 'rationale': decision['body']['rationale']})
    return {
        'spy.overview': overview(state),
        'spy.portfolio': table([('metric', '项目'), ('value', '当前状态')], [
            {'metric': '账户模式', 'value': '长桥官方模拟账户 · SPY／现金'},
            {'metric': '单次调仓上限', 'value': percent(state['policy']['max_order_bps'])},
            {'metric': '现金保留', 'value': percent(state['policy']['cash_buffer_bps'])},
            {'metric': '现金 / 美元', 'value': snapshot.get('cash_usd', '尚未对账')},
            {'metric': '可用现金 / 美元', 'value': snapshot.get('available_cash_usd', '尚未对账')},
            {'metric': 'SPY 股数', 'value': snapshot.get('shares', '尚未对账')},
            {'metric': '实际 SPY 比例', 'value': percent(snapshot['actual_spy_bps']) if snapshot else '尚未对账'},
            {'metric': '行情时间', 'value': snapshot.get('quote_at', '尚未对账')},
            {'metric': '对账状态', 'value': state['error'] or snapshot.get('at', '尚未对账')},
        ], '以真实券商记录为准；整数股与成交价格可能使实际比例偏离目标。'),
        'spy.decisions': table([('id', '决策'), ('target', '目标 SPY 比例'), ('state', '执行状态'),
                                ('action', '方向'), ('quantity', '股数'), ('order', '委托编号'), ('rationale', '理由')],
                               decisions, '收到委托编号表示券商已受理；成交状态以对账为准。'),
        'spy.fills': table([('trade_id', '成交编号'), ('order_id', '委托编号'), ('quantity', '股数'),
                            ('price', '成交价 / 美元'), ('time', '成交时间')], state['fills'],
                           '实际成交记录；本版本不计算费用和净收益。'),
    }
