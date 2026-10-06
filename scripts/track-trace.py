#!/usr/bin/env python3
"""Print every Planner turn of a set of Tracks as JSON Lines, read from a calm database (#2206 T2).

One record per line. First one `track` record per Track, then every `turn` record of all the
Tracks in start order. Every Track kind has the same shape; nothing here knows a domain.

The trace has only the turns still in the Planner's transcript. A Planner reset deletes the whole
transcript (event harness.transcript.cleared) and a rewind deletes the latest turns
(harness.transcript.rewound); those turns are not printed and not reconstructed, and the events
they caused fall into events_before_first_turn or the window of the turn before them. Every such
loss is listed in the track record's transcript_losses, so the reader knows where the gaps are.

{"type": "track", "track", "title", "area_id", "created_at", "closed_at", "creator_track_id",
 "planner_card" (null: the Track has no Planner, so it has no turns), "excerpt_chars" (the cap
 on every excerpt below except a task's error, null under --full),
 "transcript_losses": [{"id", "at", "kind", "payload"}]  every reset or rewind of this Track's
     transcript (payload: the stored payload as an object, e.g. cleared_item_count, or
     removed_item_count and turn_id),
 "events_before_first_turn": [event...]  this Track's events before its first remaining turn
     (all of them when no turn remains)}

{"type": "turn", "track", "turn_id", "started_at" (its first transcript item), "completed_at" and
 "duration_ms" (null while the turn has no turn/completed row: running, or never closed),
 "status" and "error" (from turn/completed, else null),
 "trigger": [segment...]  every segment of the turn's userMessage rows, in order; a steer adds a
     row mid-turn, so its segments have a later `at`. Null when no row recorded segments; when
     only some did, each other row stands as one segment whose fields other than `at` are null.
     segment = {"at", "presentation", "text", "attachment_count", "origin"}; origin = {"observation" (the Observation
     type tag, e.g. task_completed, track_wake, user_message), "event_id" (events.id or null),
     "event_kind" (that event's kind, or null), "source" (track_wake only: the wake event's
     source, e.g. dev.neige.mail)}. Rows written before #2206 T1 have "origin": null: not
     recorded, not guessed from the text.
 "messages": [{"at", "text"}]  the Planner's own text (its completed agentMessage items: what it
     tells or asks the user, why it chose a step), in order; reasoning items are left out,
 "actions": [action...]  every Planner transcript item except its input, messages and reasoning
     (userMessage, agentMessage, reasoning), in order. action = {"at", "item_type", "tool"
     (null for a shell command or an item without a tool), "finished" (false: the item started
     but never completed), "ok" (false when its status is failed, null when unfinished),
     "duration_ms", "error", "args", "result"}. args/result: a shell command and its output
     (strings), a tool call's arguments and result (objects, or the result's text); any other
     item type passes its whole item as args.
 "tasks": [{"at", "key", "kind", "status", "error", "goal"}]  task attempts created in the turn's
     window. error (null unless status is failed): the reason of the task's latest task.failed
     event (a worker's report, a kernel timeout or spawn error, or the Planner's rejection), whole
     up to TASK_ERROR_CHARS; without one, the task row's status_detail (a gate failure records
     only the classifier gate-red there; its failing_step, exit_code and log_tail are in the
     task.gate_result event). status_detail itself keeps only the first ~480 chars of a reason.
 "events": [{"id", "at", "kind", "actor" (actor kind), "payload" (the stored payload as an
     object)}]  this Track's events in the turn's window, minus NOISE_EVENT_KINDS,
 "bypass": [{"source", "command"}]  track-scorecard's bypass hits in the turn's window}

A turn's window runs from its start to the next remaining turn's start (no end for the last
turn): what the turn caused and what arrived before the next one. Unless --full, every text is cut
to EXCERPT_CHARS with a "…[+N chars]" tail: segment texts, messages, goals, errors (a structured
error as JSON text), and each string leaf of an event payload or of an action's args and result.
The cap is per leaf, so short fields (failing_step, exit_code, head_sha, a short error or reason)
always survive whole and only long bodies and logs are cut. A task's error is cut at
TASK_ERROR_CHARS instead. Per-turn token usage is not recorded yet.

Usage: scripts/track-trace.py --db PATH [--since MS] [--until MS] [--full] (--area AREA_ID | TRACK_ID...)
       scripts/track-trace.py --selftest   (builds a tiny database and checks the records; CI runs it)
--since/--until only select which turns are printed, by start time (epoch ms, inclusive); no
record's content depends on them. The database is opened read-only.
"""

