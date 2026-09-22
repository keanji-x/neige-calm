import json
from pathlib import Path
import runpy

EXAMPLES = Path(__file__).parents[1] / 'examples'

def test_native_demo_uses_one_structured_payload_and_no_executable_components():
    build = runpy.run_path(str(EXAMPLES / 'build_native_demo.py'))['create_view']
    facts = json.loads((EXAMPLES / 'demo-facts.json').read_text())
    view = build(facts)
    assert view == json.loads((EXAMPLES / 'native-demo.json').read_text())
    assert [row['layout'] for row in view['rows']] == ['two', 'three', 'one']
    kinds = [cell['kind'] for row in view['rows'] for cell in row['cells']]
    assert kinds == ['metrics', 'time-series', 'distribution', 'time-series', 'table', 'records']
    metrics = view['rows'][0]['cells'][0]['items']
    assert metrics[0]['value']['amount'] == 1084620
    assert metrics[2]['value']['amount'] == 6040
    groups = view['rows'][2]['cells'][0]['datasets']
    assert sum(item['handling']['label'] == '待人工决定' for item in groups[0]['items']) == 2
    assert sum(item['handling']['label'] == '待人工决定' for item in groups[1]['items']) == 3
    assert all('actions' not in item for group in groups for item in group['items'])

def test_native_demo_retains_research_cutoff_for_each_scenario():
    build = runpy.run_path(str(EXAMPLES / 'build_native_demo.py'))['create_view']
    facts = json.loads((EXAMPLES / 'demo-facts.json').read_text())
    view = build(facts)
    for group in view['rows'][2]['cells'][0]['datasets']:
        for item in group['items']:
            assert {'label': '研究截止（模拟）', 'value': facts['metadata'][group['id'] + '_research_as_of']} in item['facts']
