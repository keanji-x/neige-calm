"""Import paths and the shared portfolio fixture."""
from pathlib import Path
import sys

import pytest

ROOT = Path(__file__).parents[1]
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(Path(__file__).parent))

from rig import Rig  # noqa: E402


@pytest.fixture
def rig(tmp_path):
    return Rig(tmp_path)