import argparse
import json
import sqlite3
import sys

sys.dont_write_bytecode = True  # importing track_db must not leave scripts/__pycache__ behind
from track_db import REQUIRED_COLUMNS, bypass_hits, check_schema, open_db, planner_card, planner_turns  # noqa: E402

EXCERPT_CHARS = 300
TASK_ERROR_CHARS = 2000
NO_END = 1 << 62
# Projections of rows the trace already reads (transcript items, phases) and UI or hook state.
# Never add harness.transcript.cleared/rewound: they mark the turns the trace cannot show.
NOISE_EVENT_KINDS = (
    "overlay.set", "overlay.deleted", "harness.item.added", "harness.phase.changed", "claude.hook",
    "plugin.tool.registered",
)
TRANSCRIPT_LOSS_KINDS = ("harness.transcript.cleared", "harness.transcript.rewound")
# Transcript items that are the turn's input or the Planner's own prose, not something it did.
NOT_ACTIONS = ("userMessage", "agentMessage", "reasoning")
BOOKKEEPING = ("id", "type", "status", "error", "durationMs", "tool")


def excerpt(value, cap):
    if value is None:
        return None
    text = value if isinstance(value, str) else json.dumps(value, ensure_ascii=False)
    if cap is None or len(text) <= cap:
        return text
    return text[:cap] + f"…[+{len(text) - cap} chars]"


def capped(value, cap):
    """`value` with every string leaf cut by excerpt(); short leaves survive whole, whatever their depth."""
    if isinstance(value, dict):
        return {k: capped(v, cap) for k, v in value.items()}
    if isinstance(value, list):
        return [capped(v, cap) for v in value]
    return excerpt(value, cap) if isinstance(value, str) else value


def tool_result(result):
    if result is None:
        return None
    if result.get("structuredContent") is not None:
        return result["structuredContent"]
    return "\n".join(c.get("text", "") for c in result.get("content") or [])


def action(rows, cap):
    """One action from an item's transcript rows (item/started, then item/completed if any)."""
    finished = any(method == "item/completed" for method, _, _ in rows)
    item = json.loads(rows[-1][1])["item"]
    item_type = item["type"]
    if item_type == "commandExecution":
        args, result = item.get("command"), item.get("aggregatedOutput")
    elif "arguments" in item:
        args, result = item["arguments"], tool_result(item.get("result"))
    else:
        args, result = {k: v for k, v in item.items() if k not in BOOKKEEPING}, None
    ok = None if not finished else item.get("status") != "failed"
    error = item.get("error")
    if isinstance(error, dict):
        error = error.get("message", error)
    if error is None and ok is False and item.get("exitCode") is not None:
        error = f"exit code {item['exitCode']}"
    return {
        "at": rows[0][2],
        "item_type": item_type,
        "tool": item.get("tool"),
        "finished": finished,
        "ok": ok,
        "duration_ms": item.get("durationMs"),
        "error": excerpt(error, cap),
        "args": capped(args, cap),
        "result": capped(result, cap),
    }


def actions(db, card, turn_id, cap):
    marks = ",".join("?" * len(NOT_ACTIONS))
    items = {}
    for uuid, method, params, at in db.execute(
        "SELECT item_uuid, method, params, created_at_ms FROM harness_items WHERE card_id = ? AND turn_id = ?"
        f" AND method IN ('item/started', 'item/completed') AND item_type NOT IN ({marks}) ORDER BY id",
        (card, turn_id, *NOT_ACTIONS),
    ):
        items.setdefault(uuid, []).append((method, params, at))
    return [action(rows, cap) for rows in items.values()]


def messages(db, card, turn_id, cap):
    return [
        {"at": at, "text": excerpt(json.loads(params)["item"]["text"], cap)}
        for params, at in db.execute(
            "SELECT params, created_at_ms FROM harness_items WHERE card_id = ? AND turn_id = ?"
            " AND item_type = 'agentMessage' AND method = 'item/completed' ORDER BY id",
            (card, turn_id),
        )
    ]


