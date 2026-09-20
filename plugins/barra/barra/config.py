from dataclasses import asdict, dataclass
import re

DEFAULT_SYMBOLS = (
    "AAPL", "MSFT", "AMZN", "GOOGL", "META", "NVDA", "JPM", "BAC", "GS", "V",
    "XOM", "CVX", "UNH", "JNJ", "MRK", "PG", "KO", "WMT", "COST", "HD",
    "CAT", "GE", "HON", "UPS", "NEE", "DUK", "AMT", "PLD", "DIS", "NFLX",
)
SYMBOL = re.compile(r"^[A-Z][A-Z0-9-]{0,9}$")


@dataclass(frozen=True)
class Config:
    symbols: tuple[str, ...] = DEFAULT_SYMBOLS
    benchmark: str = "SPY"
    update_hour_utc: int = 7
    history_days: int = 126

    @classmethod
    def parse(cls, args):
        if not isinstance(args, dict) or set(args) - set(cls.__dataclass_fields__):
            raise ValueError("unknown configuration fields")
        values = asdict(cls()) | args
        symbols = values["symbols"]
        if not isinstance(symbols, (list, tuple)) or not 12 <= len(symbols) <= 40:
            raise ValueError("symbols must contain 12 to 40 distinct US tickers")
        if any(not isinstance(s, str) or not SYMBOL.fullmatch(s) for s in symbols):
            raise ValueError("symbols must be uppercase Yahoo US ticker symbols")
        if len(set(symbols)) != len(symbols):
            raise ValueError("duplicate symbols")
        benchmark = values["benchmark"]
        if not isinstance(benchmark, str) or not SYMBOL.fullmatch(benchmark):
            raise ValueError("invalid benchmark")
        if benchmark in symbols:
            raise ValueError("benchmark must not also be a research stock")
        for key, low, high in [("update_hour_utc", 0, 23), ("history_days", 60, 252)]:
            if type(values[key]) is not int or not low <= values[key] <= high:
                raise ValueError(f"{key} must be an integer in [{low}, {high}]")
        return cls(tuple(symbols), benchmark, values["update_hour_utc"], values["history_days"])

    def json(self):
        return asdict(self)
