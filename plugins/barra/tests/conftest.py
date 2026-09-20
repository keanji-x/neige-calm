import sys
from pathlib import Path

import numpy as np
import pandas as pd
import pytest

sys.path.insert(0, str(Path(__file__).parents[1]))
from barra.config import Config


@pytest.fixture
def config():
    return Config.parse({"symbols": [f"S{n}" for n in range(16)], "history_days": 60})


@pytest.fixture
def prices(config):
    rng = np.random.default_rng(1746)
    n = 450
    market = rng.normal(0.0003, 0.009, n)
    stocks = market[:, None] * np.linspace(0.5, 1.6, len(config.symbols))
    stocks += rng.normal(size=stocks.shape) * np.linspace(0.004, 0.019, len(config.symbols))
    stocks += np.linspace(-0.0003, 0.0006, len(config.symbols))
    returns = np.column_stack([stocks, market])
    return pd.DataFrame(100 * np.cumprod(1 + returns, axis=0),
                        index=pd.bdate_range(end="2026-09-18", periods=n),
                        columns=[*config.symbols, config.benchmark])