def origin(db, raw):
    if raw is None:
        return None
    row = None
    if raw["event_id"] is not None:
        row = db.execute("SELECT kind, payload FROM events WHERE id = ?", (raw["event_id"],)).fetchone()
    out = {"observation": raw["observation"], "event_id": raw["event_id"], "event_kind": row and row[0]}
    if raw["observation"] == "track_wake":
        out["source"] = row and json.loads(row[1]).get("source")
    return out


def trigger(db, card, turn_id, cap):
    segments, recorded = [], False
    for at, raw in db.execute(
        "SELECT created_at_ms, input_segments FROM harness_items WHERE card_id = ? AND turn_id = ?"
        " AND item_type = 'userMessage' AND method = 'item/completed' ORDER BY id",
        (card, turn_id),
    ):
        if raw is None:
            segments.append({"at": at, "presentation": None, "text": None, "attachment_count": None, "origin": None})
            continue
        recorded = True
        for segment in json.loads(raw):
            segments.append({
                "at": at,
                "presentation": segment["presentation"],
                "text": excerpt(segment["text"], cap),
                "attachment_count": len(segment.get("attachments") or []),
                "origin": origin(db, segment.get("origin")),
            })
    return segments if recorded else None


def events(db, track_id, start, end, cap):
    marks = ",".join("?" * len(NOISE_EVENT_KINDS))
    return [
        {"id": i, "at": at, "kind": kind, "actor": json.loads(actor)["kind"],
         "payload": capped(json.loads(payload), cap)}
        for i, at, kind, actor, payload in db.execute(
            "SELECT id, at, kind, actor, payload FROM events WHERE scope_track = ? AND at >= ? AND at < ?"
            f" AND kind NOT IN ({marks}) ORDER BY id",
            (track_id, start, end, *NOISE_EVENT_KINDS),
        )
    ]


def task_error(db, track_id, task_id, status_detail):
    """The latest task.failed reason for the task, else its status_detail."""
    row = db.execute(
        "SELECT payload FROM events WHERE scope_track = ? AND kind = 'task.failed'"
        " AND json_extract(payload, '$.idempotency_key') = ? ORDER BY id DESC LIMIT 1",
        (track_id, task_id),
    ).fetchone()
    return status_detail if row is None else json.loads(row[0])["reason"]


def tasks(db, track_id, start, end, cap):
    error_cap = None if cap is None else TASK_ERROR_CHARS
    return [
        {"at": at, "key": key, "kind": kind, "status": status,
         "error": excerpt(task_error(db, track_id, task_id, detail), error_cap) if status == "failed" else None,
         "goal": excerpt(goal, cap)}
        for at, task_id, key, kind, status, detail, goal in db.execute(
            "SELECT created_at_ms, id, key, kind, status, status_detail, goal FROM tasks WHERE track_id = ?"
            " AND created_at_ms >= ? AND created_at_ms < ? ORDER BY created_at_ms, key",
            (track_id, start, end),
        )
    ]


def transcript_losses(db, track_id):
    marks = ",".join("?" * len(TRANSCRIPT_LOSS_KINDS))
    return [
        {"id": i, "at": at, "kind": kind, "payload": json.loads(payload)}
        for i, at, kind, payload in db.execute(
            f"SELECT id, at, kind, payload FROM events WHERE scope_track = ? AND kind IN ({marks}) ORDER BY id",
            (track_id, *TRANSCRIPT_LOSS_KINDS),
        )
    ]


