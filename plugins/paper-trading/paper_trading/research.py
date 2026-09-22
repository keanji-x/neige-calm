"""Read only the named weekly report and freeze its provenance."""
from datetime import date
import json
from pathlib import Path

from .config import calendar_date, exact
from .ledger import digest, encoded


def read_bounded(root, relative):
    root = Path(root).resolve(strict=True)
    path = (root / relative).resolve(strict=True)
    if not path.is_relative_to(root) or not path.is_file():
        raise ValueError("research path escapes configured root")
    with path.open("rb") as handle:
        data = handle.read(1_000_001)
    if len(data) > 1_000_000:
        raise ValueError("research file exceeds 1 MB")
    return data.decode("utf-8")


def ingest(db, ledger, config, args):
    exact(args, {"week"})
    week = date.fromisoformat(args["week"])
    if week.isoformat() != args["week"]:
        raise ValueError("week must be YYYY-MM-DD")
    folder = f"weekly/{week.year}/{week:%m_%d}"
    report = read_bounded(config.research_root, f"{folder}/weekly_market_analysis_cn.md")
    rows = [json.loads(line) for line in read_bounded(config.research_root,
            f"weekly/{week.year}/predictions.jsonl").splitlines() if line.strip()]
    predictions = [row for row in rows if isinstance(row, dict) and row.get("issued") == args["week"]]
    if not predictions or len({p["id"] for p in predictions}) != len(predictions):
        raise ValueError("week must have uniquely identified predictions")
    for prediction in predictions:
        if calendar_date(prediction["horizon_end"]) < week:
            raise ValueError("research horizon precedes its issued date")
    body = {"week": args["week"], "report": report, "predictions": predictions}
    key = digest(body)
    if db.execute("SELECT 1 FROM sources WHERE id=?", (key,)).fetchone() is None:
        db.execute("INSERT INTO sources VALUES (?,?)", (key, encoded(body)))
        ledger.event(db, "source_ingested", {"source_id": key, "week": args["week"]})
    return {"source_id": key, **body}


def source(db, key):
    row = db.execute("SELECT body FROM sources WHERE id=?", (key,)).fetchone()
    if row is None:
        raise ValueError("unknown source_id; ingest the requested week first")
    return json.loads(row[0])
