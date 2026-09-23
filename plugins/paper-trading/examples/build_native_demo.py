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
    ui = facts['portfolio']
    assets, total = ui['assets'], ui['total']
    cash = next(asset for asset in assets if asset['id'] == 'cash')
    palettes = [5, 6, 2, 7]
    def metric(key, label, value, detail, tone='neutral', primary=False):
        return {'id': key, 'label': label, 'value': value, 'detail': detail, 'tone': tone,
                'emphasis': 'primary' if primary else 'normal'}
    metrics = {'kind': 'metrics', 'id': 'assets', 'title': '', 'items': [
        metric('nav', '总资产', scalar(total), f"证券 ${total - cash['value']:,.0f} · 现金 ${cash['value']:,.0f}", primary=True),
        metric('previous', '上一交易日收盘 · 09.18', scalar(ui['previous']), 'USD · 演示交易日'),
        metric('pnl', '本日盈亏 · 09.21', scalar(ui['daily'], signed=True), f"净入金 $0 · 现金盈亏 ${cash['day']:,.2f}", 'positive'),
        metric('change', '本日涨跌', scalar(ui['daily'] / ui['previous'] * 100, '%', 2, True, 'suffix'), '相对上一交易日', 'positive'),
    ]}
    def date(row):
        return datetime.fromisoformat(row['at'].replace('Z', '+00:00')).date().isoformat()
    nav_sets = []
    for period, samples in [('3M', ui['snapshots']), ('1M', ui['snapshots'][8:])]:
        nav_sets.append({'id': 'assets-' + period, 'label': '资产 · ' + period, 'unit': 'USD', 'style': 'line',
            'series': [{'id': 'portfolio', 'label': '组合', 'palette': 5}],
            'points': [{'date': date(row), 'values': [row['nav']]} for row in samples]})
        nav_sets.append({'id': 'returns-' + period, 'label': '收益 · ' + period, 'unit': '%', 'style': 'line',
            'series': [{'id': 'portfolio', 'label': '组合', 'palette': 5}, {'id': 'benchmark', 'label': '模拟基准', 'palette': 6}],
            'points': [{'date': date(row), 'values': [round((row['nav'] / samples[0]['nav'] - 1) * 100, 4),
                       round((row['benchmark'] / samples[0]['benchmark'] - 1) * 100, 4)]} for row in samples]})
    nav = {'kind': 'time-series', 'id': 'nav-history', 'title': '总资产变化', 'caption': '虚构历史估值；收益视图区间起点归零。',
           'emptyText': '暂无估值记录', 'datasets': nav_sets}
    distribution = {'kind': 'distribution', 'id': 'weights', 'title': '当前占比', 'unit': 'USD', 'emptyText': '暂无持仓数据',
        'slices': [{'id': a['id'], 'label': a['name'], 'value': a['value'], 'palette': palettes[i]} for i, a in enumerate(assets)]}
    weight_sets = [{'id': style, 'label': label, 'unit': '%', 'style': style,
        'series': [{'id': a['id'], 'label': a['name'], 'palette': palettes[i]} for i, a in enumerate(assets)],
        'points': [{'date': date(row), 'values': row['weights']} for row in ui['snapshots']]}
        for style, label in [('stacked', '堆叠'), ('line', '折线')]]
    weight_chart = {'kind': 'time-series', 'id': 'weight-history', 'title': '历史仓位', 'caption': '按市值 / 总资产，包含现金。',
                    'emptyText': '暂无历史仓位', 'datasets': weight_sets}
    table = {'kind': 'table', 'id': 'holdings', 'title': '持仓明细', 'table': {
        'columns': [{'key': key, 'label': label, 'align': align} for key, label, align in [
            ('name', '标的', 'left'), ('price', '现价 / USD', 'right'), ('change', '本日涨跌', 'right')]],
        'rows': [{'name': a['name'], 'price': f"{a['price']:.2f}" if a['id'] != 'cash' else '—',
                  'change': f"{(a['price'] / a['previous'] - 1) * 100:+.2f}%" if a['id'] != 'cash' else '—'} for a in assets],
        'caption': '虚构价格截至 2026.09.21 收盘，无日内换仓。'}}
    def record(item, scenario):
        constraint = item.get('type') == 'constraint'
        exposure = ui['commonWeight'] if constraint else ui['weights'][item['asset']]
        tone = 'warning' if item['initial'] in ('超复核阈值', '支持减弱') else 'neutral'
        next_check = '2026.09.24' if scenario == 'r2' and item['id'] != 'river' else item['next']
        facts = [{'label': '关联敞口', 'value': f'{exposure:.1f}%'}, {'label': '下次核验（模拟）', 'value': next_check}]
        if not constraint:
            asset = assets[item['asset']]
            facts.extend([{'label': '标的代码', 'value': asset['symbol']},
                          {'label': '本日盈亏贡献 / USD', 'value': f"{asset['day']:+,.2f}"}])
        facts.extend([{'label': '资料状态', 'value': item['data']}, {'label': '核验期限', 'value': item['deadline']},
                      {'label': '登记时间', 'value': item['registered']}, {'label': '依据', 'value': ('演示约束' if constraint else '预注册') + ' v1'},
                      {'label': '排期说明', 'value': '预设场景中的拟检查日期；没有实际调度、已执行检查或正式延期记录。'}])
        return {'id': item['code'], 'category': '组合约束（演示）' if constraint else assets[item['asset']]['name'],
            'title': item['title'], 'summary': item['copy'],
            'status': {'label': item['initial'], 'tone': tone},
            'handling': {'label': item['processing'], 'tone': 'warning' if item['requires_human_decision'] else 'neutral'},
            'facts': facts,
            'sections': [{'label': label, 'body': body} for label, body in [
                ('原始规则', item['rule']), ('裁定 / 处理边界', item['fail']), ('持有或风险依据', item['why']),
                ('价格与口径', item['pricing']), ('建议', item['recommend']),
                ('关联持仓' if constraint else '关联订单', '\n'.join(item['orders']))]],
            'evidence': [{'id': e['id'], 'label': e['label'],
                          'date': datetime.strptime(e['at'], '%Y.%m.%d').date().isoformat(),
                          'body': e['quote'], 'note': e['note'], 'tone': 'warning' if e['id'] == 'E04' else 'neutral'} for e in item['evidence']]}
    records = {'kind': 'records', 'id': 'theses', 'title': '', 'emptyText': '暂无事项',
        'datasets': [{'id': key, 'label': label,
                      'description': f"研究截止（模拟）：{facts['metadata'][key + '_research_as_of']} · 待人工决定 {sum(item['requires_human_decision'] for item in facts['scenarios'][key]['items'])} 项；估值未随场景改变。",
                      'items': [record(item, key) for item in facts['scenarios'][key]['items']]}
                     for key, label in [('r1', 'r1 · 初始状态'), ('r2', 'r2 · 预设反证')]]}
    stamp = int(datetime(2026, 9, 23, 0, 30, tzinfo=timezone.utc).timestamp() * 1000)
    return {'version': 1, 'title': '低频投资组合',
        'description': '虚构数据 · USD · 估值截至 2026.09.21 收盘；r1 / r2 为预设研究场景，未连接账户。',
        'snapshot': {'id': 'portfolio-demo-v1', 'observedAt': stamp, 'producedAt': stamp},
        'rows': [{'id': 'performance', 'title': '01 · 组合表现', 'layout': 'two-wide-end', 'cells': [metrics, nav]},
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
    recipe = '```neige-block view\n' + json.dumps(view, ensure_ascii=False, allow_nan=False, separators=(',', ':')) + '\n```\n'
    recipe_path = ROOT / 'native-demo.md'
    if args.check:
        assert recipe_path.read_text() == recipe, 'Native Recipe drift'
    else:
        recipe_path.write_text(recipe)
    print(json.dumps({'assets': facts['portfolio']['total'], 'rows': len(view['rows']), 'bytes': len(encoded.encode())}))

if __name__ == '__main__':
    main()