def track_trace(db, track_id, since, until, cap):
    """The Track's header record and its turn records."""
    row = db.execute(
        "SELECT title, area_id, created_at, closed_at, creator_track_id FROM tracks WHERE id = ?", (track_id,)
    ).fetchone()
    if row is None:
        sys.exit(f"error: no track {track_id}")
    title, area_id, created_at, closed_at, creator = row
    card = planner_card(db, track_id)
    every = [] if card is None else planner_turns(db, card)
    header = {
        "type": "track", "track": track_id, "title": title, "area_id": area_id, "created_at": created_at,
        "closed_at": closed_at, "creator_track_id": creator, "planner_card": card, "excerpt_chars": cap,
        "transcript_losses": transcript_losses(db, track_id),
        "events_before_first_turn": events(db, track_id, 0, every[0][1] if every else NO_END, cap),
    }
    turns = []
    for i, (turn_id, started, completed, params) in enumerate(every):
        if (since is not None and started < since) or (until is not None and started > until):
            continue
        stop = every[i + 1][1] if i + 1 < len(every) else NO_END
        params = params or {}
        turns.append({
            "type": "turn", "track": track_id, "turn_id": turn_id, "started_at": started, "completed_at": completed,
            "duration_ms": None if completed is None else completed - started,
            "status": params.get("status"), "error": excerpt(params.get("error"), cap),
            "trigger": trigger(db, card, turn_id, cap),
            "messages": messages(db, card, turn_id, cap),
            "actions": actions(db, card, turn_id, cap),
            "tasks": tasks(db, track_id, started, stop, cap),
            "events": events(db, track_id, started, stop, cap),
            "bypass": [{"source": s, "command": c} for s, c in bypass_hits(db, track_id, card, started, stop - 1)],
        })
    return header, turns


def trace(db, track_ids, since=None, until=None, cap=EXCERPT_CHARS):
    headers, turns = [], []
    for track_id in track_ids:
        header, track_turns = track_trace(db, track_id, since, until, cap)
        headers.append(header)
        turns += track_turns
    turns.sort(key=lambda t: (t["started_at"], t["track"], t["turn_id"]))
    return headers + turns


def area_tracks(db, area_id):
    ids = [r[0] for r in db.execute("SELECT id FROM tracks WHERE area_id = ? ORDER BY created_at, id", (area_id,))]
    if not ids:
        sys.exit(f"error: area {area_id} has no tracks")
    return ids


def selftest_db():
    """A database with only the required columns: one Track, its Planner, three turns."""
    db = sqlite3.connect(":memory:")
    for table, columns in REQUIRED_COLUMNS.items():
        db.execute(f"CREATE TABLE {table} ({', '.join(columns)})")
    db.execute("INSERT INTO tracks VALUES ('t', 'area', 'Trace', 100, NULL, 'parent')")
    db.execute("INSERT INTO cards VALUES ('planner', 't', 'planner')")
    event_rows = [
        (1, "harness.transcript.cleared", {"cleared_item_count": 9}, "Kernel", 800),
        (2, "track.wake_requested", {"source": "dev.neige.mail"}, "Kernel", 900),
        (3, "task.completed", {"key": "a"}, "Kernel", 950),
        (4, "task.gate_result", {"log_tail": "z" * 400, "error": "step lint exited 1", "exit_code": 1}, "Kernel", 1500),
        (5, "overlay.set", {}, "Kernel", 1600),
        (6, "task.failed", {"idempotency_key": "t:b", "reason": "worker: " + "r" * 2100}, "AiPlannerSession", 2500),
        (7, "harness.transcript.rewound", {"removed_item_count": 2, "turn_id": "gone"}, "Kernel", 2600),
        (8, "track.updated", {}, "User", 3500),
    ]
    for i, kind, payload, actor, at in event_rows:
        db.execute("INSERT INTO events VALUES (?, ?, ?, ?, ?, 't', NULL)",
                   (i, kind, json.dumps(payload), json.dumps({"kind": actor}), at))
    db.execute("INSERT INTO tasks VALUES ('t', 'fix', 'codex', ?, 'running', 1050, 't:fix', NULL)", ("x" * 400,))
    db.execute("INSERT INTO tasks VALUES ('t', 'gate', 'codex', 'g', 'failed', 1060, 't:gate', 'gate-red')")
    db.execute("INSERT INTO tasks VALUES ('t', 'b', 'codex', 'b', 'failed', 2000, 't:b', 'worker-reported: w')")
    segments_1 = [
        {"presentation": "system", "text": "Task a completed.", "attachments": [],
         "origin": {"observation": "task_completed", "event_id": 3}},
        {"presentation": "system", "text": "Mail arrived.", "attachments": [],
         "origin": {"observation": "track_wake", "event_id": 2}},
    ]
    segments_2 = [{"presentation": "user", "text": "User says: go", "attachments": [{"path": "a.png"}]}]
    failed = {"type": "mcpToolCall", "tool": "neige_task_declare", "status": "failed",
              "arguments": {"key": "fix", "goal": "g" * 400},
              "error": {"message": "tool call failed: DB locked"}, "result": None, "durationMs": 7}
    shell = {"type": "commandExecution", "command": "git push origin x", "status": "completed",
             "aggregatedOutput": "y" * 400, "exitCode": 0, "durationMs": 3}
    hung = {"type": "commandExecution", "command": "sleep 99", "status": "inProgress"}
    user = {"item": {"type": "userMessage"}}
    said = {"type": "agentMessage", "phase": "commentary", "text": "Declaring fix first."}
    rows = [
        ("turn-1", "u1", "userMessage", "item/completed", user, 1000, segments_1),
        ("turn-1", "m1", "mcpToolCall", "item/started", {"item": dict(failed, status="inProgress")}, 1005, None),
        ("turn-1", "m1", "mcpToolCall", "item/completed", {"item": failed}, 1010, None),
        ("turn-1", "r1", "reasoning", "item/completed", {"item": {"type": "reasoning"}}, 1015, None),
        ("turn-1", "a1", "agentMessage", "item/started", {"item": dict(said, text="")}, 1016, None),
        ("turn-1", "a1", "agentMessage", "item/completed", {"item": said}, 1018, None),
        ("turn-1", "c1", "commandExecution", "item/completed", {"item": shell}, 1020, None),
        # Two turn/completed rows: the latest one counts, whatever its params sort as.
        ("turn-1", None, None, "turn/completed", {"status": "interrupted", "error": None}, 1090, None),
        ("turn-1", None, None, "turn/completed", {"status": "completed", "error": None}, 1100, None),
        ("turn-2", "u2", "userMessage", "item/completed", user, 2000, segments_2),
        ("turn-2", "c2", "commandExecution", "item/started", {"item": hung}, 2050, None),
        ("turn-2", "u2b", "userMessage", "item/completed", user, 2100, None),
        ("turn-3", "u3", "userMessage", "item/completed", user, 3000, None),
        ("turn-3", None, None, "turn/completed", {"status": "completed", "error": None}, 3200, None),
    ]
    for n, (turn, uuid, item_type, method, params, at, segs) in enumerate(rows, 1):
        db.execute("INSERT INTO harness_items VALUES (?, 'planner', ?, ?, ?, ?, ?, ?, ?)",
                   (n, turn, uuid, item_type, method, json.dumps(params), at, segs and json.dumps(segs)))
    return db


