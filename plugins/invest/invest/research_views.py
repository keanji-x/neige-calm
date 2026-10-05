"""Thesis and research data units (#2104 §3.6, §3.7): `thesis.board` on the portfolio Track, and
`instrument.position` and `thesis.records` on a research Track, refreshed on its attested calls.

The board holds one record per counted symbol, held ones first by market value, then watched ones: the
top 99, then one 其他 record counting the rest's open theses by assessment. Each open thesis is a
section labeled with its assessment and title, so an assessment change shows on the board.
"""
from collections import Counter
from decimal import Decimal

from .report_text import bounded, new_york
from .report_views import metric, record, records, scalar, unit, unknown
from .symbols import unit_id

BOARD = 99  # records; one more slot holds 其他
ASSESSMENTS = {'open': '待评估', 'holding': '成立', 'at_risk': '承压', 'broken': '已证伪'}
ASSESSMENT_TONES = {'holding': 'positive', 'at_risk': 'warning', 'broken': 'negative'}
STANCES = {'bullish': '看多', 'bearish': '看空', 'neutral': '中性'}
PENDING = '尚未完成账户对账'


def label(thesis):
    """`<assessment> · <title>`: a title of at most 110 characters fits the 120-character label."""
    return f"{ASSESSMENTS[thesis['assessment']]} · {thesis['title']}"


def research_badge(instrument):
    if instrument['state'] != 'live':
        return {'label': '研究 Track', 'value': '待核实标的后创建', 'tone': 'neutral'}
    if instrument['stale']:
        return {'label': '研究 Track', 'value': '需续期', 'tone': 'warning'}
    if instrument['last_seen_at'] is None:
        return {'label': '研究 Track', 'value': '等待首次调用', 'tone': 'neutral'}
    return {'label': '研究 Track', 'value': '运行中', 'tone': 'positive'}


def board_record(instrument, open_theses):
    symbol = instrument['symbol']
    return record(unit_id(symbol), symbol, f'{len(open_theses)} 项论点' if open_theses else '尚无论点',
                  subtitle='持有' if instrument['held'] else '关注',
                  badges=[research_badge(instrument)],
                  sections=[{'label': label(t), 'body': t['summary']} for t in open_theses])


def board(state):
    counted = [i for i in state['instruments'] if i['state'] in ('pending', 'live')]
    positions = state['snapshot']['positions'] if state['snapshot'] else {}
    value = {s: Decimal(p['value_usd']) for s, p in positions.items()}
    order = sorted(counted, key=lambda i: (not i['held'], -value.get(i['symbol'], Decimal(0)),
                                           -state['targets'].get(i['symbol'], 0), i['symbol']))
    by_symbol = {}
    for thesis in state['theses']:
        by_symbol.setdefault(thesis['symbol'], []).append(thesis)
    shown, rest = (order, []) if len(order) <= BOARD + 1 else (order[:BOARD], order[BOARD:])
    items = [board_record(i, by_symbol.get(i['symbol'], [])) for i in shown]
    if rest:
        counts = Counter(t['assessment'] for i in rest for t in by_symbol.get(i['symbol'], []))
        summary = ' · '.join(f'{text} {counts[key]}' for key, text in ASSESSMENTS.items())
        items.append(record('other', f'其他 · {len(rest)} 个标的', f'未展开标的的论点：{summary}'))
    return records('thesis-board', '', '组合 Track 覆盖标的后，研究论点会显示在这里。', items, label='研究论点',
                   description=f'每个覆盖标的一项，持有标的按市值在前；最多 {BOARD} 项，其余合并为其他。')


def position(view):
    held, now = view['position'], view['snapshot']
    coverage = metric('coverage', '覆盖', {'state': 'text', 'text': '持有' if view['held'] else '关注'},
                      f"研究密钥 {view['key']}")
    target = metric('target', '目标占比', scalar(view['target_bps'] / 100, '%', 2, placement='suffix'),
                    '最新调仓决策的目标权重')
    if held is None:
        items = [metric('shares', '持仓', unknown(PENDING), '', primary=True),
                 metric('value', '市值', unknown(PENDING), ''), metric('weight', '实际占比', unknown(PENDING), '')]
    else:
        weight = (scalar(float(Decimal(held['weight_bps']) / 100), '%', 2, placement='suffix')
                  if held['weight_bps'] is not None else unknown('组合总资产为零'))
        items = [metric('shares', '持仓', {'state': 'text', 'text': f"{held['shares']} 股"},
                        f"对账于 {new_york(now['at'])}", primary=True),
                 metric('value', '市值', scalar(float(Decimal(held['value_usd'])), decimals=2), 'USD'),
                 metric('weight', '实际占比', weight, '占组合总资产')]
    return {'kind': 'metrics', 'id': 'position', 'title': '', 'items': items + [target, coverage]}


def thesis_record(thesis):
    retired = thesis['retired_at'] is not None
    return record(thesis['thesis_id'], thesis['title'], thesis['summary'],
                  subtitle=STANCES[thesis['stance']],
                  badges=[{'label': '评估', 'value': ASSESSMENTS[thesis['assessment']],
                           'tone': ASSESSMENT_TONES.get(thesis['assessment'], 'neutral')},
                          {'label': '状态', 'value': '已退役' if retired else '进行中', 'tone': 'neutral'},
                          {'label': '版本', 'value': str(thesis['version']), 'tone': 'neutral'}],
                  sections=[{'label': '论点', 'body': thesis['body']},
                            {'label': '来源', 'body': bounded('\n'.join(thesis['source_refs']), 8000)}])


def research_units(view):
    """The research Track's units, from its `instrument_status` view; the caller is the overlay target."""
    state = {'snapshot': view['snapshot']}
    cell = records('theses', '', '组合 Track 提出论点后显示在这里。', [thesis_record(t) for t in view['theses']],
                   label=f"{view['symbol']} 论点", description='进行中的论点在前，其后是最近退役的论点。')
    return {'instrument.position': unit(state, position(view)), 'thesis.records': unit(state, cell)}
