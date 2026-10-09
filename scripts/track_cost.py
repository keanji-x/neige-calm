"""Per-attempt wall clock and worker tokens for track-scorecard.py (#2508).

One row per task attempt (tasks.id) created in the scorecard window:
  queue_ms  created_at_ms to running_started_at_ms (null before migration 0159 or if never run)
  run_ms    running_started_at_ms to finished_at_ms; it includes the kernel gate after the worker
            reports, and for a regated attempt (whose finished_at_ms the regate resets) also the
            time it sat failed before the regate (null while unfinished or never run)
  gate_ms   summed wall clock of the attempt's gate operations: kernel gates (task-verify) and
            worker runs (task-gate-run), linked by payload task_id; 0 when there were none. Null
            while one is unfinished (gate_open).
  tokens    {input, cached, output, total} from the worker card's transcript file, the path the
            kernel's capture cursor (worker_flow_cursors.source_path) holds. Null with
            tokens_error when the card, its cursor or its file is missing, unreadable or has no
            usage. Codex: the last token_count event's info.total_token_usage (cumulative per
            thread). Claude: message.usage of assistant lines, once per message.id; input counts
            uncached, cache-write and cache-read input, as Codex's input_tokens includes cached
            input; the repeated lines of one message carry the same usage. Only the file the
            cursor names now is read: a card whose session moved to another transcript (a
            restart) or that spawned subagent transcripts is undercounted, not marked missing.
The total row sums the known values; `missing` counts the attempts each sum lacks. Transcript
files are opened read-only.
"""

import json

# The columns this module reads, beyond track_db.REQUIRED_COLUMNS; the scorecard checks both.
COST_COLUMNS = {
    "tasks": ["id", "track_id", "key", "kind", "status", "created_at_ms", "running_started_at_ms",
              "finished_at_ms", "worker_card_id"],
    "operations": ["kind", "payload_json", "created_at_ms", "completed_at_ms"],
    "worker_flow_cursors": ["card_id", "source_kind", "source_path"],
}
GATE_OP_KINDS = ("task-verify", "task-gate-run")
TOKEN_FIELDS = ("input", "cached", "output", "total")


def records(lines, marker):
    """The JSON records of the lines containing `marker`. A last line without its newline is a
    write still in progress, so it is skipped when it does not parse; any other bad line raises."""
    for line in lines:
        if marker not in line:
            continue
        try:
            yield json.loads(line)
        except ValueError:
            if line.endswith("\n"):
                raise


def codex_usage(lines):
    """A Codex rollout's cumulative usage: its last token_count event with info, or None."""
    usage = None
    for record in records(lines, '"token_count"'):
        payload = record.get("payload") or {}
        if record.get("type") == "event_msg" and payload.get("type") == "token_count" and payload.get("info"):
            total = payload["info"]["total_token_usage"]
            usage = {"input": total["input_tokens"], "cached": total["cached_input_tokens"],
                     "output": total["output_tokens"], "total": total["total_tokens"]}
    return usage


def claude_usage(lines):
    """A Claude transcript's usage, each assistant message.id once (its content blocks are separate
    lines that repeat the message's usage), or None."""
    by_message = {}
    for record in records(lines, '"usage"'):
        message = record.get("message")
        if record.get("type") == "assistant" and isinstance(message, dict) and "usage" in message:
            by_message[message["id"]] = message["usage"]
    if not by_message:
        return None
    usage = dict.fromkeys(TOKEN_FIELDS, 0)
    for u in by_message.values():
        cached = u["cache_read_input_tokens"]
        input_tokens = u["input_tokens"] + u["cache_creation_input_tokens"] + cached
        usage["input"] += input_tokens
        usage["cached"] += cached
        usage["output"] += u["output_tokens"]
        usage["total"] += input_tokens + u["output_tokens"]
    return usage


# worker_flow_cursors.source_kind (the kernel's capture source names) -> (provider, parser).
SOURCES = {"codex_rollout": ("codex", codex_usage), "claude_transcript": ("claude", claude_usage)}


