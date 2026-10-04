"""The shipped SPY recipe's template views, read from the recipe itself so no test keeps a second
list of the App's published kinds."""
import json
from pathlib import Path
import re

RECIPE = Path(__file__).parents[1] / 'spy-recipe.md'
VIEW_FENCE = re.compile(r'^```neige-block view\n(.*?)\n```$', flags=re.M | re.S)


def views(text=None):
    """Every `view` fence of `text` (default: the shipped recipe), decoded."""
    return [json.loads(v) for v in VIEW_FENCE.findall(RECIPE.read_text() if text is None else text)]


def unit_kinds():
    """The overlay kind of every live slot the recipe places."""
    return {cell['source'].rsplit('/', 1)[1]
            for view in views() for row in view['rows'] for cell in row['cells'] if cell['kind'] == 'live'}
