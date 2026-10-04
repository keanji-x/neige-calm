"""The committed example is the SPY Report template plus the production units of a scripted simulated account."""
import json
from pathlib import Path
import re
import runpy

import jsonschema

from paper_trading.allocation_views import units

EXAMPLES = Path(__file__).parents[1] / 'examples'
SCHEMA = json.loads((Path(__file__).parents[3] / 'crates/calm-types/src/report_blocks/native_view.schema.json').read_text())
UNIT_SCHEMA = {'$schema': SCHEMA['$schema'], '$defs': SCHEMA['$defs'], '$ref': '#/$defs/DataUnit'}
BUILDER = runpy.run_path(str(EXAMPLES / 'build_native_demo.py'))
MARKER = '示例数据 · 脚本化模拟账户，非真实账户'


def committed():
    return json.loads((EXAMPLES / 'native-demo.json').read_text())


def recipe_views():
    text = (EXAMPLES.parent / 'spy-recipe.md').read_text()
    return [json.loads(v) for v in re.findall(r'^```neige-block view\n(.*?)\n```$', text, flags=re.M | re.S)]


def test_example_is_the_production_output_of_the_scripted_run(tmp_path):
    state = BUILDER['simulate'](tmp_path)
    example = committed()
    # Every overlay is exactly the production unit, snapshot identity included.
    assert example['overlays'] == units(state)
    assert all(u['snapshot']['observedAt'] is not None and u['snapshot']['producedAt'] is None
               for u in example['overlays'].values())
    for unit in example['overlays'].values():
        jsonschema.Draft202012Validator(UNIT_SCHEMA).validate(unit)
    # The views are the recipe's template views; only their descriptions are replaced.
    assert [view | {'description': ''} for view in example['views']] == recipe_views()
    for view in example['views']:
        jsonschema.Draft202012Validator(SCHEMA).validate(view)
    json.dumps(example, allow_nan=False)


def test_example_never_claims_a_real_account():
    example = committed()
    assert all(view['description'].startswith(MARKER) for view in example['views'])
    assert '长桥' not in json.dumps(example, ensure_ascii=False)


def test_example_records_the_scripted_decision_outcomes():
    records = committed()['overlays']['spy.decision_log']['cell']
    [dataset] = records['datasets']
    outcomes = {item['id']: (item['badges'][0]['value'], len(item['disclosures'])) for item in dataset['items']}
    assert outcomes == {'spy-20260916': ('已成交', 1), 'spy-20260819': ('已过期', 0), 'spy-20260805': ('已成交', 2),
                        'spy-20260722': ('无需调仓', 0), 'spy-20260701': ('已成交', 1)}
    assert all('actions' not in item for item in dataset['items'])
