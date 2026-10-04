"""The shipped portfolio recipe's template views, read from the recipe itself so no test keeps a second
list of the App's published kinds."""
import json
from pathlib import Path
import re

RECIPE = Path(__file__).parents[1] / 'portfolio-recipe.md'
VIEW_FENCE = re.compile(r'^```neige-block view\n(.*?)\n```$', flags=re.M | re.S)


def views():
    return [json.loads(v) for v in VIEW_FENCE.findall(RECIPE.read_text())]


def slots():
    """Every live slot the recipe places, as (source, expected cell kind)."""
    return [(cell['source'], cell['expects'])
            for view in views() for row in view['rows'] for cell in row['cells'] if cell['kind'] == 'live']


def unit_kinds():
    return {source.rsplit('/', 1)[1] for source, _ in slots()}
