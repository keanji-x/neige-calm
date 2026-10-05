"""The shipped recipes' template views, read from the recipes themselves so no test keeps a second
list of the App's published kinds."""
import json
from pathlib import Path
import re

PLUGIN = Path(__file__).parents[1]
PORTFOLIO, INSTRUMENT = 'portfolio-recipe.md', 'instrument-recipe.md'
VIEW_FENCE = re.compile(r'^```neige-block view\n(.*?)\n```$', flags=re.M | re.S)


def views(recipe=PORTFOLIO):
    return [json.loads(v) for v in VIEW_FENCE.findall((PLUGIN / recipe).read_text())]


def slots(recipe=PORTFOLIO):
    """Every live slot the recipe places, as (source, expected cell kind)."""
    return [(cell['source'], cell['expects'])
            for view in views(recipe) for row in view['rows'] for cell in row['cells'] if cell['kind'] == 'live']


def unit_kinds(recipe=PORTFOLIO):
    return {source.rsplit('/', 1)[1] for source, _ in slots(recipe)}
