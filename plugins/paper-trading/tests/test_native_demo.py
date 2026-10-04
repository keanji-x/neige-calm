"""The committed example is the production SPY overview of a scripted simulated account."""
import json
from pathlib import Path
import runpy

import jsonschema

from paper_trading.allocation_report import tables
from paper_trading.report_views import native_view

EXAMPLES = Path(__file__).parents[1] / 'examples'
SCHEMA = json.loads((Path(__file__).parents[3] / 'crates/calm-types/src/report_blocks/native_view.schema.json').read_text())
BUILDER = runpy.run_path(str(EXAMPLES / 'build_native_demo.py'))


def committed():
    return json.loads((EXAMPLES / 'native-demo.json').read_text())


def test_example_is_the_production_overview_of_the_scripted_run(tmp_path):
    state = BUILDER['simulate'](tmp_path)
    production = tables(state)['spy.overview']
    example = committed()
    # Everything except the top-level description is exactly the production projection.
    assert {k: v for k, v in example.items() if k not in ('description', 'snapshot')} == \
        {k: v for k, v in production.items() if k not in ('description', 'snapshot')}
    assert example['snapshot']['observedAt'] == production['snapshot']['observedAt'] is not None
    assert example['snapshot']['producedAt'] is None
    # The snapshot identity stays content-derived over the replaced description.
    assert example == native_view(state, production['title'], production['rows'], example['description'])
    jsonschema.Draft202012Validator(SCHEMA).validate(example)
    json.dumps(example, allow_nan=False)
    recipe = (EXAMPLES / 'native-demo.md').read_text()
    assert recipe == '```neige-block view\n' + json.dumps(example, ensure_ascii=False, separators=(',', ':')) + '\n```\n'


def test_example_never_claims_a_real_account():
    example = committed()
    assert example['description'].startswith('示例数据 · 脚本化模拟账户，非真实账户')
    assert '长桥' not in json.dumps(example, ensure_ascii=False)


def test_example_records_the_scripted_decision_outcomes():
    records = next(c for row in committed()['rows'] for c in row['cells'] if c['kind'] == 'records')
    [dataset] = records['datasets']
    outcomes = {item['id']: (item['badges'][0]['value'], len(item['disclosures'])) for item in dataset['items']}
    assert outcomes == {'spy-20260916': ('已成交', 1), 'spy-20260819': ('已过期', 0), 'spy-20260805': ('已成交', 2),
                        'spy-20260722': ('无需调仓', 0), 'spy-20260701': ('已成交', 1)}
    assert all('actions' not in item for item in dataset['items'])
