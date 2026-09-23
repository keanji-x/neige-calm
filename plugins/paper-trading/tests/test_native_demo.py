import json
from pathlib import Path
import runpy
from decimal import Decimal

EXAMPLES = Path(__file__).parents[1] / 'examples'

def test_native_demo_uses_one_structured_payload_and_no_executable_components():
    build = runpy.run_path(str(EXAMPLES / 'build_native_demo.py'))['create_view']
    facts = json.loads((EXAMPLES / 'demo-facts.json').read_text())
    view = build(facts)
    assert view == json.loads((EXAMPLES / 'native-demo.json').read_text())
    assert [row['layout'] for row in view['rows']] == ['two-wide-end', 'three', 'one']
    kinds = [cell['kind'] for row in view['rows'] for cell in row['cells']]
    assert kinds == ['metrics', 'time-series', 'distribution', 'time-series', 'table', 'records']
    metrics = view['rows'][0]['cells'][0]['items']
    assert metrics[0]['value']['amount'] == 1084620
    assert metrics[2]['value']['amount'] == 6040
    groups = view['rows'][2]['cells'][0]['datasets']
    assert sum(item['handling']['label'] == '待人工决定' for item in groups[0]['items']) == 2
    assert sum(item['handling']['label'] == '待人工决定' for item in groups[1]['items']) == 3
    assert all('actions' not in item for group in groups for item in group['items'])
    assert (EXAMPLES / 'native-demo.md').read_text().startswith('```neige-block view\n')

def test_native_demo_retains_research_cutoff_for_each_scenario():
    build = runpy.run_path(str(EXAMPLES / 'build_native_demo.py'))['create_view']
    facts = json.loads((EXAMPLES / 'demo-facts.json').read_text())
    view = build(facts)
    for group in view['rows'][2]['cells'][0]['datasets']:
        assert facts['metadata'][group['id'] + '_research_as_of'] in group['description']

def test_compact_template_retains_complete_analytical_facts_for_planner():
    build = runpy.run_path(str(EXAMPLES / 'build_native_demo.py'))['create_view']
    facts = json.loads((EXAMPLES / 'demo-facts.json').read_text())
    view = build(facts)
    table = view['rows'][1]['cells'][2]['table']
    assert [column['key'] for column in table['columns']] == ['name', 'price', 'change']
    assert len(table['rows']) == len(facts['portfolio']['assets'])
    for group in view['rows'][2]['cells'][0]['datasets']:
        pnl = Decimal(0)
        for record, source in zip(group['items'], facts['scenarios'][group['id']]['items'], strict=True):
            fields = {field['label']: field['value'] for field in record['facts']}
            assert [field['label'] for field in record['facts'][:2]] == ['关联敞口', '下次核验（模拟）']
            assert fields['核验期限'] == source['deadline']
            assert fields['登记时间'] == source['registered']
            assert fields['资料状态'] == source['data']
            assert len(record['evidence']) == len(source['evidence'])
            if source.get('type') != 'constraint':
                asset = facts['portfolio']['assets'][source['asset']]
                assert fields['标的代码'] == asset['symbol']
                amount = Decimal(fields['本日盈亏贡献 / USD'].replace(',', ''))
                assert amount == Decimal(str(asset['day']))
                pnl += amount
        cash = next(asset for asset in facts['portfolio']['assets'] if asset['id'] == 'cash')
        assert pnl + Decimal(str(cash['day'])) == Decimal(str(facts['portfolio']['daily']))