def selftest():
    db = selftest_db()
    check_schema(db)
    records = trace(db, ["t"])
    failures = []

    def expect(name, got, want):
        if got != want:
            failures.append(f"{name}: expected {want!r}, got {got!r}")

    expect("record types", [r["type"] for r in records], ["track", "turn", "turn", "turn"])
    header, one, two, three = (records + [{}, {}, {}, {}])[:4]
    expect("header", {k: header.get(k) for k in ("track", "area_id", "creator_track_id", "planner_card")},
           {"track": "t", "area_id": "area", "creator_track_id": "parent", "planner_card": "planner"})
    expect("transcript losses", [(e["id"], e["kind"], e["payload"]) for e in header.get("transcript_losses", [])], [
        (1, "harness.transcript.cleared", {"cleared_item_count": 9}),
        (7, "harness.transcript.rewound", {"removed_item_count": 2, "turn_id": "gone"}),
    ])
    expect("events before the first turn", [e["id"] for e in header.get("events_before_first_turn", [])], [1, 2, 3])
    expect("turn ids", [r.get("turn_id") for r in (one, two, three)], ["turn-1", "turn-2", "turn-3"])
    expect("turn 1 times (latest turn/completed)",
           [one.get(k) for k in ("started_at", "completed_at", "duration_ms", "status")], [1000, 1100, 100, "completed"])
    expect("turn 2 still running", [two.get(k) for k in ("completed_at", "duration_ms", "status")], [None, None, None])
    expect("turn 1 trigger", [(s["presentation"], s["origin"]) for s in one.get("trigger") or []], [
        ("system", {"observation": "task_completed", "event_id": 3, "event_kind": "task.completed"}),
        ("system", {"observation": "track_wake", "event_id": 2, "event_kind": "track.wake_requested",
                    "source": "dev.neige.mail"}),
    ])
    expect("turn 2 trigger (pre-T1 row, then a row without segments)", two.get("trigger"), [
        {"at": 2000, "presentation": "user", "text": "User says: go", "attachment_count": 1, "origin": None},
        {"at": 2100, "presentation": None, "text": None, "attachment_count": None, "origin": None},
    ])
    expect("turn 3 trigger (no segments recorded)", three.get("trigger"), None)
    acts = one.get("actions") or []
    expect("turn 1 actions", [(a["item_type"], a["tool"], a["finished"], a["ok"], a["error"]) for a in acts], [
        ("mcpToolCall", "neige_task_declare", True, False, "tool call failed: DB locked"),
        ("commandExecution", None, True, True, None),
    ])
    expect("action args (a tool call's per leaf)", [a["args"] for a in acts],
           [{"key": "fix", "goal": "g" * 300 + "…[+100 chars]"}, "git push origin x"])
    expect("turn 1 messages (not actions)", one.get("messages"), [{"at": 1018, "text": "Declaring fix first."}])
    expect("turn 2 messages", two.get("messages"), [])
    expect("result excerpt", acts[1]["result"] if len(acts) > 1 else None, "y" * 300 + "…[+100 chars]")
    expect("turn 2 actions (unfinished)",
           [(a["at"], a["args"], a["finished"], a["ok"]) for a in two.get("actions", [])], [(2050, "sleep 99", False, None)])
    expect("turn 1 tasks", [(t["key"], t["status"], t.get("error"), t["goal"]) for t in one.get("tasks", [])], [
        ("fix", "running", None, "x" * 300 + "…[+100 chars]"),
        ("gate", "failed", "gate-red", "g"),
    ])
    expect("turn 2 task error (latest task.failed reason)", [(t["key"], t.get("error")) for t in two.get("tasks", [])],
           [("b", "worker: " + "r" * 1992 + "…[+108 chars]")])
    expect("turn 1 events (noise left out)", [(e["id"], e["actor"]) for e in one.get("events", [])], [(4, "Kernel")])
    expect("event payload capped per leaf", [e["payload"] for e in one.get("events", [])],
           [{"log_tail": "z" * 300 + "…[+100 chars]", "error": "step lint exited 1", "exit_code": 1}])
    expect("turn 2 events (losses kept)", [e["id"] for e in two.get("events", [])], [6, 7])
    expect("turn 3 events", [e["id"] for e in three.get("events", [])], [8])
    expect("turn 1 bypass", one.get("bypass"), [{"source": "planner shell", "command": "git push origin x"}])
    expect("turn 2 bypass", two.get("bypass"), [])
    expect("--since picks turns 2 and 3", [r.get("turn_id") for r in trace(db, ["t"], since=1500)],
           [None, "turn-2", "turn-3"])
    expect("--until picks turn 1 and changes no record", trace(db, ["t"], until=1200), [header, one])
    full = trace(db, ["t"], cap=None)
    expect("--full keeps the whole text", [full[1]["tasks"][0]["goal"], full[2]["tasks"][0].get("error")],
           ["x" * 400, "worker: " + "r" * 2100])
    for failure in failures:
        print("FAIL " + failure, file=sys.stderr)
    if failures:
        sys.exit(1)
    print("OK: the trace records of the three-turn fixture match")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--selftest", action="store_true", help="check the records of a built-in fixture and exit")
    parser.add_argument("--db", help="path to calm.db (opened read-only)")
    parser.add_argument("--since", type=int, metavar="MS", help="first turn start to print (epoch ms)")
    parser.add_argument("--until", type=int, metavar="MS", help="last turn start to print (epoch ms)")
    parser.add_argument("--full", action="store_true", help="print texts whole instead of excerpts")
    parser.add_argument("--area", metavar="AREA_ID", help="trace every Track of this area")
    parser.add_argument("tracks", nargs="*", metavar="TRACK_ID")
    args = parser.parse_args()
    if args.selftest:
        selftest()
        return
    if args.db is None or bool(args.area) == bool(args.tracks):
        parser.error("--db and either --area or at least one TRACK_ID are required")

    db = open_db(args.db)
    track_ids = area_tracks(db, args.area) if args.area else args.tracks
    for record in trace(db, track_ids, args.since, args.until, None if args.full else EXCERPT_CHARS):
        print(json.dumps(record, ensure_ascii=False))


if __name__ == "__main__":
    main()