def file_tokens(source_kind, path):
    """(usage, None) or (None, reason) for one transcript file."""
    if source_kind not in SOURCES:
        return None, f"unknown transcript source {source_kind}"
    try:
        with open(path, encoding="utf-8") as lines:
            usage = SOURCES[source_kind][1](lines)
    except FileNotFoundError:
        return None, f"file missing: {path}"
    except (OSError, UnicodeDecodeError) as e:
        return None, f"unreadable: {path}: {e}"
    except (ValueError, KeyError, TypeError) as e:
        return None, f"unparsable: {path}: {e!r}"
    if usage is None:
        return None, f"no usage records: {path}"
    return usage, None


def gate_ops(db, track_id):
    """task_id -> [(created_at_ms, completed_at_ms)] of the Track's gate operations."""
    ops = {}
    marks = ",".join("?" * len(GATE_OP_KINDS))
    for task_id, created, completed in db.execute(
        f"SELECT json_extract(payload_json, '$.task_id'), created_at_ms, completed_at_ms FROM operations"
        f" WHERE kind IN ({marks}) AND json_extract(payload_json, '$.track_id') = ?",
        (*GATE_OP_KINDS, track_id),
    ):
        ops.setdefault(task_id, []).append((created, completed))
    return ops


def span(start, stop):
    return None if start is None or stop is None else stop - start


def attempts(db, track_id, start, end):
    """(rows, total) for the task attempts created in [start, end]; see the module docstring."""
    ops = gate_ops(db, track_id)
    rows = []
    for task_id, key, kind, status, created, running, finished, card in db.execute(
        "SELECT id, key, kind, status, created_at_ms, running_started_at_ms, finished_at_ms, worker_card_id"
        " FROM tasks WHERE track_id = ? AND created_at_ms BETWEEN ? AND ? ORDER BY created_at_ms, id",
        (track_id, start, end),
    ):
        mine = ops.get(task_id, [])
        gate_open = sum(1 for _, completed in mine if completed is None)
        row = {
            "attempt": task_id, "key": key, "kind": kind, "status": status, "provider": None,
            "queue_ms": span(created, running), "run_ms": span(running, finished),
            "gate_ms": None if gate_open else sum(c - s for s, c in mine),
            "gate_ops": len(mine), "gate_open": gate_open,
            "tokens": None, "tokens_error": None, "transcript": None,
        }
        cursors = [] if card is None else db.execute(
            "SELECT source_kind, source_path FROM worker_flow_cursors WHERE card_id = ?", (card,)
        ).fetchall()
        if card is None:
            row["tokens_error"] = "no worker card"
        elif len(cursors) != 1:
            row["tokens_error"] = f"worker card {card} has {len(cursors)} transcript cursors"
        else:
            source_kind, path = cursors[0]
            row["provider"] = SOURCES[source_kind][0] if source_kind in SOURCES else None
            row["transcript"] = path
            row["tokens"], row["tokens_error"] = file_tokens(source_kind, path)
        rows.append(row)
    return rows, total(rows)


def total(rows):
    def known_sum(field):
        values = [r[field] for r in rows if r[field] is not None]
        return sum(values) if values else None

    tokens = [r["tokens"] for r in rows if r["tokens"] is not None]
    return {
        "attempts": len(rows),
        "queue_ms": known_sum("queue_ms"),
        "run_ms": known_sum("run_ms"),
        "gate_ms": known_sum("gate_ms"),
        "gate_ops": sum(r["gate_ops"] for r in rows),
        "tokens": {f: sum(u[f] for u in tokens) for f in TOKEN_FIELDS} if tokens else None,
        "missing": {f: sum(1 for r in rows if r[f] is None) for f in ("queue_ms", "run_ms", "gate_ms", "tokens")},
    }


def fmt_ms(ms):
    if ms is None:
        return "-"
    s = round(ms / 1000)
    return f"{s // 3600}h{s % 3600 // 60:02d}m" if s >= 3600 else f"{s // 60}m{s % 60:02d}s" if s >= 60 else f"{s}s"


