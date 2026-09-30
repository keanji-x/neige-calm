"""Plain-language display copy. Structured ledger facts remain unchanged."""
from decimal import Decimal


def money_text(value, signed=False):
    amount = Decimal(value)
    prefix = '-' if amount < 0 else '+' if signed and amount > 0 else ''
    return f'{prefix}${abs(amount):,.2f}'


def bounded(value, limit=2048):
    suffix = '... [truncated]'
    return value if len(value) <= limit else value[:limit - len(suffix)] + suffix


def state_text(value):
    return {'queued': '等待风控检查', 'ready': '等待人工确认', 'working': '等待成交',
            'settled': '已完成成交', 'canceled': '已取消', 'rejected': '已拒绝',
            'expired': '已过期', 'recorded': '已记录观望', 'submitting': '正在提交',
            'unknown': '提交结果待核实', 'open': '持仓中', 'closed': '已平仓',
            'pending': '交易进行中', 'not_entered': '尚未建仓',
            'approved': '已确认', 'unconfigured': '尚未设置策略',
            'awaiting_approval': '存在待确认草案', 'migration_required': '需要迁移旧配置'}.get(value, '未知状态')


def event_text(event, decisions):
    body, kind = event['body'], event['kind']
    plan = next((item['body'] for item in decisions if item['id'] == body.get('decision_id')
                 or item['broker_id'] is not None and item['broker_id'] == body.get('order_id')), {})
    symbol = body.get('symbol') or plan.get('symbol') or '交易计划'
    tone = 'neutral'
    if kind == 'decision_recorded':
        action = {'buy': '买入', 'sell': '卖出', 'hold': '观望'}[body['action']]
        title = f'提出{action}计划'
        detail = f"{symbol} · {body['rationale']}"
        if body['action'] != 'hold':
            detail = f"{symbol} · {body['quantity']} 股 · 限价 {money_text(body['limit_price'])}\n{body['rationale']}"
    elif kind == 'fill':
        side = {'buy': '买入', 'sell': '卖出'}.get(plan.get('action'), '订单')
        title, tone = f'{side}已成交', 'positive'
        detail = f"{symbol} · {body['quantity']} 股 · 成交价 {money_text(body['price'])}"
    elif kind == 'review_added':
        title, detail = '完成交易复盘', body['analysis']
    elif kind == 'source_ingested':
        title, detail = '导入研究报告', f"研究日期：{body['week']}"
    elif kind == 'pause_changed':
        title = '暂停新建仓' if body['paused'] else '恢复新建仓'
        detail = '现有持仓与订单仍继续对账。'
        tone = 'warning' if body['paused'] else 'neutral'
    elif kind in ('preflight', 'operator_preflight'):
        title, detail = '风控检查通过', f'{symbol} · 已检查报价、资金及策略限制，尚不代表订单成交。'
    elif kind == 'decision_state':
        title, detail = '计划状态更新', f"{symbol} · {state_text(body['state'])}"
        tone = 'warning' if body['state'] in ('unknown', 'rejected', 'expired') else 'neutral'
        outcomes = {'canceled': '订单取消已确认', 'rejected': '订单已被拒绝', 'expired': '计划已过期'}
        if body['state'] in outcomes:
            title = outcomes[body['state']]
            detail += '。已经产生的成交仍保留在交易记录中。'
        elif body['state'] == 'queued' and body.get('error'):
            title, tone = '计划暂未通过检查', 'warning'
            detail += '。' + body['error']
    elif kind in ('submission_unknown', 'cancel_unknown'):
        title, detail, tone = '订单结果待核实', f'{symbol} · 正在核对券商记录，请勿重复提交。', 'warning'
    elif kind == 'reconciliation_error':
        title, detail, tone = '账户对账异常', '账户记录暂时未能核对一致，保留上次已确认数据。', 'negative'
    elif kind == 'submission_started':
        title, detail = '开始提交订单', f'{symbol} · 正在等待券商响应。'
    elif kind == 'submission_acknowledged':
        title, detail = '券商已接收订单', f'{symbol} · 已取得订单回执，成交仍以券商执行记录为准。'
    elif kind == 'cancel_requested':
        title, detail = '请求取消订单', f'{symbol} · 正在等待券商确认。'
    elif kind == 'cancel_acknowledged':
        title, detail = '券商已接收取消请求', f'{symbol} · 最终状态以对账结果为准。'
    elif kind == 'decision_strategy_bound':
        title, detail = '记录策略依据', f'{symbol} · 已关联当时生效的策略版本。'
    else:
        title, detail = '交易记录已更新', '完整事件保留在审计记录中。'
    return {'title': title, 'detail': bounded(detail), 'tone': tone}
