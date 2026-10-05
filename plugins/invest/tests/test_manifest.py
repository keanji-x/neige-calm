"""The manifest, the operator configuration and the recipe agree with the App."""
import json
import re

import jsonschema
import pytest

from invest import series
from invest.portfolio import TOOLS
from invest.settings import OPTIONAL, REQUIRED, InvestConfig
from recipe import INSTRUMENT, PORTFOLIO, slots, views
from rig import ROOT

MANIFEST = json.loads((ROOT / 'manifest.json').read_text())
VERBS = {'ls', 'cat', 'show', 'status', 'log', 'diff', 'find', 'describe', 'read', 'write', 'commit', 'tag',
         'rename', 'add', 'set', 'rm', 'capture', 'notify', 'input', 'control', 'open', 'close', 'cancel',
         'publish', 'request', 'accept', 'reject', 'done', 'fail', 'gc', 'vacuum'}  # agent-commands.md §3


def test_manifest_exposes_exactly_the_app_tools_named_object_verb():
    assert MANIFEST['id'] == 'invest' and MANIFEST['manifest_version'] == 5
    names = [tool['name'] for tool in MANIFEST['exposes_tools']]
    assert len(names) == len(TOOLS) + 1 and set(names) == TOOLS | {series.TOOL}
    for name in names:
        noun, verb = name.split('_')
        assert noun.isalnum() and noun.islower() and verb in VERBS, name


def test_manifest_config_schema_matches_the_parsed_keys_and_defaults():
    schema = MANIFEST['config_schema']
    assert set(schema['properties']) == REQUIRED | OPTIONAL
    assert set(schema['required']) == REQUIRED and len(schema['required']) == len(REQUIRED)
    assert schema['additionalProperties'] is False
    # The kernel's config subset: scalar properties only.
    assert {p['type'] for p in schema['properties'].values()} <= {'string', 'integer', 'number', 'boolean'}
    fields = InvestConfig.__dataclass_fields__
    for key, prop in schema['properties'].items():
        if key in OPTIONAL:
            assert 'default' in prop, key
            default = fields[key].default
            parsed = InvestConfig.parse({**_values(), key: prop['default']})
            assert getattr(parsed, key) == default, key


def _values():
    return {'account_no': 'PAPER123', 'broker_home': '/home/paper', 'portfolio_track_id': 'owner',
            'oauth_client_id': 'client', 'sdk_python_path': '/usr/bin/python3',
            'instrument_recipe_id': 'recipe-instrument', 'max_held': 10, 'max_watched': 20, 'max_weight_bps': 3000}


@pytest.mark.parametrize('change,match', [
    ({'max_held': 0}, 'max_held'), ({'max_watched': 0}, 'max_watched'),
    ({'max_held': 200, 'max_watched': 56}, '255'), ({'max_weight_bps': 10001}, 'max_weight_bps'),
    ({'opening_positions': '[{"symbol": "US:SPY", "shares": 0}]'}, 'shares'),
    ({'opening_positions': '[{"symbol": "HK:700", "shares": 1}]'}, 'US'),
    ({'opening_positions': '[{"symbol": "US:SPY", "shares": 1}, {"symbol": "us:spy", "shares": 2}]'}, 'twice'),
    ({'opening_positions': '{"US:SPY": 1}'}, 'opening_positions'),
    ({'opening_positions': 'not json'}, 'opening_positions'),
    ({'max_held': 1, 'opening_positions': '[{"symbol": "US:A", "shares": 1}, {"symbol": "US:B", "shares": 1}]'},
     'max_held'),
    ({'unexpected': 1}, 'missing or unknown'),
    ({'lease_days': 0}, 'lease_days'), ({'lease_days': 91}, 'lease_days'),
    ({'instrument_recipe_id': ' '}, 'instrument_recipe_id'),
])
def test_config_refuses_invalid_values(change, match):
    with pytest.raises(ValueError, match=match):
        InvestConfig.parse(_values() | change)


def test_config_canonicalizes_opening_positions():
    config = InvestConfig.parse(_values() | {'opening_positions': '[{"symbol": "us:spy", "shares": 13}]'})
    assert config.opening_positions == (('US:SPY', 13),)
    assert InvestConfig.parse(_values()).opening_positions == ()


@pytest.mark.parametrize('recipe,kinds', [
    (PORTFOLIO, ('portfolio.', 'thesis.board')), (INSTRUMENT, ('instrument.position', 'thesis.records'))])
def test_recipe_views_are_native_views_over_invest_units(recipe, kinds):
    schema = json.loads((ROOT.parents[1] / 'crates/calm-types/src/report_blocks/native_view.schema.json').read_text())
    view_schema = {'$schema': schema['$schema'], '$defs': schema['$defs'], '$ref': '#/$defs/NativeView'}
    for view in views(recipe):
        jsonschema.Draft202012Validator(view_schema).validate(view)
    placed = slots(recipe)
    assert placed and all(source.startswith(tuple(f'neige://plugin/invest/{k}' for k in kinds))
                          for source, _ in placed)
    assert len({source for source, _ in placed}) == len(placed)


def test_planner_instructions_fit_the_kernel_cap():
    text = MANIFEST['planner_instructions']
    # `plugin_host/manifest.rs` MAX_PLANNER_INSTRUCTIONS_BYTES, legal only at manifest_version 5.
    assert text.strip() and len(text.encode()) <= 2048


def agent_texts():
    """Every text an agent reads that names invest tools: the standing instructions, the tool
    descriptions and both recipes."""
    yield 'planner_instructions', MANIFEST['planner_instructions']
    for tool in MANIFEST['exposes_tools']:
        yield tool['name'], tool['description']
    for recipe in (PORTFOLIO, INSTRUMENT):
        yield recipe, (ROOT / recipe).read_text()


def test_agent_texts_name_tools_by_their_served_names():
    names = '|'.join(tool['name'] for tool in MANIFEST['exposes_tools'])
    # A bare `instrument_status` is an unknown tool to the agent; only `plugin_invest_<tool>` is served.
    # (`neige://plugin/invest/<tool>` is a report source, not a call.)
    bare = re.compile(rf'(?<![A-Za-z0-9_/]){"(?:" + names + ")"}(?![A-Za-z0-9_])')
    for where, text in agent_texts():
        assert not bare.findall(re.sub(r'plugin_invest_\w+', '', text)), where


def test_recipes_link_each_others_reports():
    portfolio, research = ((ROOT / recipe).read_text() for recipe in (PORTFOLIO, INSTRUMENT))
    assert '研究链接' in portfolio and '- [<symbol> 研究](neige://wave/<track_id>)' in portfolio
    assert '[组合 Track](neige://wave/<portfolio_track_id>)' in research
