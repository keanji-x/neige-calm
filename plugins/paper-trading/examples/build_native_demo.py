"""Author the fictional App data as one native Neige view block. No broker access."""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path

ROOT = Path(__file__).parent

def scalar(amount, unit='$', decimals=0, signed=False, placement='prefix'):
    return {'state': 'known', 'amount': amount, 'unit': unit, 'decimals': decimals,
            'signed': signed, 'placement': placement}

def create_view(facts):
    ui = facts['ui']
    assets, total = ui['assets'], ui['total']
    def metric(key, label, value, detail, tone='neutral', primary=False):
        return {'id': key, 'label': label, 'value': value, 'detail': detail, 'tone': tone,
                'emphasis': 'primary' if primary else 'normal'}
    metrics = {'kind': 'metrics', 'id': 'assets', 'title': '资产摘要', 'items': [
        metric('nav', '总资产', scalar(total), '证券 $884,620 · 现金 $200,000', primary=True),
        metric('previous', '上一交易日收盘 · 09.18', scalar(ui['previous']), 'USD · 演示交易日'),
        metric('pnl', '本日盈亏 · 09.21', scalar(ui['daily'], signed=True), '区间净入金 $0', 'positive'),
        metric('change', '本日涨跌', scalar(ui['daily'] / ui['previous'] * 100, '%', 2, True, 'suffix'), '相对上一交易日', 'positive'),
    ]}
    def date(row):
        return datetime.fromisoformat(row['at'].replace('Z', '+00:00')).date().isoformat()
    nav_sets = []
    for period, samples in [('3M', ui['snapshots']), ('1M', ui['snapshots'][8:])]:
        nav_sets.append({'id': 'assets-' + period, 'label': '资产 · ' + period, 'unit': 'USD', 'style': 'line',
            'series': [{'id': 'portfolio', 'label': '组合', 'palette': 1}],
            'points': [{'date': date(row), 'values': [row['nav']]} for row in samples]})
        nav_sets.append({'id': 'returns-' + period, 'label': '收益 · ' + period, 'unit': '%', 'style': 'line',
            'series': [{'id': 'portfolio', 'label': '组合', 'palette': 1}, {'id': 'benchmark', 'label': '模拟基准', 'palette': 6}],
            'points': [{'date': date(row), 'values': [round((row['nav'] / samples[0]['nav'] - 1) * 100, 4),
                       round((row['benchmark'] / samples[0]['benchmark'] - 1) * 100, 4)]} for row in samples]})
    nav = {'kind': 'time-series', 'id': 'nav-history', 'title': '总资产变化', 'caption': '虚构历史估值；收益视图区间起点归零。',
           'emptyText': '暂无估值记录', 'datasets': nav_sets}
    distribution = {'kind': 'distribution', 'id': 'weights', 'title': '当前占比', 'unit': 'USD', 'emptyText': '暂无持仓数据',
        'slices': [{'id': a['id'], 'label': a['name'], 'value': a['value'], 'palette': i + 1} for i, a in enumerate(assets)]}
    weight_sets = [{'id': style, 'label': label, 'unit': '%', 'style': style,
        'series': [{'id': a['id'], 'label': a['name'], 'palette': i + 1} for i, a in enumerate(assets)],
        'points': [{'date': date(row), 'values': row['weights']} for row in ui['snapshots']]}
        for style, label in [('stacked', '堆叠'), ('line', '折线')]]
    weight_chart = {'kind': 'time-series', 'id': 'weight-history', 'title': '历史仓位', 'caption': '按市值 / 总资产，包含现金。',
                    'emptyText': '暂无历史仓位', 'datasets': weight_sets}
    table = {'kind': 'table', 'id': 'holdings', 'title': '持仓明细', 'table': {
        'columns': [{'key': key, 'label': label, 'align': align} for key, label, align in [
            ('name', '标的', 'left'), ('price', '现价 / USD', 'right'), ('change', '本日涨跌', 'right'), ('pnl', '盈亏贡献', 'right')]],
        'rows': [{'name': a['name'] + ' / ' + a['symbol'], 'price': f"{a['price']:.2f}" if a['id'] != 'cash' else '—',
                  'change': f"{(a['price'] / a['previous'] - 1) * 100:+.2f}%" if a['id'] != 'cash' else '—',
                  'pnl': f"${a['day']:+,.0f}" if a['id'] != 'cash' else '—'} for a in assets],
        'caption': '虚构价格截至 2026.09.21 收盘，无日内换仓。'}}
    def record(item):
        constraint = item.get('type') == 'constraint'
        exposure = ui['commonWeight'] if constraint else ui['weights'][item['asset']]
        tone = 'warning' if item['initial'] in ('超复核阈值', '支持减弱') else 'neutral'
        return {'id': item['code'], 'category': '组合约束（演示）' if constraint else assets[item['asset']]['name'],
            'title': item['title'], 'summary': item['copy'],
            'status': {'label': item['initial'], 'tone': tone},
            'handling': {'label': item['processing'], 'tone': 'warning' if item['requires_human_decision'] else 'neutral'},
            'facts': [{'label': '关联敞口', 'value': f'{exposure:.1f}%'}, {'label': '资料状态', 'value': item['data']},
                      {'label': '依据', 'value': ('演示约束' if constraint else '预注册') + ' v1'}],
            'sections': [{'label': label, 'body': body} for label, body in [
                ('原始规则', item['rule']), ('裁定 / 处理边界', item['fail']), ('持有或风险依据', item['why']),
                ('价格与口径', item['pricing']), ('建议', item['recommend']),
                ('时间', f"登记 {item['registered']}；核验期限 {item['deadline']}。"),
                ('关联持仓' if constraint else '关联订单', '\n'.join(item['orders']))]],
            'evidence': [{'id': e['id'], 'label': e['label'],
                          'date': datetime.strptime(e['at'], '%Y.%m.%d').date().isoformat(),
                          'body': e['quote'], 'note': e['note'], 'tone': 'warning' if e['id'] == 'E04' else 'neutral'} for e in item['evidence']]}
    records = {'kind': 'records', 'id': 'theses', 'title': '观点与组合事项', 'emptyText': '暂无事项',
        'datasets': [{'id': key, 'label': label, 'items': [record(item) for item in facts['scenarios'][key]['items']]}
                     for key, label in [('r1', 'r1 · 初始状态'), ('r2', 'r2 · 预设反证')]]}
    stamp = int(datetime(2026, 9, 22, 0, 30, tzinfo=timezone.utc).timestamp() * 1000)
    return {'version': 1, 'title': '低频投资组合',
        'description': '全部为虚构数据。两个冻结场景用于展示与阅读评审；无券商连接、交易或调度权限。资料与处理状态分开。',
        'snapshot': {'id': 'portfolio-demo-v1', 'observedAt': stamp, 'producedAt': stamp},
        'rows': [{'id': 'performance', 'title': '01 · 组合表现', 'layout': 'two', 'cells': [metrics, nav]},
                 {'id': 'allocation', 'title': '02 · 资金投向', 'layout': 'three', 'cells': [distribution, weight_chart, table]},
                 {'id': 'research', 'title': '03 · 投资观点', 'layout': 'one', 'cells': [records]}]}

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    facts = json.loads((ROOT / 'demo-facts.json').read_text())
    view = create_view(facts)
    encoded = json.dumps(view, ensure_ascii=False, indent=2, allow_nan=False) + '\n'
    destination = ROOT / 'native-demo.json'
    if args.check:
        assert destination.read_text() == encoded, 'Native Demo fixture drift'
    else:
        destination.write_text(encoded)
    print(json.dumps({'assets': facts['ui']['total'], 'rows': len(view['rows']), 'bytes': len(encoded.encode())}))

if __name__ == '__main__':
    main()