def fmt_count(n):
    if n is None:
        return "-"
    return f"{n / 1e6:.1f}M" if n >= 1e6 else f"{n / 1e3:.1f}k" if n >= 1e3 else str(n)


def table_rows(rows, tot):
    """The --detail attempt table, header first; a total cell gets "(+N?)" when N attempts lack it."""
    def tokens(t, field):
        return fmt_count(None if t is None else t[field])

    def missing(cell, field):
        n = tot["missing"][field]
        return cell + (f" (+{n}?)" if n else "")

    out = [["key", "kind", "status", "provider", "queue", "run", "gate", "gate_ops", "in", "cached", "out", "total"]]
    for r in rows:
        out.append([r["key"], r["kind"], r["status"], r["provider"] or "-", fmt_ms(r["queue_ms"]), fmt_ms(r["run_ms"]),
                    fmt_ms(r["gate_ms"]), str(r["gate_ops"]) + (f" ({r['gate_open']} open)" if r["gate_open"] else "")]
                   + [tokens(r["tokens"], f) for f in TOKEN_FIELDS])
    out.append([f"total ({tot['attempts']})", "", "", "", missing(fmt_ms(tot["queue_ms"]), "queue_ms"),
                missing(fmt_ms(tot["run_ms"]), "run_ms"), missing(fmt_ms(tot["gate_ms"]), "gate_ms"), str(tot["gate_ops"])]
               + [tokens(tot["tokens"], f) for f in TOKEN_FIELDS[:-1]] + [missing(tokens(tot["tokens"], "total"), "tokens")])
    return out


CODEX_LINES = [
    '{"type":"session_meta","payload":{"id":"s"}}\n',
    '{"type":"event_msg","payload":{"type":"token_count","info":null}}\n',
    '{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,'
    '"cached_input_tokens":80,"cache_write_input_tokens":0,"output_tokens":7,"reasoning_output_tokens":3,'
    '"total_tokens":107},"last_token_usage":{"input_tokens":60}}}}\n',
    '{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":300,'
    '"cached_input_tokens":250,"output_tokens":20,"total_tokens":320}}}}\n',
    '{"type":"event_msg","payload":{"type":"token_count","info":null}}\n',
    '{"type":"event_msg","payload":{"type":"token_count","info":{"total_tok',  # a write in progress
]
CLAUDE_LINES = [
    '{"type":"user","message":{"role":"user","content":"go"}}\n',
    '{"type":"assistant","message":{"id":"m1","content":[{"type":"thinking"}],"usage":{"input_tokens":2,'
    '"cache_creation_input_tokens":100,"cache_read_input_tokens":1000,"output_tokens":50}}}\n',
    '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use"}],"usage":{"input_tokens":2,'
    '"cache_creation_input_tokens":100,"cache_read_input_tokens":1000,"output_tokens":50}}}\n',
    '{"type":"assistant","message":{"id":"m2","content":[{"type":"text"}],"usage":{"input_tokens":3,'
    '"cache_creation_input_tokens":10,"cache_read_input_tokens":1100,"output_tokens":5}}}\n',
]


def selftest_failures():
    """Check the per-provider token parsers on inline sample lines; return the failures."""
    failures = []
    cases = [
        ("codex rollout", codex_usage(CODEX_LINES), {"input": 300, "cached": 250, "output": 20, "total": 320}),
        ("codex without usage", codex_usage(CODEX_LINES[:2]), None),
        ("claude transcript (message.id once)", claude_usage(CLAUDE_LINES),
         {"input": 1102 + 1113, "cached": 2100, "output": 55, "total": 1102 + 1113 + 55}),
        ("claude without usage", claude_usage(CLAUDE_LINES[:1]), None),
    ]
    for name, got, want in cases:
        if got != want:
            failures.append(f"{name}: expected {want}, got {got}")
    try:
        codex_usage(['{"type":"event_msg","payload":{"type":"token_count" broken\n'])
        failures.append("a complete bad line must raise")
    except ValueError:
        pass
    return failures
