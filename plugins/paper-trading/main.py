from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from paper_trading.rpc import serve

if __name__ == "__main__":
    serve()
